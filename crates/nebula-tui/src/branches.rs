//! Local-branch listing for branch pickers — shelled
//! out to the `git` CLI like the daemon's worktree ops, newest commit first
//! so the branch just worked on is the one under the cursor.

use std::path::Path;

/// One local branch plus the base it was created from, for the panel's
/// lineage-nested branch rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBranch {
    pub name: String,
    /// Branch this one was created from, read from the oldest reflog entry
    /// (`branch: Created from <base>`). None when the reflog is gone
    /// (expired, a fresh clone) or names no branch (a detached `HEAD`).
    pub created_from: Option<String>,
    /// Epoch secs of the oldest reflog entry — the branch's creation while
    /// the reflog reaches back that far. The panel's fixed row order sorts
    /// on it. None when the reflog is gone.
    pub created_at: Option<i64>,
}

/// A branch with no known base — how most test fixtures and fallbacks
/// start out.
impl From<&str> for LocalBranch {
    fn from(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            created_from: None,
            created_at: None,
        }
    }
}

/// `git -C repo args…` wherever the checkout lives — over ssh for a remote
/// project (`nebula_core::remote` keeps the path→host map).
fn git(repo: &Path, args: &[&str]) -> std::process::Command {
    let (program, argv) = nebula_core::remote::git_command(repo, args);
    let mut cmd = std::process::Command::new(program);
    cmd.args(argv);
    cmd
}

/// Local branches of `repo`, ordered newest-committed first. Empty when the
/// path isn't a git repo (or git is missing) — callers fall back to the
/// branches nebula already knows from the project's worktrees.
pub fn local_branches(repo: &Path) -> Vec<String> {
    git(
        repo,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "refs/heads/",
            "--format=%(refname)",
        ],
    )
    .output()
    .ok()
    .filter(|o| o.status.success())
    .map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|refname| refname.strip_prefix("refs/heads/"))
            .map(str::to_owned)
            .collect()
    })
    .unwrap_or_default()
}

/// `local_branches` plus each branch's creation base and time — one extra
/// `git reflog` read per branch, so this runs off-loop like the listing
/// itself.
pub fn local_branches_with_bases(repo: &Path) -> Vec<LocalBranch> {
    local_branches(repo)
        .into_iter()
        .map(|name| {
            let (created_from, created_at) = branch_creation(repo, &name);
            LocalBranch {
                name,
                created_from,
                created_at,
            }
        })
        .collect()
}

/// The base and time of a branch's oldest reflog entry. Git writes
/// `branch: Created from <base>` at creation; an explicit base (`git
/// branch x y`, `git checkout -b x y`, nebula's own base picker) records
/// the branch name, while creation from an implicit HEAD records the
/// literal `HEAD` — useless for lineage, so it maps to None.
fn branch_creation(repo: &Path, branch: &str) -> (Option<String>, Option<i64>) {
    let Some(out) = git(
        repo,
        &[
            "reflog",
            "show",
            "--date=unix",
            "--format=%gd%x09%gs",
            branch,
        ],
    )
    .output()
    .ok()
    .filter(|o| o.status.success()) else {
        return (None, None);
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some((selector, subject)) = stdout.lines().last().and_then(|l| l.split_once('\t')) else {
        return (None, None);
    };
    // `<branch>@{<unix secs>}`
    let created_at = selector
        .rsplit_once("@{")
        .and_then(|(_, t)| t.strip_suffix('}')?.parse().ok());
    let base = subject
        .strip_prefix("branch: Created from ")
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "HEAD")
        .map(str::to_owned);
    (base, created_at)
}

/// Delete a local branch: `-d` first, and when git refuses because the tip
/// isn't fully merged, retry with `-D`. The force isn't silent — every
/// caller sits behind a confirm dialog that warns commits may be lost.
pub fn delete_branch(repo: &Path, branch: &str) -> Result<(), String> {
    match branch_op(repo, &["-d", branch]) {
        Err(e) if e.contains("not fully merged") => branch_op(repo, &["-D", branch]),
        r => r,
    }
}

/// Run a `git branch …` mutation (create/delete) in `repo`. Err carries
/// git's own stderr — "not fully merged", "already exists" — so the toast
/// says exactly why the branch didn't change.
pub fn branch_op(repo: &Path, args: &[&str]) -> Result<(), String> {
    let mut argv = vec!["branch"];
    argv.extend(args);
    let out = git(repo, &argv).output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .next()
            .unwrap_or("git branch failed")
            .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn branches_come_back_newest_committed_first() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["commit", "--allow-empty", "-m", "init"]);
        // Distinct committer dates make the order deterministic without
        // sleeping between commits.
        run_git(&repo, &["branch", "older"]);
        run_git(&repo, &["checkout", "-b", "newer"]);
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .env("GIT_COMMITTER_DATE", "2030-01-01T00:00:00")
            .args(["commit", "--allow-empty", "-m", "newest"])
            .output()
            .unwrap();
        run_git(&repo, &["checkout", "main"]);

        let branches = local_branches(&repo);
        assert_eq!(branches.first().map(String::as_str), Some("newer"));
        assert!(branches.contains(&"main".to_string()));
        assert!(branches.contains(&"older".to_string()));
    }

    /// The reflog remembers what a branch was created from — explicit
    /// bases come back as lineage, an implicit HEAD creation stays flat.
    #[test]
    fn creation_bases_come_from_the_reflog() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["commit", "--allow-empty", "-m", "init"]);
        run_git(&repo, &["branch", "feat", "main"]);
        run_git(&repo, &["branch", "sub", "feat"]);
        run_git(&repo, &["checkout", "-b", "implicit"]);
        run_git(&repo, &["checkout", "main"]);

        let base = |name: &str| {
            local_branches_with_bases(&repo)
                .into_iter()
                .find(|b| b.name == name)
                .unwrap()
                .created_from
        };
        assert_eq!(base("feat").as_deref(), Some("main"));
        assert_eq!(base("sub").as_deref(), Some("feat"));
        assert_eq!(
            base("implicit"),
            None,
            "a literal 'Created from HEAD' names no branch"
        );
    }

    /// Creation time is the oldest reflog entry's, so later commits on a
    /// branch never move it.
    #[test]
    fn creation_time_is_the_oldest_reflog_entry() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-b", "main"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        run_git(&repo, &["commit", "--allow-empty", "-m", "init"]);
        let git_at = |when: &str, args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .env("GIT_COMMITTER_DATE", when)
                .args(args)
                .output()
                .unwrap();
        };
        git_at("2030-01-01T00:00:00Z", &["branch", "feat", "main"]);
        git_at("2031-01-01T00:00:00Z", &["checkout", "feat"]);
        git_at(
            "2031-01-01T00:00:00Z",
            &["commit", "--allow-empty", "-m", "later"],
        );

        let feat = local_branches_with_bases(&repo)
            .into_iter()
            .find(|b| b.name == "feat")
            .unwrap();
        assert_eq!(
            feat.created_at,
            Some(1_893_456_000),
            "2030-01-01, not the later commit"
        );
    }

    #[test]
    fn a_non_repo_path_yields_no_branches() {
        let dir = tempfile::tempdir().unwrap();
        assert!(local_branches(dir.path()).is_empty());
    }
}
