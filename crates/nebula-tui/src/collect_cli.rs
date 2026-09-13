//! Remote handoff orchestration. Checkout identity is host + absolute path;
//! branches select commits, never identify or replace daemon rows.
use crate::ipc::{handshake, try_connect, Connection};
use crate::sync_cli::{self, git_local as git, HostRunner, SshHost, SyncTarget};
use anyhow::{bail, Context, Result};
use nebula_core::{
    codec::{read_frame, write_frame},
    remote::ssh_quote,
    Agent, AgentStatus, ClientRequest, Project, ServerEvent, TerminalTab, Worktree,
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Explicit destination semantics; branch collection uses the primary directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Branch,
    Worktree,
}

/// CLI inputs. Every safety-relevant choice has an explicit value.
pub struct CollectOpts {
    /// Remote branch, checkout id, directory name, or absolute checkout path.
    pub selector: String,
    /// Destination mode. Omission infers primary vs linked checkout from the source.
    pub mode: Option<Mode>,
    /// Local project; otherwise caller session or cwd.
    pub project: Option<String>,
    /// Restrict twin selection to this configured host.
    pub host: Option<String>,
    /// Owning agent id or unique name; never overrides multiple live writers.
    pub agent: Option<String>,
    /// Inspect only; no prompts, fetches, directories, or registrations.
    pub dry_run: bool,
    /// Maximum time for a requested handoff turn.
    pub timeout: Duration,
}

/// Stable checkout identity in the JSON report, even before daemon discovery.
#[derive(Clone, Debug, Serialize)]
pub struct Identity {
    /// Machine owning the path (`local` for the laptop).
    pub host: String,
    /// Absolute checkout directory on that machine.
    pub path: PathBuf,
    /// Existing daemon identifier, if already discovered.
    pub checkout_id: Option<String>,
    /// Probed commit, not a cached branch label.
    pub sha: Option<String>,
}

/// A collect attempt always emits this report, including on refusal.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    /// Whether execution was disabled.
    pub dry_run: bool,
    /// Resolved branch name.
    pub branch: Option<String>,
    /// Resolved destination semantics.
    pub mode: Option<Mode>,
    /// Remote identity.
    pub source: Option<Identity>,
    /// Laptop identity.
    pub destination: Option<Identity>,
    /// Planned or completed actions in order.
    pub steps: Vec<String>,
    /// Failure and the safe next action, if collection did not complete.
    pub refusal: Option<String>,
}

/// Current daemon ownership map, including shell sessions.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// Local and mirrored projects.
    pub projects: Vec<Project>,
    /// Host-owned checkout rows.
    pub worktrees: Vec<Worktree>,
    /// Agent ownership and current status.
    pub agents: Vec<Agent>,
    /// Shell sessions that may also write to a checkout.
    pub terminals: Vec<TerminalTab>,
}

/// Daemon boundary for collection. Implementations must retain status events
/// during steering and require a fresh post-request completion.
#[allow(async_fn_in_trait)]
pub trait Sessions {
    /// Read the current ownership map without starting a daemon.
    async fn snapshot(&mut self) -> Result<Snapshot>;
    /// Validate current ownership on the source daemon, bypassing relay caches.
    async fn inspect(&mut self, worktree: &Worktree) -> Result<()>;
    /// Safely submit to a finished owner, then wait for a new running/finished
    /// turn. Feedback, disconnect, timeout, or ambiguous completion must fail.
    async fn handoff(&mut self, agent: &Agent, prompt: &str, timeout: Duration) -> Result<()>;
}
/// Adapter for an established daemon connection; relays route guarded RPCs
/// to the source daemon and correlate their responses.
pub struct DaemonSessions {
    conn: Connection,
}
impl DaemonSessions {
    /// Use an already handshaken connection without spawning a daemon.
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }
}
impl Sessions for DaemonSessions {
    async fn snapshot(&mut self) -> Result<Snapshot> {
        write_frame(&mut self.conn.stream, &ClientRequest::Subscribe).await?;
        loop {
            match read_frame::<ServerEvent, _>(&mut self.conn.stream).await? {
                Some(ServerEvent::Snapshot {
                    projects,
                    worktrees,
                    agents,
                    terminals,
                    ..
                }) => {
                    return Ok(Snapshot {
                        projects,
                        worktrees,
                        agents,
                        terminals,
                    })
                }
                Some(_) => {}
                None => bail!("daemon disconnected; retry after reconnect"),
            }
        }
    }
    async fn inspect(&mut self, worktree: &Worktree) -> Result<()> {
        write_frame(
            &mut self.conn.stream,
            &ClientRequest::InspectCollect {
                req_id: 2,
                worktree: worktree.id.clone(),
                branch: worktree.branch.clone(),
            },
        )
        .await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match read_frame::<ServerEvent, _>(&mut self.conn.stream).await?.context("source ownership check disconnected; update daemons when sessions can safely stop")? {
                    ServerEvent::Ack { req_id: 2, .. } => return Ok(()),
                    ServerEvent::Error { req_id: Some(2), message } => bail!("{message}"),
                    _ => {},
                }
            }
        }).await.context("source ownership check timed out; inspect host connectivity")?
    }
    async fn handoff(&mut self, agent: &Agent, prompt: &str, timeout: Duration) -> Result<()> {
        let request = 1;
        write_frame(
            &mut self.conn.stream,
            &ClientRequest::CollectHandoff {
                req_id: request,
                id: agent.id.clone(),
                worktree: agent.worktree_id.clone(),
                branch: agent.branch.clone().context(
                    "owner has no recorded branch; establish ownership before collecting",
                )?,
                expected_status_changed_at: agent.status_changed_at,
                prompt: prompt.to_owned(),
                timeout_ms: timeout
                    .as_millis()
                    .try_into()
                    .context("timeout is too large")?,
            },
        )
        .await?;
        tokio::time::timeout(timeout + Duration::from_secs(5), async {
            loop {
                match read_frame::<ServerEvent, _>(&mut self.conn.stream).await?
                    .context("daemon disconnected before guarded handoff completion; update both daemons when sessions can safely stop")? {
                    ServerEvent::Ack { req_id, .. } if req_id == request => return Ok(()),
                    ServerEvent::Error { req_id: Some(req_id), message } if req_id == request => bail!("{message}"),
                    _ => {}, // In-flight mirrored status events cannot acknowledge this request.
                }
            }
        }).await.context("handoff response timed out; inspect the source owner before retrying")?
    }
}

fn remote_git(host: &dyn HostRunner, path: &Path, args: &[&str]) -> Result<String> {
    host.run(&format!(
        "GIT_OPTIONAL_LOCKS=0 git -C {} {}",
        ssh_quote(&path.to_string_lossy()),
        args.iter()
            .map(|a| ssh_quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    ))
    .map(|s| s.trim().into())
}
fn clean(path: &Path) -> Result<()> {
    if !git(path, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        bail!(
            "{} has uncommitted or untracked files; finish explicit-file commits before retrying",
            path.display()
        );
    }
    Ok(())
}
fn one<T>(mut values: Vec<T>, what: &str) -> Result<T> {
    if values.len() != 1 {
        bail!(
            "expected one {what}, found {}; select an explicit project/host/checkout id",
            values.len()
        );
    }
    Ok(values.remove(0))
}
struct Target {
    local: Project,
    remote: Worktree,
    host: String,
    mode: Mode,
    path: PathBuf,
}
fn resolve(s: &Snapshot, o: &CollectOpts) -> Result<Target> {
    let cwd = std::env::current_dir()?;
    let caller = std::env::var("NEBULA_AGENT_ID")
        .ok()
        .and_then(|id| s.agents.iter().find(|a| a.id.as_str() == id))
        .and_then(|a| s.worktrees.iter().find(|w| w.id == a.worktree_id));
    let local = if let Some(name) = &o.project {
        one(
            s.projects
                .iter()
                .filter(|p| p.host.is_none() && (&p.name == name || p.id.as_str() == name))
                .collect(),
            "local project",
        )?
    } else {
        let w = caller
            .or_else(|| {
                s.worktrees
                    .iter()
                    .filter(|w| {
                        cwd.starts_with(&w.path)
                            && s.projects
                                .iter()
                                .any(|p| p.id == w.project_id && p.host.is_none())
                    })
                    .max_by_key(|w| w.path.components().count())
            })
            .context("no local checkout resolved; pass --project <name>")?;
        one(
            s.projects
                .iter()
                .filter(|p| p.id == w.project_id && p.host.is_none())
                .collect(),
            "local project",
        )?
    };
    let mut local = local.clone();
    local.repo_path = local
        .repo_path
        .canonicalize()
        .context("local primary path is unavailable")?;
    let twin = one(
        s.projects
            .iter()
            .filter(|p| {
                p.name == local.name
                    && p.workspace_id == local.workspace_id
                    && p.host.is_some()
                    && o.host.as_ref().is_none_or(|h| p.host.as_ref() == Some(h))
            })
            .collect(),
        "configured remote twin",
    )?;
    let selector = o.selector.as_str();
    let remote = one(
        s.worktrees
            .iter()
            .filter(|w| {
                w.project_id == twin.id
                    && (w.id.as_str() == selector
                        || w.path.to_str() == Some(selector)
                        || w.branch == selector
                        || w.path.file_name().and_then(|n| n.to_str()) == Some(selector)
                        || (matches!(selector, "root" | "primary") && w.is_main))
            })
            .collect(),
        "remote checkout",
    )?;
    git(
        &local.repo_path,
        &["check-ref-format", "--branch", &remote.branch],
    )?;
    if remote.branch == "HEAD" || !remote.path.is_absolute() {
        bail!("remote source must be an absolute checkout on a named branch");
    }
    let mode = o.mode.unwrap_or(if remote.is_main {
        Mode::Branch
    } else {
        Mode::Worktree
    });
    // Read git's actual checkout map; the daemon may not have polled yet.
    let checkouts = git(
        &local.repo_path,
        &[
            "-c",
            "core.quotePath=false",
            "worktree",
            "list",
            "--porcelain",
        ],
    )?;
    let mut held = None;
    let mut path = None;
    for line in checkouts.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if p.starts_with('"') {
                bail!("quoted/control-character checkout paths require explicit path cleanup before collection");
            }
            path = Some(PathBuf::from(p));
        }
        if line == format!("branch refs/heads/{}", remote.branch) {
            held = path.clone();
        }
    }
    let destination = match (mode, held) {
        (Mode::Branch, Some(p)) if p != local.repo_path => bail!(
            "branch is parked in {}; use --worktree or resolve the parked checkout explicitly",
            p.display()
        ),
        (Mode::Branch, _) => local.repo_path.clone(),
        (Mode::Worktree, Some(p)) if p == local.repo_path => {
            bail!("branch is held by the primary; use --branch or choose a different branch")
        }
        (Mode::Worktree, Some(p)) => p,
        (Mode::Worktree, None) => {
            let name = local
                .repo_path
                .file_name()
                .context("project path has no directory name")?
                .to_string_lossy();
            // Existing Nebula convention: sibling <repo>-worktrees/<branch>.
            let p = local
                .repo_path
                .parent()
                .context("project has no parent")?
                .join(format!("{name}-worktrees"))
                .join(&remote.branch);
            if p.exists() || p.symlink_metadata().is_ok() {
                bail!(
                    "destination {} already exists without this branch; inspect it before retrying",
                    p.display()
                );
            }
            p
        }
    };
    Ok(Target {
        local: local.clone(),
        remote: remote.clone(),
        host: twin.host.clone().unwrap(),
        mode,
        path: destination,
    })
}

fn check_sessions<'a>(s: &'a Snapshot, t: &Target, o: &CollectOpts) -> Result<Option<&'a Agent>> {
    let branch = &t.remote.branch;
    let local_ids: Vec<_> = s
        .worktrees
        .iter()
        .filter(|w| {
            w.project_id == t.local.id
                && (w.path.canonicalize().is_ok_and(|p| p == t.path) || w.branch == *branch)
        })
        .map(|w| &w.id)
        .collect();
    let local_branch_owner = |id: &nebula_core::WorktreeId, remembered: Option<&str>| {
        local_ids.contains(&id)
            || (remembered == Some(branch.as_str())
                && s.worktrees
                    .iter()
                    .any(|w| &w.id == id && w.project_id == t.local.id))
    };
    if s.agents
        .iter()
        .any(|a| a.alive && !a.archived && local_branch_owner(&a.worktree_id, a.branch.as_deref()))
        || s.terminals
            .iter()
            .any(|a| local_branch_owner(&a.worktree_id, a.branch.as_deref()))
    {
        bail!("local destination/branch has sessions; leave them in place and collect into a session-free destination");
    }
    let other_branch_owner = |id: &nebula_core::WorktreeId, remembered: Option<&str>| {
        s.worktrees.iter().any(|w| {
            &w.id == id
                && w.id != t.remote.id
                && (w.branch == *branch || remembered == Some(branch.as_str()))
                && s.projects.iter().any(|p| {
                    p.id == w.project_id
                        && p.name == t.local.name
                        && p.workspace_id == t.local.workspace_id
                })
        })
    };
    if s.agents
        .iter()
        .any(|a| a.alive && !a.archived && other_branch_owner(&a.worktree_id, a.branch.as_deref()))
        || s.terminals
            .iter()
            .any(|t| other_branch_owner(&t.worktree_id, t.branch.as_deref()))
    {
        bail!("another checkout or host has sessions on this branch; settle all writers before collecting");
    }
    if s.terminals.iter().any(|a| a.worktree_id == t.remote.id) {
        bail!("remote checkout has shell sessions; ownership is ambiguous, settle them explicitly first");
    }
    let candidates: Vec<_> = s
        .agents
        .iter()
        .filter(|a| a.worktree_id == t.remote.id && !a.archived && a.alive)
        .collect();
    if candidates.len() > 1 {
        bail!(
            "multiple remote writers share the source checkout; settle ownership before collecting"
        );
    }
    let owner = candidates.first().copied();
    if let Some(a) = owner {
        if a.branch.as_ref().is_some_and(|b| b != branch) {
            bail!("remote session belongs to a parked branch; resolve its ownership first");
        }
        if a.status != AgentStatus::Finished {
            bail!(
                "remote owner {} is {}; wait for it to finish or resolve feedback, then retry",
                a.name,
                a.status.as_str()
            );
        }
    }
    if let Some(sel) = &o.agent {
        if !owner.is_some_and(|a| a.id.as_str() == sel || &a.name == sel) {
            bail!("selected agent is not the sole finished live owner of this checkout");
        }
    }
    Ok(owner)
}

// Seed only local ignored env, using the same discovery as normal worktree
// creation. Keep existing destinations and refuse symlink traversal.
fn seed_local_env(primary: &Path, destination: &Path) -> Result<()> {
    for rel in nebula_core::envfiles::list_local(primary)? {
        let mut to = destination.to_path_buf();
        for component in rel.components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                bail!("invalid local env path");
            }
            to.push(component);
            if to
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                bail!("env destination traverses a symlink: {}", to.display());
            }
        }
        if to.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)?;
        let mut input = std::fs::File::open(primary.join(rel))?;
        std::io::copy(&mut input, &mut output)?;
        output.set_permissions(input.metadata()?.permissions())?;
    }
    Ok(())
}

/// Collect through the real git/sync path with injected SSH and daemon
/// boundaries. The report retains partial progress when an operation refuses.
pub async fn collect(
    o: &CollectOpts,
    sessions: &mut impl Sessions,
    host: &dyn HostRunner,
    r: &mut Report,
) -> Result<()> {
    let s = sessions.snapshot().await?;
    let t = resolve(&s, o)?;
    let b = &t.remote.branch;
    r.branch = Some(b.clone());
    r.mode = Some(t.mode);
    r.source = Some(Identity {
        host: t.host.clone(),
        path: t.remote.path.clone(),
        checkout_id: Some(t.remote.id.to_string()),
        sha: None,
    });
    r.destination = Some(Identity {
        host: "local".into(),
        path: t.path.clone(),
        checkout_id: s
            .worktrees
            .iter()
            .find(|w| {
                w.project_id == t.local.id && w.path.canonicalize().is_ok_and(|p| p == t.path)
            })
            .map(|w| w.id.to_string()),
        sha: None,
    });
    let owner = check_sessions(&s, &t, o)?.cloned();
    sessions.inspect(&t.remote).await?;
    let actual = remote_git(host, &t.remote.path, &["symbolic-ref", "--short", "HEAD"])?;
    if actual != *b {
        bail!("source changed branch to {actual}; refresh the checkout selection and retry");
    }
    let mut sha = remote_git(host, &t.remote.path, &["rev-parse", "HEAD"])?;
    r.source.as_mut().unwrap().sha = Some(sha.clone());
    let existing = t.path.exists();
    if existing {
        clean(&t.path)?;
        r.destination.as_mut().unwrap().sha = Some(git(&t.path, &["rev-parse", "HEAD"])?);
    }
    if t.mode == Mode::Branch {
        clean(&t.local.repo_path)?;
    }
    let target_ref = format!("refs/heads/{b}");
    let local_ref = git(&t.local.repo_path, &["rev-parse", "--verify", &target_ref]).ok();
    if let Some(ref head) = local_ref {
        // Remote must contain local history before pushing or creating anything.
        if remote_git(
            host,
            &t.remote.path,
            &["merge-base", "--is-ancestor", head, &sha],
        )
        .is_err()
        {
            bail!("local branch is ahead, diverged, or unknown to remote; review history explicitly before retrying");
        }
    }
    git(&t.local.repo_path, &["remote", "get-url", "origin"])
        .context("local checkout needs an origin remote before collect")?;
    remote_git(host, &t.remote.path, &["remote", "get-url", "origin"])
        .context("source checkout needs an origin remote before collect")?;
    let dirty = !remote_git(
        host,
        &t.remote.path,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .is_empty();
    if dirty {
        let a = owner.context("remote checkout is dirty without a sole finished live owner; ask the owning agent to finish explicit-file commits first")?;
        r.steps.push(format!(
            "request explicit-file commit and stop editing from {}",
            a.id
        ));
        r.steps
            .push("wait for a new running → finished handoff turn".into());
        if o.dry_run {
            r.steps.push("after handoff: revalidate cleanliness, ownership, and ancestry; create/select local destination; sync commits through origin".into());
            return Ok(());
        }
        let prompt = format!("Collect handoff for branch {b} in {}. Commit ONLY your own explicitly named files belonging to this task. Preserve all other agents' changes; never stage everything. If ownership is unclear or unrelated dirty files remain, report the blocker. After committing, stop editing this branch and checkout on all machines and finish this turn. Do not sync, switch, delete, restart, or archive anything. The local orchestrator will collect through origin.", t.remote.path.display());
        sessions.handoff(&a, &prompt, o.timeout).await?;
        let fresh = sessions.snapshot().await?;
        check_sessions(&fresh, &t, o)?;
        if !remote_git(
            host,
            &t.remote.path,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
        {
            bail!("remote files remain dirty after handoff; ask the owner to resolve explicit files, then retry");
        }
        sha = remote_git(host, &t.remote.path, &["rev-parse", "HEAD"])?;
        r.source.as_mut().unwrap().sha = Some(sha.clone());
    } else {
        r.steps
            .push("source clean and settled; no commit request needed".into());
    }
    if o.dry_run {
        r.steps.push(
            if existing {
                "validate ancestry and fast-forward local destination"
            } else {
                "bootstrap local worktree after origin validation"
            }
            .into(),
        );
        r.steps
            .push("sync from remote through origin (commits only); verify equal heads".into());
        return Ok(());
    }
    // Recheck ownership and paths immediately before any git mutation.
    check_sessions(&sessions.snapshot().await?, &t, o)?;
    sessions.inspect(&t.remote).await?;
    if remote_git(host, &t.remote.path, &["symbolic-ref", "--short", "HEAD"])? != *b
        || remote_git(host, &t.remote.path, &["rev-parse", "HEAD"])? != sha
        || !remote_git(
            host,
            &t.remote.path,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty()
    {
        bail!("source changed during collection; inspect ownership and retry");
    }
    if existing {
        clean(&t.path)?;
        if r.destination.as_ref().and_then(|d| d.sha.as_ref())
            != Some(&git(&t.path, &["rev-parse", "HEAD"])?)
        {
            bail!("local HEAD changed during collection; inspect ownership and retry");
        }
    }
    if git(&t.local.repo_path, &["rev-parse", "--verify", &target_ref]).ok() != local_ref {
        bail!("local branch changed during collection; inspect ownership and retry");
    }
    let target = SyncTarget {
        project: t.local.name.clone(),
        host: t.host.clone(),
        branch: b.clone(),
        local_path: t.path.clone(),
        remote_path: t.remote.path.clone(),
        local_sessions: 0,
        remote_sessions: 0,
    };
    let needs_checkout = !existing || git(&t.path, &["symbolic-ref", "--short", "HEAD"])? != *b;
    if needs_checkout {
        // Reuse sync's push primitive for the initial branch too. No remote
        // context/scripts are bootstrapped on the laptop.
        r.steps
            .push("push remote branch through its origin for initial checkout".into());
        sync_cli::push_remote(&target, host)?;
        git(&t.local.repo_path, &["fetch", "-q", "origin"])?;
        let origin = format!("refs/remotes/origin/{b}");
        if git(&t.local.repo_path, &["rev-parse", &origin])? != sha {
            bail!("origin does not match source SHA; retry after inspecting origin");
        }
        match t.mode {
            Mode::Branch => {
                clean(&t.local.repo_path)?;
                if local_ref.is_some() {
                    git(&t.path, &["switch", b])?;
                } else {
                    git(&t.path, &["switch", "-c", b, &origin])?;
                }
            }
            Mode::Worktree => {
                if local_ref.is_some() {
                    git(
                        &t.local.repo_path,
                        &[
                            "worktree",
                            "add",
                            t.path.to_str().context("non UTF-8 path")?,
                            b,
                        ],
                    )?;
                } else {
                    git(
                        &t.local.repo_path,
                        &[
                            "worktree",
                            "add",
                            "-b",
                            b,
                            t.path.to_str().context("non UTF-8 path")?,
                            &origin,
                        ],
                    )?;
                }
            }
        }
        r.steps
            .push("created/selected local destination without running bootstrap scripts".into());
    }
    // Commit-only handoff: .env and untracked remote context never cross back.
    r.steps
        .push("sync committed history through origin with fast-forward-only validation".into());
    sync_cli::sync_commits(&target, host)?;
    if !existing && t.mode == Mode::Worktree {
        seed_local_env(&t.local.repo_path, &t.path)?;
        r.steps.push(
            "seeded missing env files from local primary only; dependencies require project setup"
                .into(),
        );
    }
    let head = git(&t.path, &["rev-parse", "HEAD"])?;
    r.destination.as_mut().unwrap().sha = Some(head.clone());
    if head != sha || remote_git(host, &t.remote.path, &["rev-parse", "HEAD"])? != sha {
        bail!("heads changed during sync; inspect ownership before retrying");
    }
    r.steps
        .push("synced through origin; local and remote SHAs verified equal".into());
    Ok(())
}

/// Native CLI entry point. No daemon start/restart or session lifecycle actions.
pub async fn run(o: CollectOpts) -> Result<()> {
    let mut r = Report {
        dry_run: o.dry_run,
        ..Default::default()
    };
    let result = async {
        let stream = try_connect(&nebula_core::paths::socket_path())
            .await
            .context("no running daemon; connect to the existing daemon before collecting")?;
        let mut daemon = DaemonSessions::new(handshake(stream).await?);
        let s = daemon.snapshot().await?;
        let t = resolve(&s, &o)?;
        collect(&o, &mut daemon, &SshHost { host: t.host }, &mut r).await
    }
    .await;
    if let Err(e) = &result {
        r.refusal = Some(format!("{e:#}"));
    }
    println!("{}", serde_json::to_string_pretty(&r)?);
    result
}

#[cfg(test)]
#[path = "collect_tests.rs"]
mod tests;
