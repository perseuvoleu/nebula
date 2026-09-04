//! Git worktree operations — shelled out to the `git` CLI on purpose:
//! libgit2's worktree support lags git's, these are rare user-initiated ops,
//! and git's stderr is the best error message we could show.

use anyhow::{anyhow, bail, Result};
use std::path::{Path, PathBuf};
use tokio::process::Command;

const PACKAGE_MANAGER_LOCKFILES: [&str; 4] = [
    "pnpm-lock.yaml",
    "package-lock.json",
    "yarn.lock",
    "bun.lockb",
];

/// Shown when the `git` binary itself is missing. Every other git failure
/// carries git's own stderr; this one git never gets to print, so spelling out
/// the fix is on us — otherwise the user sees "No such file or directory" and
/// blames the directory they just picked. Kept to one line: the TUI shows it
/// in the footer flash, which truncates.
pub const GIT_MISSING: &str =
    "git was not found on your PATH — nebula needs it. Install git (https://git-scm.com/downloads), then restart nebula.";

/// True when `err` came from `git` being absent, so callers can pass the
/// message through instead of layering their own (wrong) explanation on top.
pub fn is_missing(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.to_string() == GIT_MISSING)
}

/// `git` never even started. NotFound means the binary isn't installed — the
/// one git failure with no stderr to quote, so the explanation has to be ours.
fn spawn_err(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!(GIT_MISSING)
    } else {
        anyhow::Error::new(e).context("run git")
    }
}

/// `git -C repo args…` where the repo lives: locally, or over `ssh host`
/// for a remote project (`nebula_core::remote` knows which paths are
/// which). A remote hop that fails to connect surfaces ssh's own stderr,
/// which names the host — the right explanation there.
async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let (program, argv) = nebula_core::remote::git_command(repo, args);
    let output = Command::new(&program)
        .args(&argv)
        .output()
        .await
        .map_err(|e| {
            if program == "ssh" {
                anyhow::Error::new(e).context("run ssh")
            } else {
                spawn_err(e)
            }
        })?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A plain command on the host owning `repo` (remote projects only).
async fn ssh_run(repo: &Path, words: &[&str]) -> Result<()> {
    let host = nebula_core::remote::host_for(repo)
        .ok_or_else(|| anyhow!("{} is not a remote checkout", repo.display()))?;
    let mut argv: Vec<String> = nebula_core::remote::SSH_BATCH_OPTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend([
        "--".to_string(),
        host,
        nebula_core::remote::join_quoted(words.iter().copied()),
    ]);
    let output = Command::new("ssh").args(&argv).output().await?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

/// `$HOME` on `host`, for expanding a `~/…` spelling in `nebula add
/// host:~/repo` — only the remote shell knows it.
pub async fn remote_home(host: &str) -> Result<PathBuf> {
    let mut argv: Vec<String> = nebula_core::remote::SSH_BATCH_OPTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend([
        "--".to_string(),
        host.to_string(),
        "printf %s \"$HOME\"".into(),
    ]);
    let output = Command::new("ssh").args(&argv).output().await?;
    if !output.status.success() {
        bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let home = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if home.is_empty() {
        bail!("could not resolve $HOME on {host}");
    }
    Ok(PathBuf::from(home))
}

/// `git init` an existing directory.
pub async fn init(path: &Path) -> Result<()> {
    git(path, &["init"]).await?;
    Ok(())
}

/// Verify `path` is inside a git repo and return its toplevel.
pub async fn repo_toplevel(path: &Path) -> Result<PathBuf> {
    let out = git(path, &["rev-parse", "--show-toplevel"]).await?;
    Ok(PathBuf::from(out.trim()))
}

pub async fn current_branch(repo: &Path) -> Result<String> {
    let out = git(repo, &["branch", "--show-current"]).await?;
    let branch = out.trim();
    if branch.is_empty() {
        // Detached HEAD — fall back to the short hash.
        let hash = git(repo, &["rev-parse", "--short", "HEAD"]).await?;
        return Ok(format!("detached@{}", hash.trim()));
    }
    Ok(branch.to_string())
}

/// Whether `branch` already exists as a local branch head.
pub async fn branch_exists(repo: &Path, branch: &str) -> bool {
    let head = format!("refs/heads/{branch}");
    git(repo, &["show-ref", "--verify", "--quiet", &head])
        .await
        .is_ok()
}

/// Bring `branch` here from `origin` as a tracking branch — the
/// checkout-on-another-host case, where the branch was pushed from the
/// laptop but this clone has never fetched it. Fails when origin has no
/// such branch (nothing was pushed yet).
pub async fn fetch_branch(repo: &Path, branch: &str) -> Result<()> {
    git(repo, &["fetch", "--quiet", "origin", branch]).await?;
    let upstream = format!("origin/{branch}");
    git(repo, &["branch", "--track", branch, &upstream]).await?;
    Ok(())
}

/// The base a branch's oldest reflog entry names. Git writes
/// `branch: Created from <base>` at creation; an explicit base records the
/// branch name, while creation from an implicit HEAD records the literal
/// `HEAD` — useless for lineage, so it maps to None, like an expired
/// reflog does.
pub async fn branch_creation_base(repo: &Path, branch: &str) -> Option<String> {
    let out = git(repo, &["reflog", "show", "--format=%gs", branch])
        .await
        .ok()?;
    let base = out
        .lines()
        .last()?
        .strip_prefix("branch: Created from ")?
        .trim();
    (!base.is_empty() && base != "HEAD").then(|| base.to_owned())
}

/// The commit a branch was created at, only when its oldest reflog entry is
/// the implicit-HEAD form (`branch: Created from HEAD`) that names no base.
/// The worktree sync uses it to tie an in-place `checkout -b` back to the
/// branch the checkout sat on: the creation sha matching that branch's tip
/// proves the lineage the reflog didn't record.
pub async fn branch_creation_sha_from_head(repo: &Path, branch: &str) -> Option<String> {
    let out = git(repo, &["reflog", "show", "--format=%H %gs", branch])
        .await
        .ok()?;
    let (sha, subject) = out.lines().last()?.split_once(' ')?;
    (subject.trim() == "branch: Created from HEAD").then(|| sha.to_owned())
}

/// Tip commit of a local branch.
pub async fn branch_tip(repo: &Path, branch: &str) -> Option<String> {
    let head = format!("refs/heads/{branch}");
    git(repo, &["rev-parse", &head])
        .await
        .ok()
        .map(|s| s.trim().to_owned())
}

/// `git checkout <branch>` inside a checkout. `-` (git's "previous
/// branch") works too — the deleting-a-branch-row flow uses it when no
/// recorded base survives.
pub async fn checkout(worktree: &Path, branch: &str) -> Result<()> {
    git(worktree, &["checkout", branch]).await?;
    Ok(())
}

/// `git checkout -f <branch>`: the user force-confirmed the revert, so
/// uncommitted changes that would block the plain checkout are discarded.
pub async fn checkout_forced(worktree: &Path, branch: &str) -> Result<()> {
    git(worktree, &["checkout", "-f", branch]).await?;
    Ok(())
}

/// Delete a local branch: `-d` first, and when git refuses because the tip
/// isn't fully merged, retry with `-D`. The force isn't silent — this only
/// runs for deletes the user confirmed behind a dialog that warns commits
/// may be lost.
pub async fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    match git(repo, &["branch", "-d", branch]).await {
        Err(e) if e.to_string().contains("not fully merged") => {
            git(repo, &["branch", "-D", branch]).await?;
            Ok(())
        }
        Err(e) => Err(e),
        Ok(_) => Ok(()),
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub branch: String,
}

/// Parse `git worktree list --porcelain`. The first entry is the main
/// checkout.
pub async fn list_worktrees(repo: &Path) -> Result<Vec<WorktreeEntry>> {
    let out = git(repo, &["worktree", "list", "--porcelain"]).await?;
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    let mut head: Option<String> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(done_path) = path.take() {
                entries.push(WorktreeEntry {
                    path: done_path,
                    branch: branch
                        .take()
                        .unwrap_or_else(|| detached_label(head.as_deref())),
                });
            }
            head = None;
            path = Some(PathBuf::from(p));
        } else if let Some(sha) = line.strip_prefix("HEAD ") {
            head = Some(sha.to_string());
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.trim_start_matches("refs/heads/").to_string());
        }
    }
    if let Some(done_path) = path {
        entries.push(WorktreeEntry {
            path: done_path,
            branch: branch.unwrap_or_else(|| detached_label(head.as_deref())),
        });
    }
    Ok(entries)
}

/// Display name for a checkout with no branch (detached HEAD).
fn detached_label(head: Option<&str>) -> String {
    match head {
        Some(sha) => format!("detached @ {}", &sha[..sha.len().min(7)]),
        None => "(detached)".into(),
    }
}

/// Directory a new worktree for `branch` should live in — the shared
/// naming rule lives in `nebula_core::paths` so the TUI can apply it too.
pub fn worktree_dir(repo: &Path, branch: &str) -> PathBuf {
    nebula_core::paths::worktree_dir(repo, branch)
}

/// `git worktree add <path> -b <branch> [base]`. Falls back to checking out an
/// existing branch when `-b` fails because it already exists.
pub async fn add_worktree(repo: &Path, branch: &str, base: Option<&str>) -> Result<PathBuf> {
    let path = worktree_dir(repo, branch);
    let remote = nebula_core::remote::is_remote(repo);
    if !remote && path.exists() {
        bail!("worktree path already exists: {}", path.display());
    }
    if let Some(parent) = path.parent() {
        if remote {
            // The parent dir is on the far side; `git worktree add` creates
            // the leaf but not missing ancestors, so make them there.
            let parent = parent.to_string_lossy().into_owned();
            ssh_run(repo, &["mkdir", "-p", &parent]).await?;
        } else {
            std::fs::create_dir_all(parent)?;
        }
    }
    let path_str = path.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "add", &path_str, "-b", branch];
    if let Some(base) = base {
        args.push(base);
    }
    match git(repo, &args).await {
        Ok(_) => Ok(path),
        Err(e) if e.to_string().contains("already exists") => {
            // Branch exists: check it out instead of creating.
            git(repo, &["worktree", "add", &path_str, branch]).await?;
            Ok(path)
        }
        Err(e) => Err(e),
    }
}

/// Best-effort clone of a primary checkout's top-level `node_modules` into a
/// new worktree when their first package-manager lockfile matches exactly.
/// The blocking filesystem work runs detached so worktree creation can reply
/// immediately; failures are logged and any partial destination is removed.
pub fn seed_node_modules_in_background(primary: &Path, worktree: &Path) {
    let primary = primary.to_path_buf();
    let worktree = worktree.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut lockfile = None;
        for name in PACKAGE_MANAGER_LOCKFILES {
            let path = primary.join(name);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    lockfile = Some(name);
                    break;
                }
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        path = %path.display(),
                        "node_modules seed skipped: lockfile lookup failed"
                    );
                    return;
                }
            }
        }
        let Some(lockfile) = lockfile else {
            return;
        };
        let primary_lockfile = primary.join(lockfile);
        let worktree_lockfile = worktree.join(lockfile);
        let primary_bytes = match std::fs::read(&primary_lockfile) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %primary_lockfile.display(),
                    "node_modules seed skipped: lockfile read failed"
                );
                return;
            }
        };
        let worktree_bytes = match std::fs::read(&worktree_lockfile) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %worktree_lockfile.display(),
                    "node_modules seed skipped: lockfile read failed"
                );
                return;
            }
        };
        if primary_bytes != worktree_bytes {
            return;
        }

        let source = primary.join("node_modules");
        let destination = worktree.join("node_modules");
        match std::fs::metadata(&source) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %source.display(),
                    "node_modules seed skipped: source lookup failed"
                );
                return;
            }
        }
        match destination.try_exists() {
            Ok(false) => {}
            Ok(true) => return,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %destination.display(),
                    "node_modules seed skipped: destination lookup failed"
                );
                return;
            }
        }

        let output = std::process::Command::new("cp")
            .arg("-Rc")
            .arg(&source)
            .arg(&destination)
            .output();
        match output {
            Ok(output) if output.status.success() => tracing::info!(
                source = %source.display(),
                destination = %destination.display(),
                "seeded worktree node_modules"
            ),
            Ok(output) => {
                let cleanup_error = cleanup_partial_node_modules(&destination);
                tracing::warn!(
                    status = %output.status,
                    stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                    source = %source.display(),
                    destination = %destination.display(),
                    cleanup_error,
                    "node_modules seed skipped: copy-on-write clone failed"
                );
            }
            Err(error) => {
                let cleanup_error = cleanup_partial_node_modules(&destination);
                tracing::warn!(
                    error = %error,
                    source = %source.display(),
                    destination = %destination.display(),
                    cleanup_error,
                    "node_modules seed skipped: could not run cp"
                );
            }
        }
    });
}

fn cleanup_partial_node_modules(path: &Path) -> Option<String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(error.to_string()),
    }
}

pub async fn remove_worktree(repo: &Path, worktree_path: &Path, force: bool) -> Result<()> {
    // Checkout already gone (manual rm -rf): `git worktree remove` would fail,
    // but the user's intent is already satisfied — just drop git's stale
    // bookkeeping so the entry leaves `git worktree list`. (A remote
    // checkout can't be stat'ed from here; git's own "does not exist"
    // below covers it.)
    if !nebula_core::remote::is_remote(repo) && !worktree_path.exists() {
        let _ = git(repo, &["worktree", "prune"]).await;
        return Ok(());
    }
    let path_str = worktree_path.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_str);
    match git(repo, &args).await {
        Ok(_) => Ok(()),
        // Directory exists but git no longer tracks it as a worktree (already
        // pruned, or its .git link was destroyed). Nothing for git to remove;
        // prune any leftover metadata and let the caller drop its row. The
        // directory itself is left alone — deleting an untracked dir is not
        // ours to do.
        Err(e)
            if e.to_string().contains("is not a working tree")
                || e.to_string().contains("does not exist") =>
        {
            let _ = git(repo, &["worktree", "prune"]).await;
            Ok(())
        }
        // Locked by a session that ran `git worktree lock` (Claude Code locks
        // its worktree and a killed session never unlocks). The caller has
        // already killed this worktree's sessions, so the lock is stale —
        // unlock and retry rather than surfacing git's refusal.
        Err(e) if e.to_string().contains("locked working tree") => {
            git(repo, &["worktree", "unlock", &path_str]).await?;
            git(repo, &args).await?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn init_repo(dir: &Path) {
        git(dir, &["init", "-b", "main"]).await.unwrap();
        git(dir, &["config", "user.email", "t@t"]).await.unwrap();
        git(dir, &["config", "user.name", "t"]).await.unwrap();
        git(dir, &["commit", "--allow-empty", "-m", "init"])
            .await
            .unwrap();
    }

    #[test]
    fn missing_git_binary_explains_the_install() {
        let err = spawn_err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "No such file or directory (os error 2)",
        ));
        assert!(is_missing(&err), "{err:#}");
        assert!(err.to_string().contains("Install git"));
        // Still recognized once a caller layers its own context on top.
        assert!(is_missing(&err.context("open /some/dir")));
    }

    #[test]
    fn other_spawn_failures_are_not_reported_as_missing_git() {
        let err = spawn_err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Permission denied (os error 13)",
        ));
        assert!(!is_missing(&err), "{err:#}");
    }

    #[tokio::test]
    async fn git_errors_are_not_reported_as_missing_git() {
        let tmp = tempfile::tempdir().unwrap();
        // A real git that says "not a repository" must keep saying so.
        let err = repo_toplevel(tmp.path()).await.unwrap_err();
        assert!(!is_missing(&err), "{err:#}");
    }

    #[tokio::test]
    async fn remove_worktree_survives_manual_rm_rf() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo).await;
        let wt = add_worktree(&repo, "feature", None).await.unwrap();

        // Simulate the user deleting the checkout by hand.
        std::fs::remove_dir_all(&wt).unwrap();

        remove_worktree(&repo, &wt, false).await.unwrap();
        // The stale registration should be pruned from git's list too.
        let entries = list_worktrees(&repo).await.unwrap();
        assert!(entries.iter().all(|e| e.path != wt));
    }

    #[tokio::test]
    async fn remove_worktree_ok_when_already_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo).await;
        let wt = add_worktree(&repo, "feature", None).await.unwrap();
        std::fs::remove_dir_all(&wt).unwrap();
        git(&repo, &["worktree", "prune"]).await.unwrap();

        // Path gone AND git no longer knows it — still not an error.
        remove_worktree(&repo, &wt, false).await.unwrap();
    }

    #[tokio::test]
    async fn remove_worktree_unlocks_session_locked_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo).await;
        let wt = add_worktree(&repo, "feature", None).await.unwrap();
        let wt_str = wt.to_string_lossy().into_owned();
        git(
            &repo,
            &[
                "worktree",
                "lock",
                "--reason",
                "claude session menu-enable-level",
                &wt_str,
            ],
        )
        .await
        .unwrap();

        remove_worktree(&repo, &wt, false).await.unwrap();
        let entries = list_worktrees(&repo).await.unwrap();
        assert!(entries.iter().all(|e| e.path != wt));
    }

    #[tokio::test]
    async fn remove_worktree_still_fails_on_dirty_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo).await;
        let wt = add_worktree(&repo, "feature", None).await.unwrap();
        std::fs::write(wt.join("untracked.txt"), "dirty").unwrap();

        assert!(remove_worktree(&repo, &wt, false).await.is_err());
        remove_worktree(&repo, &wt, true).await.unwrap();
    }
}
