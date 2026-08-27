//! Local-branch listing for the orchestrator flow's branch picker — shelled
//! out to the `git` CLI like the daemon's worktree ops, newest commit first
//! so the branch just worked on is the one under the cursor.

use std::path::Path;

/// Local branches of `repo`, ordered newest-committed first. Empty when the
/// path isn't a git repo (or git is missing) — callers fall back to the
/// branches nebula already knows from the project's worktrees.
pub fn local_branches(repo: &Path) -> Vec<String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "for-each-ref",
            "--sort=-committerdate",
            "refs/heads/",
            "--format=%(refname:short)",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Run a `git branch …` mutation (create/delete) in `repo`. Err carries
/// git's own stderr — "not fully merged", "already exists" — so the toast
/// says exactly why the branch didn't change.
pub fn branch_op(repo: &Path, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("branch")
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().next().unwrap_or("git branch failed").to_string())
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

    #[test]
    fn a_non_repo_path_yields_no_branches() {
        let dir = tempfile::tempdir().unwrap();
        assert!(local_branches(dir.path()).is_empty());
    }
}
