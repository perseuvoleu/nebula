//! The review queue of the git-diff viewer: the selected project's local
//! checkouts that wait for review, built in one pass by
//! `ReviewQueue::build`.

use crate::git_diff::{self, DiffBase, DiffFile};
use crate::review::{self, FileMarks};
use nebula_core::{AgentStatus, Worktree, WorktreeId};
use std::path::Path;

/// What the agents on a checkout are doing, as a reviewer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsState {
    /// At least one is running or waiting on an answer.
    MidTurn,
    /// There are agents and every one ended its turn.
    Finished,
    /// No agent sits on the checkout.
    None,
}

impl AgentsState {
    /// The state of a checkout whose unarchived agents have `statuses`.
    pub fn of(statuses: impl IntoIterator<Item = AgentStatus>) -> Self {
        let mut state = AgentsState::None;
        for status in statuses {
            if matches!(status, AgentStatus::Running | AgentStatus::NeedsFeedback) {
                return AgentsState::MidTurn;
            }
            state = AgentsState::Finished;
        }
        state
    }

    /// The wording of the review list's agent column.
    pub fn label(self) -> &'static str {
        match self {
            AgentsState::MidTurn => "agent running",
            AgentsState::Finished => "agent finished",
            AgentsState::None => "no agent",
        }
    }
}

/// What a checkout holds for review, measured on the diff the viewer opens
/// it on (`git_diff::opening_diff`).
#[derive(Debug, Clone, PartialEq)]
pub struct Changes {
    /// What the counts compare: the working tree or the review base.
    pub base: DiffBase,
    /// Files in the diff, at least one.
    pub files: usize,
    /// Lines added across `files`.
    pub added: usize,
    /// Lines removed across `files`.
    pub removed: usize,
    /// How many of `files` carry a ✓ that still applies.
    pub reviewed: usize,
}

impl Changes {
    /// The counts of `files`, the list `base` produced, `reviewed` of
    /// which carry a ✓.
    pub fn measure(
        root: &Path,
        base: DiffBase,
        files: &[DiffFile],
        head_ok: bool,
        reviewed: usize,
    ) -> Self {
        let source = git_diff::Source::Checkout(root.to_path_buf());
        let (added, removed) = git_diff::line_counts(&source, &base, files, head_ok);
        Self {
            base,
            files: files.len(),
            added,
            removed,
            reviewed,
        }
    }

    /// `3 files`, or `1 file`.
    pub fn files_label(&self) -> String {
        match self.files {
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        }
    }

    /// What the counts compare, for display: `vs main`, or `uncommitted`
    /// for the working tree.
    pub fn comparison(&self) -> String {
        match &self.base {
            DiffBase::WorkingTree => "uncommitted".to_string(),
            base => base.title_suffix().trim_start().to_string(),
        }
    }
}

/// One checkout that waits for review, or that git could not inspect.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewEntry {
    /// The checkout's row in the Worktrees panel.
    pub worktree: WorktreeId,
    /// The branch the checkout is on.
    pub branch: String,
    /// What its agents are doing.
    pub agents: AgentsState,
    /// What it holds for review; `Err` is git's message when the checkout
    /// could not be inspected.
    pub changes: Result<Changes, String>,
}

impl ReviewEntry {
    /// Whether every file of the entry carries a ✓.
    pub fn fully_reviewed(&self) -> bool {
        matches!(&self.changes, Ok(changes) if changes.reviewed == changes.files)
    }
}

/// The checkouts that wait for review, in the order the viewer walks them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReviewQueue {
    /// Entries with unreviewed files first, the fully reviewed ones last.
    pub entries: Vec<ReviewEntry>,
}

impl ReviewQueue {
    /// The entries among `checkouts` (the Worktrees panel's order): those
    /// with unreviewed files first, the fully reviewed ones last. Checkouts
    /// on a remote host are left out; one git fails on stays in, carrying
    /// the error. `loaded` is a checkout already measured (the one the
    /// viewer just opened), taken as given.
    pub fn build<'a>(
        checkouts: impl IntoIterator<Item = (&'a Worktree, AgentsState)>,
        loaded: Option<(&WorktreeId, &Changes)>,
    ) -> Self {
        let mut entries: Vec<ReviewEntry> = checkouts
            .into_iter()
            .filter_map(|(worktree, agents)| {
                let changes = match loaded {
                    Some((id, changes)) if id == &worktree.id => {
                        (changes.files > 0).then(|| Ok(changes.clone()))
                    }
                    _ => inspect(worktree).transpose(),
                }?;
                Some(ReviewEntry {
                    worktree: worktree.id.clone(),
                    branch: worktree.branch.clone(),
                    agents,
                    changes,
                })
            })
            .collect();
        entries.sort_by_key(ReviewEntry::fully_reviewed);
        Self { entries }
    }

    /// Where `worktree`'s entry sits in the queue, if it has one.
    pub fn position(&self, worktree: Option<&WorktreeId>) -> Option<usize> {
        let worktree = worktree?;
        self.entries.iter().position(|e| &e.worktree == worktree)
    }

    /// The nearest entry after `current`'s (`forward`) or before it that
    /// git could inspect; the others are reachable from the list only. A
    /// checkout outside the queue steps forward onto the first such entry.
    /// `Err` is a user-facing flash message.
    pub fn step(
        &self,
        current: Option<&WorktreeId>,
        forward: bool,
    ) -> Result<&ReviewEntry, &'static str> {
        let inspected = |entry: &&ReviewEntry| entry.changes.is_ok();
        let found = match (self.position(current), forward) {
            (Some(position), true) => self.entries[position + 1..].iter().find(inspected),
            (Some(position), false) => self.entries[..position].iter().rfind(inspected),
            (None, true) => self.entries.iter().find(inspected),
            (None, false) => None,
        };
        found.ok_or(if forward {
            "no next checkout to review"
        } else {
            "no previous checkout to review"
        })
    }
}

fn inspect(worktree: &Worktree) -> Result<Option<Changes>, String> {
    let root = worktree.path.as_path();
    if nebula_core::remote::is_remote(root) || !root.is_dir() {
        return Ok(None);
    }
    let created_from = worktree.created_from.as_deref();
    let source = git_diff::Source::Checkout(root.to_path_buf());
    let (base, files) = git_diff::opening_diff(&source, &worktree.branch, created_from, None)?;
    if files.is_empty() {
        return Ok(None);
    }
    let head = git_diff::head_oid(root);
    let head_ok = head.is_some();
    let reviewed = base
        .mark_scope(head.as_deref().unwrap_or_default())
        .map(|scope| review::load_marks(root, scope))
        .map_or(0, |stored| {
            marks_on_unchanged_diffs(root, &files, &base, head_ok, &stored).len()
        });
    Ok(Some(Changes::measure(
        root, base, &files, head_ok, reviewed,
    )))
}

/// The marks among `stored` whose file is still in `files` with the diff
/// text that was approved.
pub fn marks_on_unchanged_diffs(
    root: &Path,
    files: &[DiffFile],
    base: &DiffBase,
    head_ok: bool,
    stored: &FileMarks,
) -> FileMarks {
    let source = git_diff::Source::Checkout(root.to_path_buf());
    files
        .iter()
        .filter_map(|file| {
            let mark = *stored.get(&file.path)?;
            let diff = git_diff::diff_for(&source, file, base, head_ok);
            (review::fingerprint(&diff) == mark).then(|| (file.path.clone(), mark))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nebula_core::ProjectId;
    use std::path::PathBuf;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
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

    fn checkout(id: &str, path: PathBuf, branch: &str, created_from: Option<&str>) -> Worktree {
        Worktree {
            id: WorktreeId(id.into()),
            project_id: ProjectId("p1".into()),
            path,
            branch: branch.into(),
            is_main: id == "main",
            created_from: created_from.map(str::to_owned),
            pinned: false,
            for_branch: false,
            sort_order: 0,
        }
    }

    fn three_checkouts(dir: &tempfile::TempDir) -> Vec<Worktree> {
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("tracked.txt"), "old line\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);

        let a = dir.path().join("wt-a");
        git(
            &repo,
            &["worktree", "add", "-b", "feat-a", a.to_str().unwrap()],
        );
        std::fs::write(a.join("a.txt"), "one\ntwo\n").unwrap();
        git(&a, &["add", "."]);
        git(&a, &["commit", "-m", "feat a"]);

        let b = dir.path().join("wt-b");
        git(
            &repo,
            &["worktree", "add", "-b", "feat-b", b.to_str().unwrap()],
        );
        std::fs::write(b.join("tracked.txt"), "new line\n").unwrap();
        git(&b, &["commit", "-am", "feat b"]);

        vec![
            checkout("main", repo, "main", None),
            checkout("a", a, "feat-a", Some("main")),
            checkout("b", b, "feat-b", Some("main")),
        ]
    }

    fn build(checkouts: &[Worktree]) -> ReviewQueue {
        ReviewQueue::build(checkouts.iter().map(|w| (w, AgentsState::None)), None)
    }

    #[test]
    fn the_queue_lists_the_checkouts_that_differ_from_their_base() {
        let dir = tempfile::tempdir().unwrap();
        review::with_store_path(dir.path().join("reviewed.json"), || {
            let queue = build(&three_checkouts(&dir));
            let on_main = DiffBase::Branch(crate::git_diff::GitRef::local("main"));
            assert_eq!(
                queue.entries,
                [
                    ReviewEntry {
                        worktree: WorktreeId("a".into()),
                        branch: "feat-a".into(),
                        agents: AgentsState::None,
                        changes: Ok(Changes {
                            base: on_main.clone(),
                            files: 1,
                            added: 2,
                            removed: 0,
                            reviewed: 0,
                        }),
                    },
                    ReviewEntry {
                        worktree: WorktreeId("b".into()),
                        branch: "feat-b".into(),
                        agents: AgentsState::None,
                        changes: Ok(Changes {
                            base: on_main,
                            files: 1,
                            added: 1,
                            removed: 1,
                            reviewed: 0,
                        }),
                    },
                ]
            );
        });
    }

    #[test]
    fn a_fully_reviewed_checkout_sorts_last() {
        let dir = tempfile::tempdir().unwrap();
        review::with_store_path(dir.path().join("reviewed.json"), || {
            let checkouts = three_checkouts(&dir);
            let base = DiffBase::Branch(crate::git_diff::GitRef::local("main"));
            let file = DiffFile {
                path: "a.txt".into(),
                orig_path: None,
                xy: ['A', ' '],
            };
            let approved = git_diff::diff_for(
                &git_diff::Source::Checkout(checkouts[1].path.clone()),
                &file,
                &base,
                true,
            );
            review::store_marks(
                &checkouts[1].path,
                review::MarkScope::Branch { base: "main" },
                &FileMarks::from([("a.txt".to_string(), review::fingerprint(&approved))]),
            );

            let queue = build(&checkouts);
            let order: Vec<(&str, bool)> = queue
                .entries
                .iter()
                .map(|e| (e.branch.as_str(), e.fully_reviewed()))
                .collect();
            assert_eq!(order, [("feat-b", false), ("feat-a", true)]);
            let reviewed = queue.entries[1].changes.as_ref().map(|c| c.reviewed);
            assert_eq!(reviewed, Ok(1));
        });
    }

    fn waiting(id: &str) -> ReviewEntry {
        ReviewEntry {
            worktree: WorktreeId(id.into()),
            branch: id.into(),
            agents: AgentsState::None,
            changes: Ok(Changes {
                base: DiffBase::WorkingTree,
                files: 1,
                added: 0,
                removed: 0,
                reviewed: 0,
            }),
        }
    }

    fn uninspectable(id: &str) -> ReviewEntry {
        ReviewEntry {
            changes: Err("git status failed: boom".into()),
            ..waiting(id)
        }
    }

    #[test]
    fn stepping_past_either_end_stays_put_and_says_so() {
        let queue = ReviewQueue {
            entries: vec![waiting("a"), waiting("b")],
        };
        let (a, b) = (WorktreeId("a".into()), WorktreeId("b".into()));

        assert_eq!(queue.step(Some(&a), true).map(|e| &e.worktree), Ok(&b));
        assert_eq!(queue.step(Some(&b), false).map(|e| &e.worktree), Ok(&a));
        assert_eq!(
            queue.step(Some(&b), true),
            Err("no next checkout to review")
        );
        assert_eq!(
            queue.step(Some(&a), false),
            Err("no previous checkout to review")
        );
    }

    #[test]
    fn stepping_skips_the_checkouts_git_cannot_inspect() {
        let queue = ReviewQueue {
            entries: vec![
                uninspectable("w"),
                waiting("a"),
                uninspectable("x"),
                uninspectable("y"),
                waiting("b"),
                uninspectable("z"),
            ],
        };
        let (a, b) = (WorktreeId("a".into()), WorktreeId("b".into()));
        let outside = WorktreeId("clean".into());

        assert_eq!(queue.step(Some(&a), true).map(|e| &e.worktree), Ok(&b));
        assert_eq!(queue.step(Some(&b), false).map(|e| &e.worktree), Ok(&a));
        assert_eq!(
            queue.step(Some(&outside), true).map(|e| &e.worktree),
            Ok(&a)
        );
        assert_eq!(
            queue.step(Some(&b), true),
            Err("no next checkout to review")
        );
        assert_eq!(
            queue.step(Some(&a), false),
            Err("no previous checkout to review")
        );
    }

    #[test]
    fn a_checkout_git_cannot_inspect_stays_in_the_queue_with_its_error() {
        let dir = tempfile::tempdir().unwrap();
        review::with_store_path(dir.path().join("reviewed.json"), || {
            let mut checkouts = three_checkouts(&dir);
            let not_a_repo = dir.path().join("plain");
            std::fs::create_dir(&not_a_repo).unwrap();
            checkouts.insert(0, checkout("x", not_a_repo, "broken", Some("main")));

            let queue = build(&checkouts);
            let branches: Vec<&str> = queue.entries.iter().map(|e| e.branch.as_str()).collect();
            assert_eq!(branches, ["broken", "feat-a", "feat-b"]);
            let error = queue.entries[0].changes.as_ref().unwrap_err();
            assert!(
                error.starts_with("git status failed: fatal: not a git repository"),
                "{error}"
            );
            assert!(!queue.entries[0].fully_reviewed());
        });
    }
}
