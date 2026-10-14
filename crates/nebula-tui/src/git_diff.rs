//! Git status/diff readers for the diff modal.
//!
//! Synchronous `std::process` on purpose (the `pbcopy` precedent in
//! event_loop.rs): these run only on key events — opening the modal or
//! switching files — and per-file diffs are fast.

use crate::app::DiffView;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Checkout(PathBuf),
    Revision {
        repo: PathBuf,
        branch: String,
        commit: String,
    },
}

impl Source {
    /// Pins local branch `branch` to its current tip: every read through
    /// the result sees that commit, even after the branch moves or is
    /// deleted.
    pub fn branch_tip(repo: &Path, branch: &str) -> Result<Source, String> {
        let commit = rev_oid(repo, &format!("refs/heads/{branch}"))
            .ok_or_else(|| format!("branch {branch} no longer exists"))?;
        Ok(Source::Revision {
            repo: repo.to_path_buf(),
            branch: branch.to_string(),
            commit,
        })
    }

    /// Where git runs: the checkout, or for a revision the repository it is
    /// read from, whose working tree belongs to another checkout and is
    /// never read.
    pub fn dir(&self) -> &Path {
        match self {
            Source::Checkout(path) => path,
            Source::Revision { repo, .. } => repo,
        }
    }

    /// The pinned commit id a revision reads.
    pub fn rev(&self) -> Option<&str> {
        match self {
            Source::Checkout(_) => None,
            Source::Revision { commit, .. } => Some(commit),
        }
    }

    /// When false, no reader may look at or compare with the working tree
    /// under `dir`.
    pub fn has_working_tree(&self) -> bool {
        self.rev().is_none()
    }

    /// At most `max` bytes of `path`; a revision's blob is never buffered
    /// past `max`. `Err` is a user-facing message.
    pub fn read(&self, path: &str, max: usize) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let mut bytes = match self {
            Source::Checkout(root) => {
                let mut bytes = Vec::new();
                std::fs::File::open(root.join(path))
                    .and_then(|file| file.take(max as u64).read_to_end(&mut bytes))
                    .map_err(|e| format!("couldn't read file: {e}"))?;
                bytes
            }
            Source::Revision { repo, commit, .. } => {
                read_blob(repo, &format!("{commit}:{path}"), max)?
            }
        };
        bytes.truncate(max);
        Ok(bytes)
    }

    /// `(cwd, file)` for the editor. A checkout hands back its own
    /// directory, so the real file is edited. A revision streams the pinned
    /// blob into a fresh 0700 directory under `$TMPDIR/nebula-rev` that
    /// this call alone owns, and returns only once the copy is complete and
    /// read-only. Copies are never reused or cleaned up here.
    pub fn editor_target(&self, path: &str) -> Result<(PathBuf, String), String> {
        let Source::Revision { repo, commit, .. } = self else {
            return Ok((self.dir().to_path_buf(), path.to_string()));
        };
        let relative = Path::new(path);
        if !relative
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        {
            return Err(format!("{path} is not a path inside the repository"));
        }
        let failed = |e: String| format!("couldn't copy {path} out of {commit}: {e}");
        let dir = private_copy_dir(commit).map_err(|e| failed(e.to_string()))?;
        let copy = || -> Result<(), String> {
            let file = dir.join(relative);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)
                .map_err(|e| e.to_string())?;
            let mut child = cat_blob(repo, &format!("{commit}:{path}"))?;
            let copied = child
                .stdout
                .take()
                .map(|mut stdout| std::io::copy(&mut stdout, &mut out));
            if let Some(Err(e)) = copied {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
            let output = child.wait_with_output().map_err(|e| e.to_string())?;
            if !output.status.success() {
                return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
            }
            let mut perms = out.metadata().map_err(|e| e.to_string())?.permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&file, perms).map_err(|e| e.to_string())
        };
        if let Err(e) = copy() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(failed(e));
        }
        Ok((dir, path.to_string()))
    }
}

fn private_copy_dir(commit: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join("nebula-rev");
    std::fs::create_dir_all(&root)?;
    let short = &commit[..commit.len().min(12)];
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(format!("{short}-{}-{n}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            result => return result.map(|()| dir),
        }
    }
}

fn cat_blob(root: &Path, object: &str) -> Result<std::process::Child, String> {
    use std::process::Stdio;
    let (program, argv) = nebula_core::remote::git_command(root, &["cat-file", "blob", object]);
    Command::new(program)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run git: {e}"))
}

fn read_blob(root: &Path, object: &str, max: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut child = cat_blob(root, object)?;
    let mut bytes = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        if let Err(e) = stdout.take(max as u64).read_to_end(&mut bytes) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("couldn't read file: {e}"));
        }
    }
    if bytes.len() >= max {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(bytes);
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("couldn't read file: {}", stderr.trim()));
    }
    Ok(bytes)
}

fn rev_oid(root: &Path, rev: &str) -> Option<String> {
    let rev = format!("{rev}^{{commit}}");
    let output = run_git(root, &["rev-parse", "--verify", "--quiet", &rev]).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

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

impl CommitEntry {
    /// `when` squeezed for a narrow row: "3 hours ago" → "3h". Anything
    /// git words differently comes back unchanged.
    pub fn age(&self) -> String {
        let mut words = self.when.split_whitespace();
        let (Some(n), Some(unit)) = (words.next(), words.next()) else {
            return self.when.clone();
        };
        let suffix = match unit.trim_end_matches([',', 's']) {
            "second" => "s",
            "minute" => "m",
            "hour" => "h",
            "day" => "d",
            "week" => "w",
            "month" => "mo",
            "year" => "y",
            _ => return self.when.clone(),
        };
        if n.parse::<u32>().is_err() {
            return self.when.clone();
        }
        format!("{n}{suffix}")
    }
}

/// What the diff viewer compares. `WorkingTree` (the default) is the
/// checkout's uncommitted changes against HEAD; `Upstream` is the working
/// tree against the branch's tracking ref (everything not yet pushed,
/// committed or not); `Branch` is the working tree against its merge base
/// with another branch (everything a merge into that branch would bring);
/// `Commit` is one commit against its first parent.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DiffBase {
    #[default]
    WorkingTree,
    Upstream(GitRef),
    Branch(GitRef),
    Commit(CommitEntry),
}

/// A comparison ref as git names it in full (`refs/heads/main`,
/// `refs/remotes/origin/main`) or a bare commit id: the identity git is
/// asked about, never an abbreviation it would have to disambiguate.
/// Displays as its label (`main`, `origin/main`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRef(String);

impl GitRef {
    /// Local branch `branch`, taken literally: no lookup, so it names
    /// `refs/heads/<branch>` whether or not that ref exists.
    pub fn local(branch: &str) -> Self {
        GitRef(format!("refs/heads/{branch}"))
    }

    /// `refname` as written when it is a full refname (`refs/…`); `None`
    /// for anything git would have to disambiguate.
    pub fn full(refname: &str) -> Option<Self> {
        refname
            .starts_with("refs/")
            .then(|| GitRef(refname.to_string()))
    }

    /// A recorded base (`created_from`, a reflog's `Created from`),
    /// resolved by exact lookups in this order: a full refname as written,
    /// then `refs/heads/<name>`, `refs/remotes/<name>`, `refs/tags/<name>`
    /// when that ref exists, then `name` as a commit id when it is 7–64 hex
    /// characters, else `refs/heads/<name>`.
    pub fn recorded(root: &Path, name: &str) -> Self {
        if let Some(full) = GitRef::full(name) {
            return full;
        }
        let local = GitRef::local(name);
        if local.oid(root).is_some() {
            return local;
        }
        for namespace in ["refs/remotes/", "refs/tags/"] {
            let candidate = GitRef(format!("{namespace}{name}"));
            if candidate.oid(root).is_some() {
                return candidate;
            }
        }
        let commit_id =
            (7..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_hexdigit());
        match commit_id {
            true => GitRef(name.to_string()),
            false => local,
        }
    }

    /// For display only: two refs can share a label
    /// (`refs/heads/origin/main` and `refs/remotes/origin/main`).
    pub fn label(&self) -> &str {
        ["refs/heads/", "refs/remotes/", "refs/tags/"]
            .iter()
            .find_map(|prefix| self.0.strip_prefix(prefix))
            .unwrap_or(&self.0)
    }

    fn oid(&self, root: &Path) -> Option<String> {
        rev_oid(root, &self.0)
    }
}

impl std::fmt::Display for GitRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl DiffBase {
    pub fn is_working_tree(&self) -> bool {
        matches!(self, DiffBase::WorkingTree)
    }

    /// Short label for the diff pane title: nothing for the working tree,
    /// `vs origin/main` for the upstream or a branch, `@ abc1234` for a
    /// commit.
    pub fn title_suffix(&self) -> String {
        match self {
            DiffBase::WorkingTree => String::new(),
            DiffBase::Upstream(name) | DiffBase::Branch(name) => format!(" vs {name}"),
            DiffBase::Commit(c) => format!(" @ {}", c.short),
        }
    }

    /// Where this base's ✓ marks are stored under HEAD OID `head`; `None`
    /// for the bases that have none (the upstream, a commit).
    pub fn mark_scope<'a>(&'a self, head: &'a str) -> Option<crate::review::MarkScope<'a>> {
        use crate::review::MarkScope;
        match self {
            DiffBase::WorkingTree => Some(MarkScope::WorkingTree { head }),
            DiffBase::Branch(base) => Some(MarkScope::Branch { base: base.label() }),
            DiffBase::Upstream(_) | DiffBase::Commit(_) => None,
        }
    }

    /// The `git diff` revision arguments that compare the working tree
    /// with this base: the upstream's tip, or `--merge-base <branch>` so a
    /// branch that moved on does not show its own commits reversed, the
    /// name resolved to its commit id. `Ok(None)` for the bases that are
    /// not a working-tree-vs-rev diff.
    pub fn rev_args(&self, root: &Path) -> Result<Option<Vec<String>>, String> {
        let (git_ref, merge_base) = match self {
            DiffBase::Upstream(git_ref) => (git_ref, false),
            DiffBase::Branch(git_ref) => (git_ref, true),
            DiffBase::WorkingTree | DiffBase::Commit(_) => return Ok(None),
        };
        let oid = git_ref
            .oid(root)
            .ok_or_else(|| format!("{git_ref} no longer resolves"))?;
        Ok(Some(match merge_base {
            true => vec!["--merge-base".to_string(), oid],
            false => vec![oid],
        }))
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

/// The last `MAX_COMMITS` commits reachable from the source's tip, newest
/// first. An unborn HEAD yields an empty list; `Err` is a user-facing
/// flash message.
pub fn recent_commits(source: &Source) -> Result<Vec<CommitEntry>, String> {
    let root = source.dir();
    if source.has_working_tree() && !has_head(root) {
        return Ok(Vec::new());
    }
    let mut args = vec![
        "log",
        "-n",
        MAX_COMMITS,
        "--format=%H%x1f%h%x1f%P%x1f%ar%x1f%s",
    ];
    args.extend(source.rev());
    args.push("--");
    let output = run_git(root, &args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git log failed: {}", stderr.trim()));
    }
    Ok(parse_log(&String::from_utf8_lossy(&output.stdout)))
}

/// The branch's tracking ref (`origin/main`), if it has one.
pub fn upstream_name(source: &Source) -> Option<GitRef> {
    // A branch that has never been pushed (a fresh feature branch) tracks
    // nothing; the remote's copy of the branch, then the remote's default
    // branch (`origin/HEAD` → `origin/develop`), stand in so "vs origin"
    // is still on offer.
    let root = source.dir();
    let (branch, tracking) = match source {
        Source::Revision { branch, .. } => {
            (Some(branch.clone()), configured_upstream(root, branch))
        }
        Source::Checkout(_) => (
            rev_name(root, "HEAD").filter(|b| b != "HEAD"),
            full_ref(root, "@{u}"),
        ),
    };
    tracking
        .or_else(|| full_ref(root, &format!("refs/remotes/origin/{}", branch.as_deref()?)))
        .or_else(|| {
            // The branch it was cut from (`git checkout -b feat develop`),
            // as that branch's own remote copy when it has one.
            let base = GitRef::recorded(root, &creation_base(root, branch.as_deref()?)?);
            let upstream = base
                .0
                .strip_prefix("refs/heads/")
                .and_then(|local| configured_upstream(root, local));
            upstream.or_else(|| base.oid(root).map(|_| base))
        })
        .or_else(|| full_ref(root, "refs/remotes/origin/HEAD"))
}

fn full_ref(root: &Path, rev: &str) -> Option<GitRef> {
    let output = run_git(root, &["rev-parse", "--symbolic-full-name", rev]).ok()?;
    if !output.status.success() {
        return None;
    }
    GitRef::full(String::from_utf8_lossy(&output.stdout).trim())
}

fn configured_upstream(root: &Path, branch: &str) -> Option<GitRef> {
    let refname = format!("refs/heads/{branch}");
    let output = run_git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)%1f%(upstream)",
            &refname,
        ],
    )
    .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| line.split_once('\x1f'))
        .find(|(name, _)| *name == refname)
        .and_then(|(_, upstream)| GitRef::full(upstream))
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

/// The branch a checkout is reviewed against: the one the TUI recorded it
/// was cut from (`created_from`), else the reflog's. `None` when neither
/// is known, it names `branch` itself, or it no longer resolves to a commit.
pub fn review_base(root: &Path, branch: &str, created_from: Option<&str>) -> Option<GitRef> {
    created_from
        .map(str::to_owned)
        .or_else(|| creation_base(root, branch))
        .and_then(|base| recorded_base(root, branch, &base))
}

fn recorded_base(root: &Path, branch: &str, base: &str) -> Option<GitRef> {
    let base = GitRef::recorded(root, base);
    (base != GitRef::local(branch) && base.oid(root).is_some()).then_some(base)
}

/// What the viewer opens on. A checkout: the uncommitted changes whenever
/// there are any, else everything since the review base when that shows
/// something, else the empty working tree. `Err` is a user-facing flash
/// message.
pub fn opening_diff(
    source: &Source,
    branch: &str,
    created_from: Option<&str>,
    primary: Option<&str>,
) -> Result<(DiffBase, Vec<DiffFile>), String> {
    let root = source.dir();
    if !source.has_working_tree() {
        let base = created_from
            .and_then(|base| recorded_base(root, branch, base))
            .map(DiffBase::Branch)
            .or_else(|| configured_upstream(root, branch).map(DiffBase::Upstream))
            .or_else(|| primary.map(|p| DiffBase::Branch(GitRef::local(p))))
            .ok_or_else(|| format!("no base to compare {branch} with"))?;
        let files = files_since(source, &base)?;
        return Ok((base, files));
    }
    let files = changed_files(root)?;
    if !files.is_empty() {
        return Ok((DiffBase::WorkingTree, files));
    }
    let Some(base) = review_base(root, branch, created_from) else {
        return Ok((DiffBase::WorkingTree, files));
    };
    let base = DiffBase::Branch(base);
    let since = files_since(source, &base)?;
    if since.is_empty() {
        return Ok((DiffBase::WorkingTree, files));
    }
    Ok((base, since))
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

/// Every other branch of the checkout for the ^g picker's branch list:
/// local ones first, then the remote-tracking ones, each most recently
/// committed first. `current` (the checked-out branch) is left out.
/// `Err` is a user-facing flash message.
pub fn branches(root: &Path, current: &str) -> Result<Vec<GitRef>, String> {
    let output = run_git(
        root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(symref)%1f%(refname)",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git for-each-ref failed: {}", stderr.trim()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (mut local, mut remote) = (Vec::new(), Vec::new());
    for line in text.lines() {
        // A symref (`origin/HEAD`) only repeats the branch it points at.
        let Some(("", refname)) = line.split_once('\x1f') else {
            continue;
        };
        let Some(git_ref) = GitRef::full(refname) else {
            continue;
        };
        if refname.starts_with("refs/heads/") {
            if git_ref != GitRef::local(current) {
                local.push(git_ref);
            }
        } else if refname.starts_with("refs/remotes/") {
            remote.push(git_ref);
        }
    }
    local.extend(remote);
    Ok(local)
}

/// Files of the source that differ from `base` (`DiffBase::rev_args`;
/// tracked, via `git diff`) plus, for a checkout, the untracked ones from
/// status. `Err` is a user-facing flash.
pub fn files_since(source: &Source, base: &DiffBase) -> Result<Vec<DiffFile>, String> {
    let root = source.dir();
    let rev = base.rev_args(root)?.unwrap_or_default();
    let mut args = vec!["diff", "-M", "--name-status", "-z"];
    args.extend(rev.iter().map(String::as_str));
    args.extend(source.rev());
    args.push("--");
    let output = run_git(root, &args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", stderr.trim()));
    }
    let mut files = parse_name_status_z(&output.stdout);
    if !source.has_working_tree() {
        return Ok(files);
    }
    files.extend(
        changed_files(root)?
            .into_iter()
            .filter(DiffFile::is_untracked),
    );
    Ok(files)
}

/// Lines added and removed across `files`, the list `base` produced:
/// tracked ones from `git diff --numstat`, untracked ones as all added.
/// A git failure or a binary file counts as zero.
pub fn line_counts(
    source: &Source,
    base: &DiffBase,
    files: &[DiffFile],
    head_ok: bool,
) -> (usize, usize) {
    let root = source.dir();
    let mut args = vec!["diff", "--numstat", "-M"];
    let empty_tree;
    let Ok(rev) = base.rev_args(root) else {
        return (0, 0);
    };
    match &rev {
        Some(rev) => {
            args.extend(rev.iter().map(String::as_str));
            args.extend(source.rev());
        }
        None if head_ok => args.push("HEAD"),
        None => {
            empty_tree = empty_tree_oid(root);
            args.push(&empty_tree);
        }
    }
    args.push("--");
    let tracked = run_git(root, &args).map(|o| o.stdout).unwrap_or_default();
    let (mut added, mut removed) = (0, 0);
    for line in String::from_utf8_lossy(&tracked).lines() {
        let mut columns = line.split('\t');
        added += columns.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        removed += columns.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    }
    for file in files.iter().filter(|f| f.is_untracked()) {
        let bytes = std::fs::read(root.join(&file.path)).unwrap_or_default();
        if !bytes.contains(&0) {
            added += String::from_utf8_lossy(&bytes).lines().count();
        }
    }
    (added, removed)
}

/// The empty tree's OID in this repository's hash format: what an unborn
/// HEAD is compared against so staged files count.
fn empty_tree_oid(root: &Path) -> String {
    let output = run_git(root, &["hash-object", "-t", "tree", "/dev/null"]);
    output
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
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

/// Every file of the source in git listing order, for the fuzzy file
/// finder: a checkout's tracked + untracked files (gitignore respected), a
/// revision's tree. `Err` is a user-facing flash message.
pub fn list_files(source: &Source) -> Result<Vec<String>, String> {
    let output = match source {
        Source::Checkout(root) => run_git(
            root,
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?,
        Source::Revision { repo, commit, .. } => {
            run_git(repo, &["ls-tree", "-r", "-z", "--name-only", commit])?
        }
    };
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
pub fn diff_for(source: &Source, file: &DiffFile, base: &DiffBase, head_ok: bool) -> String {
    let root = source.dir();
    let rev = match base.rev_args(root) {
        Ok(rev) => rev,
        Err(msg) => return msg,
    };
    let output = if let DiffBase::Commit(c) = base {
        let mut args = Vec::new();
        commit_range(c, &mut args);
        args.extend(["-p", "-M", "--no-color", "--no-ext-diff", "--", &file.path]);
        if let Some(orig) = &file.orig_path {
            args.push(orig);
        }
        run_git(root, &args)
    } else if let (Some(rev), false) = (&rev, file.is_untracked()) {
        let mut args = vec!["diff"];
        args.extend(rev.iter().map(String::as_str));
        args.extend(source.rev());
        args.extend(["-M", "--no-color", "--no-ext-diff", "--", &file.path]);
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
        Some(file) => diff_for(&view.source, file, &view.base, view.head_ok),
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

    fn checkout(root: &Path) -> Source {
        Source::Checkout(root.to_path_buf())
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
        let diff = diff_for(&checkout(&repo), tracked, &DiffBase::WorkingTree, true);
        assert!(diff.contains("-old line"), "{diff}");
        assert!(diff.contains("+new line"), "{diff}");
        // Untracked goes through the --no-index exit-1 path.
        let diff = diff_for(&checkout(&repo), fresh, &DiffBase::WorkingTree, true);
        assert!(diff.contains("+hello"), "{diff}");
    }

    #[test]
    fn list_files_includes_untracked_and_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        std::fs::write(repo.join("fresh.txt"), "hello\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), "nope\n").unwrap();

        let files = list_files(&checkout(&repo)).unwrap();
        assert!(files.contains(&"tracked.txt".to_string()), "{files:?}");
        assert!(files.contains(&"fresh.txt".to_string()), "{files:?}");
        assert!(!files.contains(&"ignored.txt".to_string()), "{files:?}");
    }

    #[test]
    fn list_files_errors_outside_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let err = list_files(&checkout(dir.path())).unwrap_err();
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
        let diff = diff_for(&checkout(&repo), &files[0], &DiffBase::WorkingTree, false);
        assert!(diff.contains("+content"), "{diff}");
        assert!(
            recent_commits(&checkout(&repo)).unwrap().is_empty(),
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

        let commits = recent_commits(&checkout(&repo)).unwrap();
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
        let diff = diff_for(&checkout(&repo), tracked, &base, true);
        assert!(diff.contains("-old line"), "{diff}");
        assert!(diff.contains("+new line"), "{diff}");

        // Third commit: a rename carries the original path.
        let files = commit_files(&repo, &commits[0]).unwrap();
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].path, "moved.txt");
        assert_eq!(files[0].orig_path.as_deref(), Some("added.txt"));
        assert_eq!(files[0].xy[0], 'R');
        let diff = diff_for(
            &checkout(&repo),
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
            &checkout(&repo),
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
        assert!(upstream_name(&checkout(&repo)).is_none(), "no remote yet");
        // A local "remote": branch `base` tracked as the upstream of main.
        git(&repo, &["branch", "base"]);
        git(&repo, &["branch", "--set-upstream-to=base"]);
        assert_eq!(upstream_name(&checkout(&repo)), Some(GitRef::local("base")));
        std::fs::write(repo.join("tracked.txt"), "committed\n").unwrap();
        git(&repo, &["commit", "-am", "local work"]);
        std::fs::write(repo.join("fresh.txt"), "hello\n").unwrap();

        // Nothing uncommitted except the untracked file …
        let files = changed_files(&repo).unwrap();
        assert_eq!(files.len(), 1);
        // … but against the upstream the committed edit shows too.
        let base = DiffBase::Upstream(GitRef::local("base"));
        let files = files_since(&checkout(&repo), &base).unwrap();
        assert_eq!(files.len(), 2, "{files:?}");
        let tracked = files.iter().find(|f| f.path == "tracked.txt").unwrap();
        assert_eq!(tracked.xy, ['M', ' ']);
        let diff = diff_for(&checkout(&repo), tracked, &base, true);
        assert!(diff.contains("-old line"), "{diff}");
        assert!(diff.contains("+committed"), "{diff}");
        let fresh = files.iter().find(|f| f.path == "fresh.txt").unwrap();
        assert!(diff_for(&checkout(&repo), fresh, &base, true).contains("+hello"));
    }

    #[test]
    fn branch_base_diffs_against_the_merge_base_not_the_branch_tip() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        // `dev` moves on after `feat` is cut from it.
        git(&repo, &["branch", "-m", "dev"]);
        git(&repo, &["checkout", "-b", "feat"]);
        std::fs::write(repo.join("mine.txt"), "one\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feat 1"]);
        std::fs::write(repo.join("mine.txt"), "one\ntwo\n").unwrap();
        git(&repo, &["commit", "-am", "feat 2"]);
        git(&repo, &["checkout", "dev"]);
        std::fs::write(repo.join("theirs.txt"), "dev only\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "dev moved"]);
        git(&repo, &["checkout", "feat"]);

        assert_eq!(branches(&repo, "feat").unwrap(), [GitRef::local("dev")]);
        let base = DiffBase::Branch(GitRef::local("dev"));
        let files = files_since(&checkout(&repo), &base).unwrap();
        // Both feat commits as one change; dev's own commit stays out.
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].path, "mine.txt");
        let diff = diff_for(&checkout(&repo), &files[0], &base, true);
        assert!(diff.contains("+one") && diff.contains("+two"), "{diff}");
    }

    fn branch_with_one_commit(repo: &PathBuf) {
        git(repo, &["checkout", "-b", "feat", "main"]);
        std::fs::write(repo.join("mine.txt"), "one\n").unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-m", "feat 1"]);
    }

    #[test]
    fn a_committed_branch_opens_on_the_branch_it_was_cut_from() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        branch_with_one_commit(&repo);

        let (base, files) = opening_diff(&checkout(&repo), "feat", None, None).unwrap();
        assert_eq!(base, DiffBase::Branch(GitRef::local("main")));
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["mine.txt"]);
    }

    #[test]
    fn a_recorded_base_named_like_a_commit_id_is_the_branch_of_that_name() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        git(&repo, &["branch", "cafe1234"]);
        branch_with_one_commit(&repo);

        assert_eq!(
            review_base(&repo, "feat", Some("cafe1234")),
            Some(GitRef::local("cafe1234"))
        );
    }

    #[test]
    fn the_recorded_base_wins_over_the_reflog() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        git(&repo, &["branch", "release"]);
        branch_with_one_commit(&repo);

        let (base, _) = opening_diff(&checkout(&repo), "feat", Some("release"), None).unwrap();
        assert_eq!(base, DiffBase::Branch(GitRef::local("release")));
    }

    #[test]
    fn an_uncommitted_edit_opens_on_the_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);
        branch_with_one_commit(&repo);
        std::fs::write(repo.join("tracked.txt"), "new line\n").unwrap();

        let (base, files) = opening_diff(&checkout(&repo), "feat", None, None).unwrap();
        assert_eq!(base, DiffBase::WorkingTree);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["tracked.txt"]);
    }

    #[test]
    fn a_clean_branch_with_no_known_base_opens_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir);

        let (base, files) = opening_diff(&checkout(&repo), "main", None, None).unwrap();
        assert_eq!(base, DiffBase::WorkingTree);
        assert!(files.is_empty(), "{files:?}");
    }

    #[test]
    fn line_counts_before_the_first_commit_include_staged_files() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("staged.txt"), "one\ntwo\n").unwrap();
        git(&repo, &["add", "."]);
        std::fs::write(repo.join("loose.txt"), "three\n").unwrap();

        let files = changed_files(&repo).unwrap();
        assert_eq!(
            line_counts(&checkout(&repo), &DiffBase::WorkingTree, &files, false),
            (3, 0)
        );
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
    fn commit_age_is_the_relative_date_squeezed() {
        let age = |when: &str| {
            CommitEntry {
                oid: String::new(),
                short: String::new(),
                parent: None,
                when: when.into(),
                subject: String::new(),
            }
            .age()
        };
        assert_eq!(age("12 minutes ago"), "12m");
        assert_eq!(age("3 hours ago"), "3h");
        assert_eq!(age("1 day ago"), "1d");
        assert_eq!(age("5 months ago"), "5mo");
        assert_eq!(age("1 year, 2 months ago"), "1y");
        assert_eq!(age("now"), "now");
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
