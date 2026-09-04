//! Git status/diff readers for the diff modal.
//!
//! Synchronous `std::process` on purpose (the `pbcopy` precedent in
//! event_loop.rs): these run only on key events — opening the modal or
//! switching files — and per-file diffs are fast.

use crate::app::DiffView;
use std::path::Path;
use std::process::{Command, Output};

/// Keep pathological diffs from bloating the overlay state.
const MAX_DIFF_LINES: usize = 20_000;

/// One changed file from `git status --porcelain=v1 -z`.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffFile {
    /// Path relative to the worktree root (for renames: the NEW path).
    pub path: String,
    /// Pre-rename path for R/C entries.
    pub orig_path: Option<String>,
    /// The two porcelain status columns, e.g. ['M',' '], [' ','M'], ['?','?'].
    pub xy: [char; 2],
}

impl DiffFile {
    pub fn is_untracked(&self) -> bool {
        self.xy == ['?', '?']
    }

    /// The raw two-character code for the list column ("M ", " M", "??", …).
    pub fn status_str(&self) -> String {
        self.xy.iter().collect()
    }
}

/// One row of the diff viewer's ^g commit picker (`git log` order).
#[derive(Debug, Clone, PartialEq)]
pub struct CommitEntry {
    pub oid: String,
    /// Abbreviated OID for titles and rows.
    pub short: String,
    /// First parent; `None` for a root commit (diffed against the empty
    /// tree).
    pub parent: Option<String>,
    /// Relative author date ("3 hours ago").
    pub when: String,
    pub subject: String,
}

/// What the diff viewer compares. `WorkingTree` (the default) is the
/// checkout's uncommitted changes against HEAD; `Upstream` is the working
/// tree against the branch's tracking ref (everything not yet pushed,
/// committed or not); `Commit` is one commit against its first parent.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DiffBase {
    #[default]
    WorkingTree,
    Upstream(String),
    Commit(CommitEntry),
}

impl DiffBase {
    pub fn is_working_tree(&self) -> bool {
        matches!(self, DiffBase::WorkingTree)
    }

    /// Short label for the diff pane title: nothing for the working tree,
    /// `vs origin/main` for the upstream, `@ abc1234` for a commit.
    pub fn title_suffix(&self) -> String {
        match self {
            DiffBase::WorkingTree => String::new(),
            DiffBase::Upstream(name) => format!(" vs {name}"),
            DiffBase::Commit(c) => format!(" @ {}", c.short),
        }
    }
}

/// Picker depth: the last `MAX_COMMITS` commits reachable from HEAD.
const MAX_COMMITS: &str = "200";

/// Line classification for coloring; styling itself lives in ui.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Add,
    Remove,
    Hunk,
    Header,
    Context,
}

pub fn classify_diff_line(line: &str) -> DiffLineKind {
    const HEADERS: [&str; 11] = [
        "diff --git",
        "index ",
        "new file",
        "deleted file",
        "similarity ",
        "dissimilarity ",
        "rename ",
        "copy ",
        "old mode",
        "new mode",
        "Binary files",
    ];
    if line.starts_with("+++") || line.starts_with("---") {
        DiffLineKind::Header
    } else if line.starts_with('+') {
        DiffLineKind::Add
    } else if line.starts_with('-') {
        DiffLineKind::Remove
    } else if line.starts_with("@@") {
        DiffLineKind::Hunk
    } else if HEADERS.iter().any(|h| line.starts_with(h)) {
        DiffLineKind::Header
    } else {
        DiffLineKind::Context
    }
}

/// `git -C root args…` wherever the checkout lives: locally, or over ssh
/// for a remote project (`nebula_core::remote` keeps the path→host map),
/// so every view built on this runner works on a remote checkout unchanged.
fn run_git(root: &Path, args: &[&str]) -> Result<Output, String> {
    let (program, argv) = nebula_core::remote::git_command(root, args);
    Command::new(program)
        .args(argv)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))
}

/// Changed files (staged + unstaged + untracked) in status order.
/// `Err` is a user-facing flash message.
pub fn changed_files(root: &Path) -> Result<Vec<DiffFile>, String> {
    let output = run_git(root, &["status", "--porcelain=v1", "-z", "-uall"])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git status failed: {}", stderr.trim()));
    }
    Ok(parse_status_z(&output.stdout))
}

/// Parse NUL-separated porcelain v1: `XY path\0`, and for X ∈ {R, C} a second
/// NUL-terminated field holds the ORIGINAL path (`XY new\0old\0`).
pub fn parse_status_z(bytes: &[u8]) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut fields = bytes.split(|b| *b == 0);
    while let Some(entry) = fields.next() {
        let entry = String::from_utf8_lossy(entry);
        if entry.len() < 4 {
            continue;
        }
        let mut chars = entry.chars();
        let x = chars.next().unwrap_or(' ');
        let y = chars.next().unwrap_or(' ');
        let path = entry[3..].to_string();
        let orig_path = if matches!(x, 'R' | 'C') {
            fields
                .next()
                .map(|f| String::from_utf8_lossy(f).into_owned())
        } else {
            None
        };
        files.push(DiffFile {
            path,
            orig_path,
            xy: [x, y],
        });
    }
    files
}

/// The last `MAX_COMMITS` commits reachable from HEAD, newest first. An
/// unborn HEAD yields an empty list; `Err` is a user-facing flash message.
pub fn recent_commits(root: &Path) -> Result<Vec<CommitEntry>, String> {
    if !has_head(root) {
        return Ok(Vec::new());
    }
    let output = run_git(
        root,
        &[
            "log",
            "-n",
            MAX_COMMITS,
            "--format=%H%x1f%h%x1f%P%x1f%ar%x1f%s",
        ],
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git log failed: {}", stderr.trim()));
    }
    Ok(parse_log(&String::from_utf8_lossy(&output.stdout)))
}

/// The branch's tracking ref (`origin/main`), if it has one.
pub fn upstream_name(root: &Path) -> Option<String> {
    // A branch that has never been pushed (a fresh feature branch) tracks
    // nothing; the remote's copy of the branch, then the remote's default
    // branch (`origin/HEAD` → `origin/develop`), stand in so "vs origin"
    // is still on offer.
    let branch = rev_name(root, "HEAD").filter(|b| b != "HEAD");
    rev_name(root, "@{u}")
        .or_else(|| rev_name(root, &format!("origin/{}", branch.as_deref()?)))
        .or_else(|| {
            // The branch it was cut from (`git checkout -b feat develop`),
            // as that branch's own remote copy when it has one.
            let base = creation_base(root, branch.as_deref()?)?;
            rev_name(root, &format!("{base}@{{u}}")).or_else(|| rev_name(root, &base))
        })
        .or_else(|| rev_name(root, "origin/HEAD"))
}

/// The branch `branch` was created from, per its reflog's oldest entry
/// (`branch: Created from develop`); `None` for `HEAD`-relative or
/// unlogged creations.
fn creation_base(root: &Path, branch: &str) -> Option<String> {
    let output = run_git(
        root,
        &[
            "reflog",
            "show",
            "--format=%gs",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let base = text
        .lines()
        .last()?
        .strip_prefix("branch: Created from ")?
        .trim()
        .to_string();
    (!base.is_empty() && base != "HEAD" && base != branch).then_some(base)
}

/// `rev` resolved to its short symbolic name (`origin/main`), or `None`
/// when git cannot resolve it.
fn rev_name(root: &Path, rev: &str) -> Option<String> {
    let output = run_git(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", rev],
    )
    .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !name.is_empty()).then_some(name)
}

/// Working-tree files that differ from `rev` (tracked, via `git diff`)
/// plus the untracked ones from status. `Err` is a user-facing flash.
pub fn files_since(root: &Path, rev: &str) -> Result<Vec<DiffFile>, String> {
    let output = run_git(root, &["diff", "-M", "--name-status", "-z", rev])?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", stderr.trim()));
    }
    let mut files = parse_name_status_z(&output.stdout);
    files.extend(
        changed_files(root)?
            .into_iter()
            .filter(DiffFile::is_untracked),
    );
    Ok(files)
}

/// Parse `%H\x1f%h\x1f%P\x1f%ar\x1f%s` lines.
pub fn parse_log(text: &str) -> Vec<CommitEntry> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\x1f');
            let oid = f.next()?.to_string();
            let short = f.next()?.to_string();
            let parent = f.next()?.split_whitespace().next().map(str::to_string);
            let when = f.next()?.to_string();
            let subject = f.next().unwrap_or("").to_string();
            Some(CommitEntry {
                oid,
                short,
                parent,
                when,
                subject,
            })
        })
        .collect()
}

/// The two revisions `git diff` compares for a commit: first parent → the
/// commit, or `--root` for a parentless one (diff-tree handles that form).
fn commit_range<'a>(c: &'a CommitEntry, args: &mut Vec<&'a str>) {
    match &c.parent {
        Some(parent) => {
            args.push("diff");
            args.push(parent);
            args.push(&c.oid);
        }
        None => {
            args.extend(["diff-tree", "-r", "--root", "--no-commit-id", &c.oid]);
        }
    }
}

/// Files a commit touched (against its first parent), in git order.
/// `Err` is a user-facing flash message.
pub fn commit_files(root: &Path, c: &CommitEntry) -> Result<Vec<DiffFile>, String> {
    let mut args = Vec::new();
    commit_range(c, &mut args);
    args.extend(["-M", "--name-status", "-z"]);
    let output = run_git(root, &args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", stderr.trim()));
    }
    Ok(parse_name_status_z(&output.stdout))
}

/// Parse NUL-separated `--name-status -z`: `STATUS\0path\0`, where a
/// rename/copy status (`R100`, `C75`) is followed by `old\0new\0`. The
/// status letter lands in the first porcelain column so the list colors
/// match the working-tree view.
pub fn parse_name_status_z(bytes: &[u8]) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut fields = bytes.split(|b| *b == 0);
    while let Some(status) = fields.next() {
        let status = String::from_utf8_lossy(status);
        let Some(x) = status.chars().next() else {
            continue;
        };
        let Some(first) = fields.next() else {
            break;
        };
        let first = String::from_utf8_lossy(first).into_owned();
        let (path, orig_path) = if matches!(x, 'R' | 'C') {
            let new = fields
                .next()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .unwrap_or_default();
            (new, Some(first))
        } else {
            (first, None)
        };
        files.push(DiffFile {
            path,
            orig_path,
            xy: [x, ' '],
        });
    }
    files
}

/// Every file in the checkout (tracked + untracked, gitignore respected) in
/// git listing order, for the fuzzy file finder. `Err` is a user-facing
/// flash message.
pub fn list_files(root: &Path) -> Result<Vec<String>, String> {
    let output = run_git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git ls-files failed: {}", stderr.trim()));
    }
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect())
}

/// Does this checkout have any commit? Unborn HEAD changes the diff command.
pub fn has_head(root: &Path) -> bool {
    head_oid(root).is_some()
}

/// HEAD's commit OID, `None` on an unborn HEAD (or outside a repo). The
/// review marks are scoped to this: any HEAD move invalidates them.
pub fn head_oid(root: &Path) -> Option<String> {
    let output = run_git(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Diff text for one file. Never fails: errors become the displayed text so
/// the modal survives a repo vanishing out from under it.
pub fn diff_for(root: &Path, file: &DiffFile, base: &DiffBase, head_ok: bool) -> String {
    let output = if let DiffBase::Commit(c) = base {
        let mut args = Vec::new();
        commit_range(c, &mut args);
        args.extend(["-p", "-M", "--no-color", "--no-ext-diff", "--", &file.path]);
        if let Some(orig) = &file.orig_path {
            args.push(orig);
        }
        run_git(root, &args)
    } else if let (DiffBase::Upstream(rev), false) = (base, file.is_untracked()) {
        let mut args = vec![
            "diff",
            rev,
            "-M",
            "--no-color",
            "--no-ext-diff",
            "--",
            &file.path,
        ];
        if let Some(orig) = &file.orig_path {
            args.push(orig);
        }
        run_git(root, &args)
    } else if file.is_untracked() {
        // --no-index exits 1 when the files differ; only >= 2 is an error.
        run_git(
            root,
            &[
                "diff",
                "--no-index",
                "--no-color",
                "--",
                "/dev/null",
                &file.path,
            ],
        )
    } else {
        let mut args = vec!["diff"];
        if head_ok {
            args.push("HEAD");
        }
        args.extend(["--no-color", "--no-ext-diff", "--", &file.path]);
        if let Some(orig) = &file.orig_path {
            args.push(orig);
        }
        run_git(root, &args)
    };
    let output = match output {
        Ok(o) => o,
        Err(e) => return e,
    };
    let ok = if file.is_untracked() {
        matches!(output.status.code(), Some(0) | Some(1))
    } else {
        output.status.success()
    };
    if !ok {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return format!("git diff failed: {}", stderr.trim());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.trim().is_empty() {
        return "(no textual changes)".to_string();
    }
    let mut lines = text.lines();
    let mut out: String = lines
        .by_ref()
        .take(MAX_DIFF_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if lines.next().is_some() {
        out.push_str("\n… (truncated)");
    }
    out
}

/// Reload `view.diff` for the currently selected file and reset the scroll.
pub fn load_selected_diff(view: &mut DiffView) {
    let diff = match view.selected_file() {
        Some(file) => diff_for(&view.root, file, &view.base, view.head_ok),
        None => String::new(),
    };
    view.diff_line_count = diff.lines().count();
    view.diff = diff;
    view.scroll = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_status_z_handles_all_statuses() {
        let bytes =
            b"M  staged.rs\0 M unstaged.rs\0?? new.txt\0D  gone.rs\0R  renamed.rs\0original.rs\0";
        let files = parse_status_z(bytes);
        assert_eq!(files.len(), 5);
        assert_eq!(files[0].path, "staged.rs");
        assert_eq!(files[0].xy, ['M', ' ']);
        assert_eq!(files[1].xy, [' ', 'M']);
        assert!(files[2].is_untracked());
        assert_eq!(files[2].status_str(), "??");
        assert_eq!(files[3].xy, ['D', ' ']);
        assert_eq!(files[4].path, "renamed.rs");
        assert_eq!(files[4].orig_path.as_deref(), Some("original.rs"));
        assert!(files[0].orig_path.is_none());
    }

    #[test]
    fn classify_diff_line_covers_headers_vs_adds() {
        assert_eq!(classify_diff_line("+added"), DiffLineKind::Add);
        assert_eq!(classify_diff_line("+++ b/file"), DiffLineKind::Header);
        assert_eq!(classify_diff_line("-removed"), DiffLineKind::Remove);
        assert_eq!(classify_diff_line("--- a/file"), DiffLineKind::Header);
        assert_eq!(classify_diff_line("@@ -1,3 +1,4 @@"), DiffLineKind::Hunk);
        assert_eq!(
            classify_diff_line("diff --git a/x b/x"),
            DiffLineKind::Header
        );
        assert_eq!(
            classify_diff_line("index 123..456 100644"),
            DiffLineKind::Header
        );
        assert_eq!(
            classify_diff_line("Binary files a/x and b/x differ"),
            DiffLineKind::Header
        );
        assert_eq!(classify_diff_line(" context"), DiffLineKind::Context);
    }

    fn git(repo: &PathBuf, args: &[&str]) {
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

    fn make_repo(dir: &tempfile::TempDir) -> PathBuf {
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("tracked.txt"), "old line\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);
        repo
    }

    #[test]
    fn changed_files_and_diff_for_from_real_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        std::fs::write(repo.join("tracked.txt"), "new line\n").unwrap();
        std::fs::write(repo.join("fresh.txt"), "hello\n").unwrap();

        let files = changed_files(&repo).unwrap();
        assert_eq!(files.len(), 2);
        let tracked = files.iter().find(|f| f.path == "tracked.txt").unwrap();
        let fresh = files.iter().find(|f| f.path == "fresh.txt").unwrap();
        assert!(fresh.is_untracked());

        assert!(has_head(&repo));
        let diff = diff_for(&repo, tracked, &DiffBase::WorkingTree, true);
        assert!(diff.contains("-old line"), "{diff}");
        assert!(diff.contains("+new line"), "{diff}");
        // Untracked goes through the --no-index exit-1 path.
        let diff = diff_for(&repo, fresh, &DiffBase::WorkingTree, true);
        assert!(diff.contains("+hello"), "{diff}");
    }

    #[test]
    fn list_files_includes_untracked_and_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        std::fs::write(repo.join("fresh.txt"), "hello\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), "nope\n").unwrap();

        let files = list_files(&repo).unwrap();
        assert!(files.contains(&"tracked.txt".to_string()), "{files:?}");
        assert!(files.contains(&"fresh.txt".to_string()), "{files:?}");
        assert!(!files.contains(&"ignored.txt".to_string()), "{files:?}");
    }

    #[test]
    fn list_files_errors_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = list_files(dir.path()).unwrap_err();
        assert!(err.contains("git ls-files failed"), "{err}");
    }

    #[test]
    fn head_oid_moves_with_commits() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        let first = head_oid(&repo).expect("committed repo has a HEAD");
        std::fs::write(repo.join("tracked.txt"), "new line\n").unwrap();
        git(&repo, &["commit", "-am", "second"]);
        let second = head_oid(&repo).expect("still has a HEAD");
        assert_ne!(first, second, "a commit moves the OID");
        assert!(head_oid(dir.path()).is_none(), "no repo, no OID");
    }

    #[test]
    fn unborn_head_falls_back_to_plain_diff() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("only.txt"), "content\n").unwrap();

        assert!(!has_head(&repo));
        let files = changed_files(&repo).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].is_untracked());
        let diff = diff_for(&repo, &files[0], &DiffBase::WorkingTree, false);
        assert!(diff.contains("+content"), "{diff}");
        assert!(
            recent_commits(&repo).unwrap().is_empty(),
            "unborn: no picker rows"
        );
    }

    #[test]
    fn recent_commits_and_commit_files_walk_history() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        std::fs::write(repo.join("tracked.txt"), "new line\n").unwrap();
        std::fs::write(repo.join("added.txt"), "added\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "second"]);
        git(&repo, &["mv", "added.txt", "moved.txt"]);
        git(&repo, &["commit", "-m", "third"]);

        let commits = recent_commits(&repo).unwrap();
        assert_eq!(commits.len(), 3);
        assert_eq!(commits[0].subject, "third");
        assert_eq!(commits[2].subject, "init");
        assert!(commits[2].parent.is_none(), "root commit has no parent");
        assert_eq!(commits[1].parent.as_deref(), Some(commits[2].oid.as_str()));
        assert!(!commits[0].when.is_empty());

        // Second commit: one modified, one added.
        let base = DiffBase::Commit(commits[1].clone());
        let files = commit_files(&repo, &commits[1]).unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        let tracked = files.iter().find(|f| f.path == "tracked.txt").unwrap();
        assert_eq!(tracked.xy, ['M', ' ']);
        let added = files.iter().find(|f| f.path == "added.txt").unwrap();
        assert_eq!(added.xy, ['A', ' ']);
        let diff = diff_for(&repo, tracked, &base, true);
        assert!(diff.contains("-old line"), "{diff}");
        assert!(diff.contains("+new line"), "{diff}");

        // Third commit: a rename carries the original path.
        let files = commit_files(&repo, &commits[0]).unwrap();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].path, "moved.txt");
        assert_eq!(files[0].orig_path.as_deref(), Some("added.txt"));
        assert_eq!(files[0].xy[0], 'R');
        let diff = diff_for(
            &repo,
            &files[0],
            &DiffBase::Commit(commits[0].clone()),
            true,
        );
        assert!(diff.contains("rename"), "{diff}");

        // Root commit: diffed against the empty tree.
        let files = commit_files(&repo, &commits[2]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].xy, ['A', ' ']);
        let diff = diff_for(
            &repo,
            &files[0],
            &DiffBase::Commit(commits[2].clone()),
            true,
        );
        assert!(diff.contains("+old line"), "{diff}");
    }

    #[test]
    fn upstream_base_diffs_the_working_tree_against_the_tracking_ref() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        assert!(upstream_name(&repo).is_none(), "no remote yet");
        // A local "remote": branch `base` tracked as the upstream of main.
        git(&repo, &["branch", "base"]);
        git(&repo, &["branch", "--set-upstream-to=base"]);
        assert_eq!(upstream_name(&repo).as_deref(), Some("base"));
        std::fs::write(repo.join("tracked.txt"), "committed\n").unwrap();
        git(&repo, &["commit", "-am", "local work"]);
        std::fs::write(repo.join("fresh.txt"), "hello\n").unwrap();

        // Nothing uncommitted except the untracked file …
        let files = changed_files(&repo).unwrap();
        assert_eq!(files.len(), 1);
        // … but against the upstream the committed edit shows too.
        let files = files_since(&repo, "base").unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        let tracked = files.iter().find(|f| f.path == "tracked.txt").unwrap();
        assert_eq!(tracked.xy, ['M', ' ']);
        let base = DiffBase::Upstream("base".into());
        let diff = diff_for(&repo, tracked, &base, true);
        assert!(diff.contains("-old line"), "{diff}");
        assert!(diff.contains("+committed"), "{diff}");
        let fresh = files.iter().find(|f| f.path == "fresh.txt").unwrap();
        assert!(diff_for(&repo, fresh, &base, true).contains("+hello"));
    }

    #[test]
    fn parse_name_status_z_handles_renames() {
        let files = parse_name_status_z(b"M\0a.rs\0R100\0old.rs\0new.rs\0A\0b.rs\0");
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].xy, ['M', ' ']);
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].orig_path.as_deref(), Some("old.rs"));
        assert_eq!(files[2].path, "b.rs");
    }

    #[test]
    fn parse_log_reads_first_parent_only() {
        let text = "aaa\x1fa\x1fbbb ccc\x1f2 days ago\x1fmerge\nbbb\x1fb\x1f\x1fnow\x1froot";
        let log = parse_log(text);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].parent.as_deref(), Some("bbb"));
        assert_eq!(log[0].subject, "merge");
        assert!(log[1].parent.is_none());
    }

    #[test]
    fn changed_files_errors_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = changed_files(dir.path()).unwrap_err();
        assert!(err.contains("git status failed"), "{err}");
    }
}
