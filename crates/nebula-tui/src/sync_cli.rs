//! `nebula sync`: one project, both machines.
//!
//! A local project and its twin on a host (`nebula add findl:~/repo`, same
//! name, same workspace) are two checkouts of one repo that nothing keeps
//! in step. This verb reports both sides, moves commits across in the safe
//! direction, and carries the git-ignored context along (`.env*` laptop →
//! host, `.scratch/` both ways, skills laptop → host). Whenever moving
//! would lose or overwrite work it refuses with the reason and the command
//! that resolves it — the refusal is the product; a half sync is the
//! failure mode this replaces.
//!
//! Commits travel through `origin`, never over the relay: each side already
//! pushes and pulls with its own deploy key. Everything ssh-shaped goes
//! through [`HostRunner`], so the tests drive the whole flow with two local
//! clones and never reach a real host.

use crate::remote_cli::{self, Snapshot};
use anyhow::{bail, Context, Result};
use nebula_core::envfiles::{self, Pushed};
use nebula_core::remote::ssh_quote;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Which way commits may move. Context (`.env*`, `.scratch/`, skills)
/// follows the same restriction: `To` sends, `From` collects, `Both` does
/// both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Both,
    To,
    From,
}

#[derive(Clone, Debug)]
pub struct SyncOpts {
    pub direction: Direction,
    /// The host `--to`/`--from` named. The twin decides the real host; a
    /// different name here is an error, never a redirect.
    pub named_host: Option<String>,
    /// Print the JSON state of both sides (plus the plan) and stop.
    pub status: bool,
    /// Print the plan, change nothing.
    pub dry_run: bool,
    /// Diverged branches: rebase this checkout onto the host's commits.
    pub rebase: bool,
    /// Overwrite `.env` files the host already has.
    pub force_env: bool,
    /// Run the skills mirror (`nebula-sync-skills`) as part of the sync.
    pub skills: bool,
    /// Project name when the cwd is not a checkout.
    pub project: Option<String>,
}

/// One action of a plan, in execution order. `Push*` sends a side's
/// branch to origin; `Pull*` fast-forwards a side from origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    PushLocal,
    PullRemote,
    PushRemote,
    PullLocal,
    /// `git rebase origin/<branch>` here — only with `--rebase`, and only
    /// ever this side: the host's history is never rewritten.
    RebaseLocal,
    CarryEnv,
    CarryScratch,
    MirrorSkills,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPlan {
    Steps(Vec<Step>),
    Refusal { reason: String, hint: String },
}

/// One checkout, as the probe script reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Side {
    pub head: String,
    pub branch: String,
    /// Uncommitted changes to tracked files (untracked ones never block a
    /// fast-forward).
    pub dirty: bool,
    pub has_origin: bool,
    /// `git fetch origin` succeeded (false with no origin, or offline).
    pub fetch_ok: bool,
    /// `.scratch/` exists.
    pub scratch: bool,
}

/// Everything the planner looks at. `remote: None` means the host could
/// not be reached; `unreachable` then carries ssh's complaint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncState {
    pub project: String,
    pub host: String,
    pub branch: String,
    pub local: Side,
    pub remote: Option<Side>,
    pub unreachable: Option<String>,
    /// Commits here the host lacks.
    pub local_ahead: usize,
    /// Commits on the host this side lacks.
    pub remote_ahead: usize,
    /// Local `.env*` files that could travel.
    pub env_files: usize,
}

/// The planner. Pure: the safety rules live here and nowhere else, in
/// this order — unreachable, wrong branch, no origin, a dirty side that
/// would receive commits, diverged without `--rebase`, then the move.
pub fn plan(state: &SyncState, direction: Direction, rebase: bool, skills: bool) -> SyncPlan {
    let host = &state.host;
    let refuse = |reason: String, hint: String| SyncPlan::Refusal { reason, hint };
    let Some(remote) = &state.remote else {
        return refuse(
            format!(
                "{host} is unreachable{}",
                state
                    .unreachable
                    .as_deref()
                    .map(|e| format!(": {e}"))
                    .unwrap_or_default()
            ),
            format!("check `nebula remote {host} status`; a laptop offline or a host down both look like this"),
        );
    };
    if remote.branch != state.branch {
        return refuse(
            format!(
                "the checkout on {host} is on {}, not {}",
                remote.branch, state.branch
            ),
            format!(
                "sync only pairs checkouts of the same branch — `nebula worktree list --project {}@{host}` shows the host's",
                state.project
            ),
        );
    }
    for (side, where_, s) in [
        ("here", "here", &state.local),
        ("the host", host.as_str(), remote),
    ] {
        if !s.has_origin {
            return refuse(
                format!("the checkout {where_} has no `origin` remote"),
                format!("commits travel through origin; add one on {side}: `git remote add origin <url>`"),
            );
        }
        if !s.fetch_ok {
            return refuse(
                format!("`git fetch origin` failed {where_}"),
                format!("origin must answer on {side} (network, deploy key); retry when it does"),
            );
        }
    }

    let dirty_local = || {
        refuse(
            "this checkout has uncommitted changes and would receive commits".into(),
            "commit or stash here first (stage explicit paths, never `git add -A` in a shared checkout)".into(),
        )
    };
    let dirty_remote = || {
        refuse(
            format!("the checkout on {host} has uncommitted changes and would receive commits"),
            format!(
                "commit or stash on {host} first: `nebula agent send <name> … --project {}@{host}` to a session there, or `ssh {host} 'git -C <path> stash'`",
                state.project
            ),
        )
    };
    let wrong_way = |flag: &str, other: &str, who: &str, n: usize| {
        refuse(
            format!("{who} ahead by {n}; --{flag} would need the other direction"),
            format!("run `nebula sync --{other} {host}` or a bare `nebula sync`"),
        )
    };

    let mut steps = Vec::new();
    let (la, ra) = (state.local_ahead, state.remote_ahead);
    if la > 0 && ra > 0 {
        // Diverged. Only ever rebase this side; the host keeps its history.
        if state.local.dirty {
            return dirty_local();
        }
        if !rebase {
            return refuse(
                format!("both sides have commits the other lacks: here +{la}, {host} +{ra}"),
                format!("rerun with --rebase to rebase this checkout onto {host}'s commits (history here is rewritten, never the host's), or merge by hand"),
            );
        }
        if direction != Direction::From && remote.dirty {
            return dirty_remote();
        }
        steps.extend([Step::PushRemote, Step::RebaseLocal]);
        if direction != Direction::From {
            steps.extend([Step::PushLocal, Step::PullRemote]);
        }
    } else if la > 0 {
        if direction == Direction::From {
            return wrong_way("from", "to", "this checkout is", la);
        }
        if remote.dirty {
            return dirty_remote();
        }
        steps.extend([Step::PushLocal, Step::PullRemote]);
    } else if ra > 0 {
        if direction == Direction::To {
            return wrong_way("to", "from", &format!("{host} is"), ra);
        }
        if state.local.dirty {
            return dirty_local();
        }
        steps.extend([Step::PushRemote, Step::PullLocal]);
    }

    if direction != Direction::From && state.env_files > 0 {
        steps.push(Step::CarryEnv);
    }
    let scratch_out = direction != Direction::From && state.local.scratch;
    let scratch_in = direction != Direction::To && remote.scratch;
    if scratch_out || scratch_in {
        steps.push(Step::CarryScratch);
    }
    if skills && direction != Direction::From {
        steps.push(Step::MirrorSkills);
    }
    SyncPlan::Steps(steps)
}

/// "Run this on the host": the one seam between `sync` and ssh. The real
/// implementation is ssh in BatchMode; the tests run the host's script
/// locally against a second clone.
pub trait HostRunner {
    /// Shell script on the host; stdout on success, the host's stderr as
    /// the error otherwise.
    fn run(&self, script: &str) -> Result<String>;
    /// `.env*` files, laptop → host, kept-unless-overwrite.
    fn push_env(
        &self,
        local_repo: &Path,
        remote_repo: &Path,
        files: &[PathBuf],
        overwrite: bool,
    ) -> Result<Pushed>;
    /// One rsync pass of `.scratch/`, update-only, never deleting.
    fn carry_scratch(&self, local_repo: &Path, remote_repo: &Path, to_host: bool) -> Result<()>;
    /// The skills mirror; returns the line to print.
    fn mirror_skills(&self) -> Result<String>;
}

/// The ssh-backed runner.
pub struct SshHost {
    pub host: String,
}

impl HostRunner for SshHost {
    fn run(&self, script: &str) -> Result<String> {
        remote_cli::ssh(&self.host, script, false)
    }

    fn push_env(
        &self,
        local_repo: &Path,
        remote_repo: &Path,
        files: &[PathBuf],
        overwrite: bool,
    ) -> Result<Pushed> {
        envfiles::push(local_repo, &self.host, remote_repo, files, overwrite)
    }

    fn carry_scratch(&self, local_repo: &Path, remote_repo: &Path, to_host: bool) -> Result<()> {
        let local = format!("{}/.scratch/", local_repo.display());
        let remote = format!("{}:{}/.scratch/", self.host, remote_repo.display());
        let (src, dst) = if to_host {
            (local, remote)
        } else {
            std::fs::create_dir_all(local_repo.join(".scratch")).context("create .scratch")?;
            (remote, local)
        };
        let ssh = format!("ssh {}", nebula_core::remote::SSH_BATCH_OPTS.join(" "));
        rsync_update(&src, &dst, Some(&ssh))
    }

    fn mirror_skills(&self) -> Result<String> {
        let host = &self.host;
        let Some(script) = remote_cli::which("nebula-sync-skills") else {
            return Ok(
                "skills: `nebula-sync-skills` not on PATH — see the sync-skills skill; skipped"
                    .into(),
            );
        };
        let status = Command::new(script)
            .arg(host)
            .status()
            .context("run nebula-sync-skills")?;
        Ok(if status.success() {
            format!("skills → {host}")
        } else {
            format!("skills → {host}: sync failed ({status})")
        })
    }
}

/// `rsync -a --update src/ dst/`: newer files win, nothing is deleted.
fn rsync_update(src: &str, dst: &str, shell: Option<&str>) -> Result<()> {
    let mut cmd = Command::new("rsync");
    cmd.args(["-a", "--update"]);
    if let Some(sh) = shell {
        cmd.args(["-e", sh]);
    }
    let out = cmd.arg(src).arg(dst).output().context("run rsync")?;
    if !out.status.success() {
        bail!("rsync: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// The pair of checkouts a sync acts on, resolved from the daemon snapshot
/// (or built by hand in tests).
#[derive(Clone, Debug)]
pub struct SyncTarget {
    pub project: String,
    pub host: String,
    pub branch: String,
    pub local_path: PathBuf,
    pub remote_path: PathBuf,
    /// Unarchived sessions on each checkout, from the snapshot.
    pub local_sessions: usize,
    pub remote_sessions: usize,
}

/// One checkout's facts, as one shell script (the same one runs here via
/// `sh -c` and on the host via the runner). `other` is the far side's head:
/// when this side already has that commit it reports the exact
/// ahead/behind pair (`vs=<mine>\t<theirs>`).
fn probe_script(path: &Path, branch: &str, other: Option<&str>) -> String {
    let p = ssh_quote(&path.to_string_lossy());
    let b = ssh_quote(branch);
    let o = ssh_quote(other.unwrap_or(""));
    format!(
        "cd {p} 2>/dev/null || {{ echo missing=1; exit 0; }}; \
         if git remote get-url origin >/dev/null 2>&1; then echo origin=1; \
           if git fetch -q origin 2>/dev/null; then echo fetch=1; else echo fetch=0; fi; \
         else echo origin=0; echo fetch=0; fi; \
         echo branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null); \
         echo head=$(git rev-parse HEAD 2>/dev/null); \
         if [ -n \"$(git status --porcelain --untracked-files=no 2>/dev/null)\" ]; then echo dirty=1; else echo dirty=0; fi; \
         echo ahead_origin=$(git rev-list --count \"origin/\"{b}..HEAD 2>/dev/null || echo 0); \
         if [ -n {o} ] && git cat-file -e {o} 2>/dev/null; then echo \"vs=$(git rev-list --left-right --count HEAD...{o})\"; fi; \
         if [ -d .scratch ]; then echo scratch=1; else echo scratch=0; fi"
    )
}

/// What one probe said: the side, its commits beyond origin, and the
/// exact (mine, theirs) pair when it knew the other head.
struct Probe {
    side: Side,
    ahead_origin: usize,
    vs: Option<(usize, usize)>,
}

fn parse_probe(out: &str) -> Result<Probe> {
    let mut side = Side::default();
    let mut ahead_origin = 0;
    let mut vs = None;
    for line in out.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "missing" => bail!("no such checkout"),
            "origin" => side.has_origin = v == "1",
            "fetch" => side.fetch_ok = v == "1",
            "branch" => side.branch = v.to_string(),
            "head" => side.head = v.to_string(),
            "dirty" => side.dirty = v == "1",
            "scratch" => side.scratch = v == "1",
            "ahead_origin" => ahead_origin = v.parse().unwrap_or(0),
            "vs" => {
                let mut it = v
                    .split_whitespace()
                    .map(|n| n.parse::<usize>().unwrap_or(0));
                vs = Some((it.next().unwrap_or(0), it.next().unwrap_or(0)));
            }
            _ => {}
        }
    }
    if side.head.is_empty() {
        bail!("not a git checkout: {}", out.trim());
    }
    Ok(Probe {
        side,
        ahead_origin,
        vs,
    })
}

fn sh(script: &str) -> Result<String> {
    let out = Command::new("sh")
        .args(["-c", script])
        .output()
        .context("run sh")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Probe both sides and work out who is ahead of whom. Exact when either
/// side has the other's head (it fetched it from origin); with unpushed
/// commits on both sides each side's lead is measured from origin, which
/// is what the refusal reports.
pub fn gather(target: &SyncTarget, host: &dyn HostRunner) -> Result<SyncState> {
    let local = parse_probe(&sh(&probe_script(
        &target.local_path,
        &target.branch,
        None,
    ))?)
    .with_context(|| format!("probe {}", target.local_path.display()))?;
    let env_files = envfiles::list_local(&target.local_path)
        .map(|f| f.len())
        .unwrap_or(0);
    let mut state = SyncState {
        project: target.project.clone(),
        host: target.host.clone(),
        branch: target.branch.clone(),
        local: local.side.clone(),
        env_files,
        ..Default::default()
    };
    let remote = match host.run(&probe_script(
        &target.remote_path,
        &target.branch,
        Some(&local.side.head),
    )) {
        Ok(out) => match parse_probe(&out) {
            Ok(p) => p,
            Err(e) => {
                state.unreachable = Some(format!("{e:#}"));
                return Ok(state);
            }
        },
        Err(e) => {
            state.unreachable = Some(format!("{e:#}"));
            return Ok(state);
        }
    };
    let (la, ra) = if let Some((theirs, mine)) = remote.vs {
        (mine, theirs)
    } else {
        // The host doesn't have our head; maybe we have theirs (it pushed).
        let vs = sh(&format!(
            "cd {p} && git cat-file -e {r} 2>/dev/null && git rev-list --left-right --count HEAD...{r}",
            p = ssh_quote(&target.local_path.to_string_lossy()),
            r = ssh_quote(&remote.side.head)
        ))
        .ok()
        .and_then(|out| {
            let mut it = out.split_whitespace().map(|n| n.parse::<usize>().ok());
            Some((it.next()??, it.next()??))
        });
        vs.unwrap_or((local.ahead_origin, remote.ahead_origin))
    };
    state.remote = Some(remote.side);
    state.local_ahead = la;
    state.remote_ahead = ra;
    Ok(state)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn git_local(path: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .context("run git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What a step will do, one line, for `--dry-run` and the status plan.
fn describe(step: Step, t: &SyncTarget) -> String {
    let (host, b) = (&t.host, &t.branch);
    match step {
        Step::PushLocal => format!("push here → origin: {b}"),
        Step::PullRemote => format!("fast-forward {host}: {b} from origin"),
        Step::PushRemote => format!("push {host} → origin: {b}"),
        Step::PullLocal => format!("fast-forward here: {b} from origin"),
        Step::RebaseLocal => format!("rebase here onto origin/{b} (host history untouched)"),
        Step::CarryEnv => format!("env → {host} (existing files kept unless --force-env)"),
        Step::CarryScratch => format!("scratch ⇄ {host} (newer file wins, nothing deleted)"),
        Step::MirrorSkills => format!("skills → {host} (nebula-sync-skills)"),
    }
}

fn execute(
    step: Step,
    t: &SyncTarget,
    state: &SyncState,
    opts: &SyncOpts,
    host: &dyn HostRunner,
    out: &mut dyn Write,
) -> Result<()> {
    let (h, b) = (&t.host, &t.branch);
    let rp = ssh_quote(&t.remote_path.to_string_lossy());
    match step {
        Step::PushLocal => {
            git_local(&t.local_path, &["push", "-q", "origin", b])?;
            let head = git_local(&t.local_path, &["rev-parse", "HEAD"])?;
            writeln!(out, "push here → origin: {b} @ {}", short(&head))?;
        }
        Step::PullRemote => {
            let line = host.run(&format!(
                "git -C {rp} fetch -q origin 2>&1 && git -C {rp} merge -q --ff-only origin/{b} 2>&1 && git -C {rp} log --oneline -1"
            ))?;
            writeln!(out, "git {h}:{}: {}", t.remote_path.display(), line.trim())?;
        }
        Step::PushRemote => {
            let head = host.run(&format!(
                "git -C {rp} push -q origin {b} 2>&1 && git -C {rp} rev-parse HEAD"
            ))?;
            writeln!(out, "push {h} → origin: {b} @ {}", short(head.trim()))?;
        }
        Step::PullLocal => {
            git_local(&t.local_path, &["fetch", "-q", "origin"])?;
            git_local(
                &t.local_path,
                &["merge", "-q", "--ff-only", &format!("origin/{b}")],
            )?;
            let line = git_local(&t.local_path, &["log", "--oneline", "-1"])?;
            writeln!(out, "git here: {line}")?;
        }
        Step::RebaseLocal => {
            git_local(&t.local_path, &["fetch", "-q", "origin"])?;
            if let Err(e) = git_local(&t.local_path, &["rebase", "-q", &format!("origin/{b}")]) {
                let _ = git_local(&t.local_path, &["rebase", "--abort"]);
                bail!(
                    "rebase onto origin/{b} stopped on conflicts and was aborted — rebase by hand, then rerun `nebula sync`\n  {e}"
                );
            }
            let head = git_local(&t.local_path, &["rev-parse", "HEAD"])?;
            writeln!(out, "rebase here onto origin/{b}: now @ {}", short(&head))?;
        }
        Step::CarryEnv => {
            let files = envfiles::list_local(&t.local_path)?;
            let p = host.push_env(&t.local_path, &t.remote_path, &files, opts.force_env)?;
            let list = |v: &[PathBuf]| {
                v.iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if !p.sent.is_empty() {
                writeln!(out, "env → {h}: sent {}", list(&p.sent))?;
            }
            if !p.kept.is_empty() {
                writeln!(
                    out,
                    "env: kept on {h} (differs? --force-env overwrites): {}",
                    list(&p.kept)
                )?;
            }
        }
        Step::CarryScratch => {
            let remote_has = state.remote.as_ref().is_some_and(|r| r.scratch);
            if opts.direction != Direction::From && state.local.scratch {
                host.carry_scratch(&t.local_path, &t.remote_path, true)?;
                writeln!(out, "scratch → {h}: newer files sent, nothing deleted")?;
            }
            if opts.direction != Direction::To && remote_has {
                host.carry_scratch(&t.local_path, &t.remote_path, false)?;
                writeln!(out, "scratch ← {h}: newer files fetched, nothing deleted")?;
            }
        }
        Step::MirrorSkills => {
            writeln!(out, "{}", host.mirror_skills()?)?;
        }
    }
    Ok(())
}

/// The `--status` document.
fn status_json(t: &SyncTarget, state: &SyncState, plan: &SyncPlan) -> serde_json::Value {
    let remote = state.remote.as_ref();
    serde_json::json!({
        "project": t.project,
        "branch": t.branch,
        "host": t.host,
        "local": {
            "path": t.local_path,
            "head": state.local.head,
            "dirty": state.local.dirty,
            "ahead": state.local_ahead,
            "sessions": t.local_sessions,
        },
        "remote": {
            "path": t.remote_path,
            "head": remote.map(|r| r.head.clone()),
            "dirty": remote.map(|r| r.dirty),
            "ahead": remote.map(|_| state.remote_ahead),
            "sessions": t.remote_sessions,
            "reachable": remote.is_some(),
            "error": state.unreachable,
        },
        "plan": plan,
    })
}

/// Run one sync against a resolved target. Refusals come back as the
/// error (non-zero exit); `--status` and `--dry-run` never fail on one.
pub fn sync_target(
    t: &SyncTarget,
    opts: &SyncOpts,
    host: &dyn HostRunner,
    out: &mut dyn Write,
) -> Result<()> {
    let state = gather(t, host)?;
    let plan = plan(&state, opts.direction, opts.rebase, opts.skills);
    if opts.status {
        writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&status_json(t, &state, &plan))?
        )?;
        return Ok(());
    }
    let steps = match plan {
        SyncPlan::Refusal { reason, hint } => {
            if opts.dry_run {
                writeln!(out, "would refuse: {reason}\n  → {hint}")?;
                return Ok(());
            }
            bail!("refused: {reason}\n  → {hint}");
        }
        SyncPlan::Steps(steps) => steps,
    };
    let moves = steps.iter().any(|s| {
        matches!(
            s,
            Step::PushLocal
                | Step::PullRemote
                | Step::PushRemote
                | Step::PullLocal
                | Step::RebaseLocal
        )
    });
    if !moves {
        writeln!(
            out,
            "in step: {} @ {} here and on {}{}",
            t.branch,
            short(&state.local.head),
            t.host,
            if steps.is_empty() {
                "; nothing to carry"
            } else {
                ""
            }
        )?;
        if steps.is_empty() {
            return Ok(());
        }
    }
    if opts.dry_run {
        writeln!(
            out,
            "plan ({} step{}, nothing run):",
            steps.len(),
            if steps.len() == 1 { "" } else { "s" }
        )?;
        for s in &steps {
            writeln!(out, "  {}", describe(*s, t))?;
        }
        return Ok(());
    }
    for s in steps {
        execute(s, t, &state, opts, host, out)?;
    }
    Ok(())
}

/// Which pair of checkouts: `--project` wins; otherwise the calling
/// session's worktree, else the deepest local checkout holding the cwd.
/// The twin is the same-named project of the same workspace with a host;
/// the remote checkout is its worktree on this checkout's branch.
pub fn resolve_target(snap: &Snapshot, project_flag: Option<&str>) -> Result<SyncTarget> {
    let (projects, worktrees, agents) = snap;
    let cwd = std::env::current_dir().ok();
    let deepest_local = |filter: &dyn Fn(&nebula_core::Worktree) -> bool| {
        let cwd = cwd.as_ref()?;
        worktrees
            .iter()
            .filter(|w| filter(w) && cwd.starts_with(&w.path))
            .filter(|w| {
                projects
                    .iter()
                    .any(|p| p.id == w.project_id && p.host.is_none())
            })
            .max_by_key(|w| w.path.components().count())
    };
    let (project, local_path) = if let Some(flag) = project_flag {
        if let Some((name, host)) = flag
            .rsplit_once('@')
            .filter(|(n, h)| !n.is_empty() && !h.is_empty())
        {
            bail!(
                "`nebula sync` runs from the local side: pass --project {name}, not {name}@{host}"
            );
        }
        let p = projects
            .iter()
            .find(|p| p.name == flag && p.host.is_none())
            .with_context(|| format!("no local project named \"{flag}\""))?;
        let path = deepest_local(&|w| w.project_id == p.id)
            .map(|w| w.path.clone())
            .unwrap_or_else(|| p.repo_path.clone());
        (p, path)
    } else {
        let by_agent = std::env::var("NEBULA_AGENT_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .and_then(|id| agents.iter().find(|a| a.id.as_str() == id))
            .and_then(|a| worktrees.iter().find(|w| w.id == a.worktree_id));
        let w = by_agent
            .or_else(|| deepest_local(&|_| true))
            .context("not inside a nebula session or a known checkout — pass --project <name>")?;
        let p = projects
            .iter()
            .find(|p| p.id == w.project_id)
            .context("this checkout's project is gone from the daemon")?;
        if let Some(h) = &p.host {
            bail!("this session lives on {h}; `nebula sync` runs from the local side");
        }
        (p, w.path.clone())
    };
    let branch = git_local(&local_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "HEAD" {
        bail!(
            "{} is on a detached HEAD; check a branch out first",
            local_path.display()
        );
    }
    let twin = projects
        .iter()
        .find(|t| t.host.is_some() && t.name == project.name && t.workspace_id == project.workspace_id)
        .with_context(|| {
            format!(
                "{} has no twin on another host — nothing to sync with (add one: `nebula add findl:~/{}`; from a host, the laptop is not reachable)",
                project.name, project.name
            )
        })?;
    let host = twin.host.clone().unwrap_or_default();
    let remote_wt = worktrees
        .iter()
        .find(|w| w.project_id == twin.id && w.branch == branch)
        .with_context(|| {
            format!(
                "twin on {host} has no checkout of {branch}; create one with `nebula worktree new … --project {}@{host}` or `nebula switch`",
                project.name
            )
        })?;
    let sessions = |path: &Path| {
        agents
            .iter()
            .filter(|a| !a.archived)
            .filter(|a| {
                worktrees
                    .iter()
                    .any(|w| w.id == a.worktree_id && w.path == path)
            })
            .count()
    };
    Ok(SyncTarget {
        project: project.name.clone(),
        host,
        branch,
        local_sessions: sessions(&local_path),
        remote_sessions: sessions(&remote_wt.path),
        local_path,
        remote_path: remote_wt.path.clone(),
    })
}

pub async fn run(opts: SyncOpts) -> Result<()> {
    let snap = remote_cli::snapshot().await?;
    let target = resolve_target(&snap, opts.project.as_deref())?;
    if let Some(named) = opts.named_host.as_deref().filter(|h| *h != target.host) {
        bail!(
            "{} has its twin on {}, not {named} — say `--to {}` / `--from {}` (or no host at all)",
            target.project,
            target.host,
            target.host,
            target.host
        );
    }
    let host = SshHost {
        host: target.host.clone(),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    sync_target(&target, &opts, &host, &mut out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(dirty: bool) -> Side {
        Side {
            head: "aaaaaaa".into(),
            branch: "main".into(),
            dirty,
            has_origin: true,
            fetch_ok: true,
            scratch: false,
        }
    }

    fn state(reachable: bool, dl: bool, dr: bool, la: usize, ra: usize) -> SyncState {
        SyncState {
            project: "p".into(),
            host: "findl".into(),
            branch: "main".into(),
            local: side(dl),
            remote: reachable.then(|| side(dr)),
            unreachable: (!reachable).then(|| "ssh: connect timed out".into()),
            local_ahead: la,
            remote_ahead: ra,
            env_files: 0,
        }
    }

    fn steps(p: SyncPlan) -> Vec<Step> {
        match p {
            SyncPlan::Steps(s) => s,
            SyncPlan::Refusal { reason, .. } => panic!("refused: {reason}"),
        }
    }

    fn refusal(p: SyncPlan) -> String {
        match p {
            SyncPlan::Refusal { reason, hint } => format!("{reason} → {hint}"),
            SyncPlan::Steps(s) => panic!("expected refusal, got {s:?}"),
        }
    }

    /// The safety table, one row per rule. `(reachable, dirty local, dirty
    /// remote, local ahead, remote ahead, direction, rebase)` → outcome.
    #[test]
    fn planner_table() {
        use Direction::*;
        use Step::*;
        let ok = |s: &[Step]| Ok::<Vec<Step>, &str>(s.to_vec());
        // Expected refusals are named by a substring of the reason.
        let table: Vec<(
            (bool, bool, bool, usize, usize, Direction, bool),
            Result<Vec<Step>, &str>,
        )> = vec![
            // Unreachable refuses no matter what else is true.
            ((false, false, false, 0, 0, Both, false), Err("unreachable")),
            ((false, true, true, 3, 3, To, true), Err("unreachable")),
            // In step: nothing moves.
            ((true, false, false, 0, 0, Both, false), ok(&[])),
            ((true, true, true, 0, 0, Both, false), ok(&[])),
            // Local ahead: push, host fast-forwards. A dirty host refuses;
            // a dirty local side is fine (it sends, never receives).
            (
                (true, false, false, 2, 0, Both, false),
                ok(&[PushLocal, PullRemote]),
            ),
            (
                (true, true, false, 2, 0, Both, false),
                ok(&[PushLocal, PullRemote]),
            ),
            (
                (true, false, true, 2, 0, Both, false),
                Err("on findl has uncommitted"),
            ),
            (
                (true, false, false, 2, 0, To, false),
                ok(&[PushLocal, PullRemote]),
            ),
            (
                (true, false, false, 2, 0, From, false),
                Err("--from would need the other direction"),
            ),
            // Host ahead: mirror image.
            (
                (true, false, false, 0, 1, Both, false),
                ok(&[PushRemote, PullLocal]),
            ),
            (
                (true, false, true, 0, 1, Both, false),
                ok(&[PushRemote, PullLocal]),
            ),
            (
                (true, true, false, 0, 1, Both, false),
                Err("this checkout has uncommitted"),
            ),
            (
                (true, false, false, 0, 1, From, false),
                ok(&[PushRemote, PullLocal]),
            ),
            (
                (true, false, false, 0, 1, To, false),
                Err("--to would need the other direction"),
            ),
            // Diverged: refused without --rebase; with it, only this side
            // is rebased, then the host fast-forwards.
            (
                (true, false, false, 1, 1, Both, false),
                Err("both sides have commits"),
            ),
            (
                (true, false, false, 1, 1, To, false),
                Err("both sides have commits"),
            ),
            (
                (true, false, false, 1, 1, From, false),
                Err("both sides have commits"),
            ),
            (
                (true, false, false, 1, 1, Both, true),
                ok(&[PushRemote, RebaseLocal, PushLocal, PullRemote]),
            ),
            (
                (true, false, false, 1, 1, To, true),
                ok(&[PushRemote, RebaseLocal, PushLocal, PullRemote]),
            ),
            (
                (true, false, false, 1, 1, From, true),
                ok(&[PushRemote, RebaseLocal]),
            ),
            (
                (true, true, false, 1, 1, Both, true),
                Err("this checkout has uncommitted"),
            ),
            (
                (true, false, true, 1, 1, Both, true),
                Err("on findl has uncommitted"),
            ),
            (
                (true, false, true, 1, 1, From, true),
                ok(&[PushRemote, RebaseLocal]),
            ),
        ];
        for ((reachable, dl, dr, la, ra, dir, rebase), want) in table {
            let got = plan(&state(reachable, dl, dr, la, ra), dir, rebase, false);
            let label = format!("({reachable}, {dl}, {dr}, {la}, {ra}, {dir:?}, rebase={rebase})");
            match want {
                Ok(s) => assert_eq!(steps(got), s, "{label}"),
                Err(needle) => {
                    let r = refusal(got);
                    assert!(r.contains(needle), "{label}: {r}");
                }
            }
        }
    }

    /// Every combination, checked against the invariants the table rows
    /// express one at a time.
    #[test]
    fn planner_invariants_over_every_combination() {
        use Step::*;
        for reachable in [false, true] {
            for dl in [false, true] {
                for dr in [false, true] {
                    for la in [0, 2] {
                        for ra in [0, 2] {
                            for dir in [Direction::Both, Direction::To, Direction::From] {
                                for rebase in [false, true] {
                                    let label = format!("({reachable}, {dl}, {dr}, {la}, {ra}, {dir:?}, rebase={rebase})");
                                    let got =
                                        plan(&state(reachable, dl, dr, la, ra), dir, rebase, true);
                                    let SyncPlan::Steps(steps) = got else {
                                        assert!(
                                            !reachable
                                                || (la > 0 && ra > 0 && !rebase)
                                                || (dl && (ra > 0))
                                                || (dr
                                                    && la > 0
                                                    && !(dir == Direction::From && ra > 0))
                                                || (la > 0 && ra == 0 && dir == Direction::From)
                                                || (ra > 0 && la == 0 && dir == Direction::To),
                                            "{label}: unexpected refusal"
                                        );
                                        continue;
                                    };
                                    assert!(reachable, "{label}: unreachable must refuse");
                                    // A dirty side never receives commits.
                                    if dl {
                                        assert!(
                                            !steps.contains(&PullLocal)
                                                && !steps.contains(&RebaseLocal),
                                            "{label}: {steps:?}"
                                        );
                                    }
                                    if dr {
                                        assert!(!steps.contains(&PullRemote), "{label}: {steps:?}");
                                    }
                                    // Direction restricts the moves and the context flow.
                                    if dir == Direction::To {
                                        assert!(!steps.contains(&PullLocal), "{label}: {steps:?}");
                                    }
                                    if dir == Direction::From {
                                        for s in [PushLocal, PullRemote, CarryEnv, MirrorSkills] {
                                            assert!(!steps.contains(&s), "{label}: {steps:?}");
                                        }
                                    } else {
                                        assert!(
                                            steps.contains(&MirrorSkills),
                                            "{label}: {steps:?}"
                                        );
                                    }
                                    // Rebase only on request, only when diverged, only here.
                                    assert_eq!(
                                        steps.contains(&RebaseLocal),
                                        rebase && la > 0 && ra > 0,
                                        "{label}: {steps:?}"
                                    );
                                    // Commits move only when someone is ahead.
                                    let moves = steps.iter().any(|s| {
                                        matches!(s, PushLocal | PullRemote | PushRemote | PullLocal)
                                    });
                                    assert_eq!(moves, la > 0 || ra > 0, "{label}: {steps:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn planner_refuses_wrong_branch_and_missing_origin_before_moving() {
        let mut s = state(true, false, false, 1, 0);
        s.remote.as_mut().unwrap().branch = "other".into();
        assert!(refusal(plan(&s, Direction::Both, false, false)).contains("is on other, not main"));
        let mut s = state(true, false, false, 1, 0);
        s.local.has_origin = false;
        assert!(refusal(plan(&s, Direction::Both, false, false)).contains("here has no `origin`"));
        let mut s = state(true, false, false, 1, 0);
        s.remote.as_mut().unwrap().fetch_ok = false;
        assert!(refusal(plan(&s, Direction::Both, false, false)).contains("failed findl"));
    }

    #[test]
    fn planner_carries_context_by_direction() {
        let mut s = state(true, false, false, 0, 0);
        s.env_files = 1;
        s.local.scratch = true;
        s.remote.as_mut().unwrap().scratch = true;
        assert_eq!(
            steps(plan(&s, Direction::Both, false, true)),
            vec![Step::CarryEnv, Step::CarryScratch, Step::MirrorSkills]
        );
        assert_eq!(
            steps(plan(&s, Direction::From, false, true)),
            vec![Step::CarryScratch]
        );
        assert_eq!(
            steps(plan(&s, Direction::Both, false, false)),
            vec![Step::CarryEnv, Step::CarryScratch]
        );
        // Scratch absent on both sides: nothing to carry.
        s.local.scratch = false;
        s.remote.as_mut().unwrap().scratch = false;
        s.env_files = 0;
        assert_eq!(steps(plan(&s, Direction::Both, false, false)), vec![]);
    }

    // ---- end to end: two clones of a bare origin, no ssh ----

    /// The host, played by a second clone: scripts run through `sh -c`
    /// with the clone's own absolute path, env files are copied, `.scratch`
    /// rsyncs locally, and the skills mirror is a no-op line.
    struct LocalHost;

    impl HostRunner for LocalHost {
        fn run(&self, script: &str) -> Result<String> {
            sh(script)
        }
        fn push_env(
            &self,
            local_repo: &Path,
            remote_repo: &Path,
            files: &[PathBuf],
            overwrite: bool,
        ) -> Result<Pushed> {
            let mut p = Pushed::default();
            for f in files {
                let dst = remote_repo.join(f);
                if dst.exists() && !overwrite {
                    p.kept.push(f.clone());
                    continue;
                }
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(local_repo.join(f), dst)?;
                p.sent.push(f.clone());
            }
            Ok(p)
        }
        fn carry_scratch(
            &self,
            local_repo: &Path,
            remote_repo: &Path,
            to_host: bool,
        ) -> Result<()> {
            let l = format!("{}/.scratch/", local_repo.display());
            let r = format!("{}/.scratch/", remote_repo.display());
            if to_host {
                rsync_update(&l, &r, None)
            } else {
                rsync_update(&r, &l, None)
            }
        }
        fn mirror_skills(&self) -> Result<String> {
            Ok("skills: skipped (test runner)".into())
        }
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        git_local(repo, args).unwrap_or_else(|e| panic!("{e}"))
    }

    fn commit(repo: &Path, file: &str, text: &str) -> String {
        std::fs::write(repo.join(file), text).unwrap();
        git(repo, &["add", file]);
        git(repo, &["commit", "-q", "-m", &format!("{file}: {text}")]);
        git(repo, &["rev-parse", "HEAD"])
    }

    /// A bare origin and two clones of it, `laptop` and `host`, with one
    /// pushed commit and `.env*` / `.scratch/` ignored.
    fn two_clones(dir: &tempfile::TempDir) -> (SyncTarget, PathBuf, PathBuf) {
        let origin = dir.path().join("origin.git");
        sh(&format!(
            "git init -q --bare -b main {}",
            ssh_quote(&origin.to_string_lossy())
        ))
        .unwrap();
        let laptop = dir.path().join("laptop");
        let host = dir.path().join("host");
        for clone in [&laptop, &host] {
            sh(&format!(
                "git clone -q {} {}",
                ssh_quote(&origin.to_string_lossy()),
                ssh_quote(&clone.to_string_lossy())
            ))
            .unwrap();
            git(clone, &["config", "user.email", "t@t"]);
            git(clone, &["config", "user.name", "t"]);
        }
        git(&laptop, &["checkout", "-q", "-b", "main"]);
        std::fs::write(laptop.join(".gitignore"), ".env*\n.scratch/\n").unwrap();
        git(&laptop, &["add", ".gitignore"]);
        commit(&laptop, "tracked.txt", "one");
        git(&laptop, &["push", "-q", "-u", "origin", "main"]);
        git(&host, &["fetch", "-q", "origin"]);
        git(&host, &["checkout", "-q", "main"]);
        let target = SyncTarget {
            project: "p".into(),
            host: "findl".into(),
            branch: "main".into(),
            local_path: laptop.clone(),
            remote_path: host.clone(),
            local_sessions: 0,
            remote_sessions: 0,
        };
        (target, laptop, host)
    }

    fn opts() -> SyncOpts {
        SyncOpts {
            direction: Direction::Both,
            named_host: None,
            status: false,
            dry_run: false,
            rebase: false,
            force_env: false,
            skills: true,
            project: None,
        }
    }

    fn run_sync(t: &SyncTarget, o: &SyncOpts) -> Result<String> {
        let mut out = Vec::new();
        sync_target(t, o, &LocalHost, &mut out).map(|()| String::from_utf8(out).unwrap())
    }

    fn head(repo: &Path) -> String {
        git(repo, &["rev-parse", "HEAD"])
    }

    #[test]
    fn local_ahead_fast_forwards_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let (t, laptop, host) = two_clones(&dir);
        let new = commit(&laptop, "tracked.txt", "two");
        let out = run_sync(&t, &opts()).unwrap();
        assert_eq!(head(&host), new, "{out}");
        assert!(out.contains("push here → origin: main"), "{out}");
        assert!(out.contains("git findl:"), "{out}");
        assert!(out.contains("skills: skipped"), "{out}");
        // Nothing left to do: in step.
        let out = run_sync(&t, &opts()).unwrap();
        assert!(out.contains("in step"), "{out}");
    }

    #[test]
    fn host_ahead_fast_forwards_local() {
        let dir = tempfile::tempdir().unwrap();
        let (t, laptop, host) = two_clones(&dir);
        let new = commit(&host, "tracked.txt", "from host");
        let out = run_sync(&t, &opts()).unwrap();
        assert_eq!(head(&laptop), new, "{out}");
        assert!(out.contains("push findl → origin: main"), "{out}");
        assert!(out.contains("git here:"), "{out}");
        // --to on a host-ahead pair refuses and points the other way.
        commit(&host, "tracked.txt", "again");
        let err = run_sync(
            &t,
            &SyncOpts {
                direction: Direction::To,
                ..opts()
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("--to would need the other direction"),
            "{err}"
        );
        assert!(
            err.to_string().contains("nebula sync --from findl"),
            "{err}"
        );
    }

    #[test]
    fn diverged_refuses_then_rebases_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let (t, laptop, host) = two_clones(&dir);
        commit(&laptop, "a.txt", "laptop");
        let host_commit = commit(&host, "b.txt", "host");
        let err = run_sync(&t, &opts()).unwrap_err().to_string();
        assert!(
            err.contains("refused: both sides have commits the other lacks: here +1, findl +1"),
            "{err}"
        );
        assert!(err.contains("--rebase"), "{err}");
        // Untouched on refusal.
        assert_eq!(head(&host), host_commit);
        assert!(!laptop.join("b.txt").exists());

        let out = run_sync(
            &t,
            &SyncOpts {
                rebase: true,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(head(&laptop), head(&host), "{out}");
        // The host's commit is the base, the laptop's on top: host history kept.
        let log = git(&laptop, &["log", "--format=%s", "-3"]);
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            vec!["a.txt: laptop", "b.txt: host", "tracked.txt: one"]
        );
        assert!(host.join("a.txt").exists() && laptop.join("b.txt").exists());
    }

    #[test]
    fn dirty_host_refuses_by_name_and_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (t, laptop, host) = two_clones(&dir);
        let before = head(&host);
        std::fs::write(host.join("tracked.txt"), "edited on host\n").unwrap();
        commit(&laptop, "tracked.txt", "two");
        let err = run_sync(&t, &opts()).unwrap_err().to_string();
        assert!(
            err.contains("refused: the checkout on findl has uncommitted changes"),
            "{err}"
        );
        assert!(err.contains("nebula agent send"), "{err}");
        assert_eq!(head(&host), before);
        // Untracked files never count as dirty.
        std::fs::write(host.join("tracked.txt"), "one").unwrap();
        std::fs::write(host.join("stray.txt"), "x").unwrap();
        run_sync(&t, &opts()).unwrap();
        assert_eq!(head(&host), head(&laptop));
    }

    #[test]
    fn scratch_newer_file_wins_each_way_and_nothing_is_deleted() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let (t, laptop, host) = two_clones(&dir);
        std::fs::create_dir_all(laptop.join(".scratch")).unwrap();
        std::fs::create_dir_all(host.join(".scratch")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(600);
        let stamp = |p: &Path, when: SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(when)
                .unwrap()
        };
        // Same name on both sides: laptop's is newer.
        std::fs::write(laptop.join(".scratch/spec.md"), "laptop newer").unwrap();
        std::fs::write(host.join(".scratch/spec.md"), "host older").unwrap();
        stamp(&host.join(".scratch/spec.md"), old);
        // Same name, host's is newer.
        std::fs::write(laptop.join(".scratch/notes.md"), "laptop older").unwrap();
        stamp(&laptop.join(".scratch/notes.md"), old);
        std::fs::write(host.join(".scratch/notes.md"), "host newer").unwrap();
        // One side only.
        std::fs::write(laptop.join(".scratch/only-laptop.md"), "l").unwrap();
        std::fs::write(host.join(".scratch/only-host.md"), "h").unwrap();

        let out = run_sync(&t, &opts()).unwrap();
        assert!(
            out.contains("scratch → findl") && out.contains("scratch ← findl"),
            "{out}"
        );
        let read = |p: &Path| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(&host.join(".scratch/spec.md")), "laptop newer");
        assert_eq!(read(&laptop.join(".scratch/notes.md")), "host newer");
        assert_eq!(read(&host.join(".scratch/only-laptop.md")), "l");
        assert_eq!(read(&laptop.join(".scratch/only-host.md")), "h");
        // --from carries only host → laptop.
        std::fs::write(laptop.join(".scratch/later.md"), "l2").unwrap();
        run_sync(
            &t,
            &SyncOpts {
                direction: Direction::From,
                ..opts()
            },
        )
        .unwrap();
        assert!(!host.join(".scratch/later.md").exists());
    }

    #[test]
    fn env_files_are_kept_on_the_host_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (t, laptop, host) = two_clones(&dir);
        std::fs::write(laptop.join(".env"), "LOCAL=1").unwrap();
        std::fs::write(laptop.join(".env.local"), "L2=1").unwrap();
        std::fs::write(host.join(".env"), "HOST=1").unwrap();
        let out = run_sync(&t, &opts()).unwrap();
        assert!(out.contains("env → findl: sent .env.local"), "{out}");
        assert!(
            out.contains("env: kept on findl") && out.contains(".env"),
            "{out}"
        );
        let read = |p: &Path| std::fs::read_to_string(p).unwrap();
        assert_eq!(read(&host.join(".env")), "HOST=1");
        assert_eq!(read(&host.join(".env.local")), "L2=1");
        run_sync(
            &t,
            &SyncOpts {
                force_env: true,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(read(&host.join(".env")), "LOCAL=1");
        // --from never sends env to the host.
        std::fs::write(laptop.join(".env"), "CHANGED=1").unwrap();
        run_sync(
            &t,
            &SyncOpts {
                direction: Direction::From,
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(read(&host.join(".env")), "LOCAL=1");
    }

    #[test]
    fn dry_run_prints_the_plan_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (t, laptop, host) = two_clones(&dir);
        let before = head(&host);
        commit(&laptop, "tracked.txt", "two");
        std::fs::write(laptop.join(".env"), "X=1").unwrap();
        let out = run_sync(
            &t,
            &SyncOpts {
                dry_run: true,
                ..opts()
            },
        )
        .unwrap();
        assert!(out.starts_with("plan (4 steps, nothing run):"), "{out}");
        assert!(out.contains("push here → origin: main"), "{out}");
        assert!(
            out.contains("fast-forward findl: main from origin"),
            "{out}"
        );
        assert!(out.contains("env → findl"), "{out}");
        assert!(out.contains("skills → findl"), "{out}");
        assert_eq!(head(&host), before);
        assert!(!host.join(".env").exists());
        // A dry run of a refusal reports it without failing.
        commit(&host, "b.txt", "host");
        let out = run_sync(
            &t,
            &SyncOpts {
                dry_run: true,
                ..opts()
            },
        )
        .unwrap();
        assert!(out.starts_with("would refuse: both sides"), "{out}");
    }

    #[test]
    fn status_is_json_with_both_sides_and_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let (mut t, laptop, host) = two_clones(&dir);
        t.local_sessions = 1;
        t.remote_sessions = 2;
        let new = commit(&laptop, "tracked.txt", "two");
        let out = run_sync(
            &t,
            &SyncOpts {
                status: true,
                ..opts()
            },
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["project"], "p");
        assert_eq!(v["branch"], "main");
        assert_eq!(v["host"], "findl");
        assert_eq!(v["local"]["head"], new);
        assert_eq!(v["local"]["dirty"], false);
        assert_eq!(v["local"]["ahead"], 1);
        assert_eq!(v["local"]["sessions"], 1);
        assert_eq!(v["remote"]["path"], host.to_str().unwrap());
        assert_eq!(v["remote"]["head"], head(&host));
        assert_eq!(v["remote"]["ahead"], 0);
        assert_eq!(v["remote"]["sessions"], 2);
        assert_eq!(v["remote"]["reachable"], true);
        assert_eq!(
            v["plan"]["steps"],
            serde_json::json!(["push_local", "pull_remote", "mirror_skills"])
        );
        assert_eq!(
            head(&host),
            git(&host, &["rev-parse", "HEAD"]),
            "status moved nothing"
        );

        // Unreachable host: still JSON, still exit 0, plan is the refusal.
        struct Down;
        impl HostRunner for Down {
            fn run(&self, _: &str) -> Result<String> {
                bail!("ssh findl: connect timed out")
            }
            fn push_env(&self, _: &Path, _: &Path, _: &[PathBuf], _: bool) -> Result<Pushed> {
                unreachable!()
            }
            fn carry_scratch(&self, _: &Path, _: &Path, _: bool) -> Result<()> {
                unreachable!()
            }
            fn mirror_skills(&self) -> Result<String> {
                unreachable!()
            }
        }
        let mut out = Vec::new();
        sync_target(
            &t,
            &SyncOpts {
                status: true,
                ..opts()
            },
            &Down,
            &mut out,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["remote"]["reachable"], false);
        assert!(v["remote"]["head"].is_null());
        assert!(
            v["plan"]["refusal"]["reason"]
                .as_str()
                .unwrap()
                .contains("unreachable"),
            "{v}"
        );
        // And a bare sync against it is a refusal.
        let err = sync_target(&t, &opts(), &Down, &mut Vec::new())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("refused: findl is unreachable: ssh findl: connect timed out"),
            "{err}"
        );
    }

    #[test]
    fn missing_host_checkout_is_a_probe_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let (mut t, _laptop, _host) = two_clones(&dir);
        t.remote_path = dir.path().join("nowhere");
        let err = run_sync(&t, &opts()).unwrap_err().to_string();
        assert!(err.contains("unreachable: no such checkout"), "{err}");
    }
}
