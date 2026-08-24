//! Client-side IPC: connect to the daemon (auto-spawning it when absent) and
//! perform the version handshake.

use anyhow::{bail, Context, Result};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{paths, AgentId, ClientRequest, ServerEvent, PROTOCOL_VERSION};
use std::time::Duration;
use tokio::net::UnixStream;

pub struct Connection {
    pub stream: UnixStream,
    pub daemon_pid: u32,
}

/// Connect, auto-spawning `current_exe() daemon` when nothing is listening.
pub async fn connect_or_spawn() -> Result<Connection> {
    let sock = paths::socket_path();

    if let Ok(conn) = try_connect(&sock).await {
        return handshake(conn).await;
    }

    spawn_daemon()?;

    // Poll-connect while the daemon boots.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match try_connect(&sock).await {
            Ok(conn) => return handshake(conn).await,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "daemon did not come up on {} — check {}",
                        sock.display(),
                        paths::daemon_log_path().display()
                    )
                })
            }
        }
    }
}

async fn try_connect(sock: &std::path::Path) -> Result<UnixStream> {
    Ok(UnixStream::connect(sock).await?)
}

fn spawn_daemon() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().context("resolve current_exe")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // New *session*, not just a new process group: besides outliving this
    // client and skipping its terminal signals (Ctrl+C etc.), the daemon must
    // hold no controlling terminal. It shells out to the user's interactive
    // shell (CLI probes, login-shell agent wrap), and an interactive zsh that
    // can reach a tty via /dev/tty grabs its foreground process group —
    // SIGTTIN-stopping the TUI running on this terminal mid-frame.
    unsafe {
        cmd.pre_exec(|| {
            if libc_setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("spawn nebula daemon")?;
    Ok(())
}

// Avoid a libc dependency for one call (same pattern as nebula-core's geteuid).
fn libc_setsid() -> i32 {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe { setsid() }
}

async fn handshake(mut stream: UnixStream) -> Result<Connection> {
    write_frame(
        &mut stream,
        &ClientRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    match read_frame::<ServerEvent, _>(&mut stream).await? {
        Some(ServerEvent::HelloOk { daemon_pid, .. }) => Ok(Connection { stream, daemon_pid }),
        Some(ServerEvent::Incompatible {
            daemon_protocol_version,
        }) => bail!(
            "daemon speaks protocol v{daemon_protocol_version}, this client v{PROTOCOL_VERSION} — \
             run `nebula kill` and relaunch"
        ),
        other => bail!("unexpected handshake reply: {other:?}"),
    }
}

/// Channel-based IPC handle for the TUI event loop: outbound requests go
/// through `tx`; inbound events arrive on `rx`. Reader/writer tasks own the
/// socket halves.
pub struct IpcChannels {
    pub tx: tokio::sync::mpsc::Sender<ClientRequest>,
    pub rx: tokio::sync::mpsc::Receiver<ServerEvent>,
}

pub fn split_connection(conn: Connection) -> IpcChannels {
    let (read_half, mut write_half) = conn.stream.into_split();
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<ServerEvent>(1024);
    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<ClientRequest>(256);

    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(read_half);
        while let Ok(Some(ev)) = read_frame::<ServerEvent, _>(&mut reader).await {
            if event_tx.send(ev).await.is_err() {
                break;
            }
        }
        // Dropping event_tx closes the channel, signalling disconnect.
    });

    tokio::spawn(async move {
        while let Some(req) = req_rx.recv().await {
            if write_frame(&mut write_half, &req).await.is_err() {
                break;
            }
        }
    });

    IpcChannels {
        tx: req_tx,
        rx: event_rx,
    }
}

/// One-shot client for `nebula rename`, run from inside an agent session's
/// CLI: resolve the agent from NEBULA_AGENT_ID and ask the daemon to title
/// it. Never spawns a daemon — no daemon means no session worth titling.
///
/// Daemon-reported outcomes (renamed, or "already titled" on the non-force
/// path) both print and exit 0: for the model running this, a declined
/// auto-title is a settled answer, not a failure to retry.
pub async fn rename_current_agent(title: String, force: bool) -> Result<()> {
    let agent_id = std::env::var("NEBULA_AGENT_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .context(
            "NEBULA_AGENT_ID is not set — `nebula rename` only works from inside a \
             nebula agent session",
        )?;
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running — title unchanged");
    };
    let mut conn = handshake(stream).await?;
    let req_id = 1u64;
    let id = AgentId(agent_id);
    let request = if force {
        ClientRequest::RenameAgent {
            req_id,
            id,
            name: title.clone(),
        }
    } else {
        ClientRequest::AutoRenameAgent {
            req_id,
            id,
            name: title.clone(),
        }
    };
    write_frame(&mut conn.stream, &request).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Ack { req_id: r, .. }) if r == req_id => {
                println!("session renamed to \"{title}\"");
                return Ok(());
            }
            Some(ServerEvent::Error {
                req_id: Some(r),
                message,
            }) if r == req_id => {
                println!("nebula: {message}");
                return Ok(());
            }
            Some(_) => continue,
            None => bail!("daemon closed the connection before replying"),
        }
    }
}

/// One-shot client for `nebula add <dir>` (and bare `nebula <dir>`): resolve
/// the path locally — the daemon's cwd is not ours, so relative paths must be
/// absolutized here — and ask the daemon to register it as a project. The
/// daemon owns the rest: normalizing to the repo toplevel, naming the project
/// after the directory, rejecting non-repos and duplicates. Spawns a daemon
/// when none is running, same as launching the TUI would.
pub async fn add_project(path: String) -> Result<()> {
    let expanded = match (path.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(home)) => std::path::PathBuf::from(home).join(rest),
        _ => std::path::PathBuf::from(&path),
    };
    let dir = std::fs::canonicalize(&expanded)
        .with_context(|| format!("{} does not exist", expanded.display()))?;
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let mut conn = connect_or_spawn().await?;
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::AddProject {
            req_id,
            path: dir.clone(),
            name: None,
            create_missing: false,
        },
    )
    .await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Ack { req_id: r, .. }) if r == req_id => {
                println!("added project {}", dir.display());
                return Ok(());
            }
            Some(ServerEvent::Error {
                req_id: Some(r),
                message,
            }) if r == req_id => bail!("{message}"),
            Some(_) => continue,
            None => bail!("daemon closed the connection before replying"),
        }
    }
}

/// One `nebula workspace <op>` invocation, resolved and executed against the
/// daemon (spawned when absent, same as `nebula add`).
#[derive(Debug, Clone)]
pub enum WorkspaceOp {
    Add { name: String },
    Open { name: String },
    List,
    Delete { name: String },
    Rename { name: String, new_name: String },
}

/// One-shot client for `nebula workspace …`. Name→id resolution runs off a
/// snapshot (Subscribe's first reply), so the daemon's RPC surface stays
/// id-based for the TUI's picker.
pub async fn run_workspace_op(op: WorkspaceOp) -> Result<()> {
    use nebula_core::{Workspace, WorkspaceId};
    let mut conn = connect_or_spawn().await?;
    write_frame(&mut conn.stream, &ClientRequest::Subscribe).await?;
    let (workspaces, active, projects) = loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Snapshot {
                workspaces,
                active_workspace,
                projects,
                ..
            }) => break (workspaces, active_workspace, projects),
            Some(_) => continue,
            None => bail!("daemon closed the connection before sending a snapshot"),
        }
    };
    let resolve = |name: &str| -> Result<WorkspaceId> {
        workspaces
            .iter()
            .find(|w: &&Workspace| w.name == name)
            .map(|w| w.id.clone())
            .with_context(|| {
                let names: Vec<&str> = workspaces.iter().map(|w| w.name.as_str()).collect();
                format!(
                    "no workspace named '{name}' (available: {})",
                    names.join(", ")
                )
            })
    };
    let req_id = 1u64;
    let (request, done): (ClientRequest, String) = match op {
        WorkspaceOp::List => {
            for w in &workspaces {
                let marker = if w.id == active { "*" } else { " " };
                let count = projects.iter().filter(|p| p.workspace_id == w.id).count();
                println!(
                    "{marker} {}  ({count} project{})",
                    w.name,
                    if count == 1 { "" } else { "s" }
                );
            }
            return Ok(());
        }
        WorkspaceOp::Add { name } => (
            ClientRequest::AddWorkspace {
                req_id,
                name: name.clone(),
            },
            format!("workspace '{name}' added — open it with `nebula workspace open {name}`"),
        ),
        WorkspaceOp::Open { name } => (
            ClientRequest::OpenWorkspace {
                req_id,
                id: resolve(&name)?,
            },
            format!("workspace '{name}' opened"),
        ),
        WorkspaceOp::Delete { name } => (
            ClientRequest::RemoveWorkspace {
                req_id,
                id: resolve(&name)?,
            },
            format!("workspace '{name}' deleted"),
        ),
        WorkspaceOp::Rename { name, new_name } => (
            ClientRequest::RenameWorkspace {
                req_id,
                id: resolve(&name)?,
                name: new_name.clone(),
            },
            format!("workspace '{name}' renamed to '{new_name}'"),
        ),
    };
    write_frame(&mut conn.stream, &request).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Ack { req_id: r, .. }) if r == req_id => {
                println!("{done}");
                return Ok(());
            }
            Some(ServerEvent::Error {
                req_id: Some(r),
                message,
            }) if r == req_id => bail!("{message}"),
            Some(_) => continue,
            None => bail!("daemon closed the connection before replying"),
        }
    }
}

/// Ask a running daemon to shut down. Ok(false) when none is running.
///
/// A daemon on a different protocol version closes the socket right after
/// the handshake, so `Shutdown` can never reach it — exactly the situation
/// `nebula kill` exists to fix. Fall back to SIGTERM via the pidfile, guarded
/// by the daemon's flock so a stale pid is never signalled.
pub async fn kill_daemon() -> Result<bool> {
    let sock = paths::socket_path();
    if let Ok(stream) = try_connect(&sock).await {
        if let Ok(mut conn) = handshake(stream).await {
            write_frame(&mut conn.stream, &ClientRequest::Shutdown).await?;
            wait_for_daemon_exit().await;
            return Ok(true);
        }
        return kill_by_pidfile().await;
    }
    // Nothing listening — but a wedged or mid-boot daemon may still hold the
    // pidfile lock; fall through to the same check.
    kill_by_pidfile().await
}

/// Outcome of `shutdown_if_idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleShutdown {
    /// Nothing listening on the socket.
    NoDaemon,
    /// The daemon held no live PTYs and was shut down cleanly.
    ShutDown,
    /// Live sessions exist; the daemon was left running.
    SessionsLive { count: usize },
    /// A daemon is listening but its protocol version differs, so its
    /// session state can't be inspected.
    Skewed,
}

/// Shut the daemon down only when it holds no live PTYs — the post-upgrade
/// handoff. An idle daemon can die safely (the next client launch spawns one
/// from the new binary on disk); live sessions would be killed with it, so
/// their daemon is left alone and the restart stays the user's call.
pub async fn shutdown_if_idle() -> Result<IdleShutdown> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        return Ok(IdleShutdown::NoDaemon);
    };
    let Ok(mut conn) = handshake(stream).await else {
        return Ok(IdleShutdown::Skewed);
    };
    write_frame(&mut conn.stream, &ClientRequest::Subscribe).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Snapshot {
                agents, terminals, ..
            }) => {
                let live = agents.iter().filter(|a| a.alive).count()
                    + terminals.iter().filter(|t| t.alive).count();
                if live > 0 {
                    return Ok(IdleShutdown::SessionsLive { count: live });
                }
                write_frame(&mut conn.stream, &ClientRequest::Shutdown).await?;
                wait_for_daemon_exit().await;
                return Ok(IdleShutdown::ShutDown);
            }
            Some(_) => continue,
            None => bail!("daemon closed the connection before sending a snapshot"),
        }
    }
}

/// SIGTERM the daemon recorded in the pidfile (its SIGTERM handler runs the
/// same clean shutdown as `Shutdown`). Ok(false) when no daemon is alive.
async fn kill_by_pidfile() -> Result<bool> {
    let path = paths::pidfile_path();
    if !daemon_holds_pidfile_lock(&path) {
        return Ok(false);
    }
    let pid: i32 = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .filter(|pid| *pid > 0)
        .context("daemon is running but its pidfile is unreadable — kill it manually")?;
    if send_sigterm(pid) != 0 {
        bail!("failed to signal daemon pid {pid} — kill it manually");
    }
    wait_for_daemon_exit().await;
    Ok(true)
}

/// Liveness = flock possession (mirrors the daemon's PidfileLock): if we can
/// take the lock ourselves, nobody holds it. Released on drop.
fn daemon_holds_pidfile_lock(path: &std::path::Path) -> bool {
    use std::os::fd::AsRawFd;
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    else {
        return false;
    };
    flock_try_exclusive(file.as_raw_fd()) != 0
}

/// Poll until the daemon releases its pidfile lock, so a relaunch right after
/// `nebula kill` can't race the old daemon's teardown.
async fn wait_for_daemon_exit() {
    let path = paths::pidfile_path();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while daemon_holds_pidfile_lock(&path) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// Tiny extern shims, same dep-light idiom as nebula_core::paths.
fn flock_try_exclusive(fd: i32) -> i32 {
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    unsafe { flock(fd, LOCK_EX | LOCK_NB) }
}

fn send_sigterm(pid: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    unsafe { kill(pid, SIGTERM) }
}

// ---- agent-facing control verbs (`nebula worktree new`, `nebula agent …`) ----

/// One tree snapshot over an already-handshaken connection: send Subscribe,
/// return the first Snapshot's rows. The connection keeps streaming deltas
/// afterwards, which the control verbs use to catch their own upserts.
async fn subscribe_snapshot(
    conn: &mut Connection,
) -> Result<(
    Vec<nebula_core::Project>,
    Vec<nebula_core::Worktree>,
    Vec<nebula_core::Agent>,
)> {
    write_frame(&mut conn.stream, &ClientRequest::Subscribe).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Snapshot {
                projects,
                worktrees,
                agents,
                ..
            }) => return Ok((projects, worktrees, agents)),
            Some(_) => continue,
            None => bail!("daemon closed the connection before the snapshot"),
        }
    }
}

/// Resolve which project a control verb targets: `--project <name>` wins,
/// otherwise the caller's own session (NEBULA_AGENT_ID) names it.
fn resolve_project(
    projects: &[nebula_core::Project],
    worktrees: &[nebula_core::Worktree],
    agents: &[nebula_core::Agent],
    project_flag: Option<&str>,
) -> Result<nebula_core::Project> {
    if let Some(name) = project_flag {
        let mut hits = projects.iter().filter(|p| p.name == name);
        let hit = hits.next().with_context(|| {
            format!(
                "no project named \"{name}\" (have: {})",
                projects
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        return Ok(hit.clone());
    }
    let agent_id = std::env::var("NEBULA_AGENT_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .context(
            "not inside a nebula agent session — pass --project <name> to pick the target",
        )?;
    let agent = agents
        .iter()
        .find(|a| a.id.as_str() == agent_id)
        .context("this session's agent row is gone from the daemon")?;
    let worktree = worktrees
        .iter()
        .find(|w| w.id == agent.worktree_id)
        .context("this session's worktree is gone from the daemon")?;
    projects
        .iter()
        .find(|p| p.id == worktree.project_id)
        .context("this session's project is gone from the daemon")
        .cloned()
}

/// Await the Ack (or Error) for `req_id`, collecting entity upserts seen on
/// the way; the daemon broadcasts the created row just before the Ack.
async fn await_ack(
    conn: &mut Connection,
    req_id: u64,
    upserts: &mut Vec<nebula_core::Entity>,
) -> Result<Option<nebula_core::EntityId>> {
    loop {
        let event = tokio::time::timeout(
            Duration::from_secs(60),
            read_frame::<ServerEvent, _>(&mut conn.stream),
        )
        .await
        .context("timed out waiting for the daemon's reply")??;
        match event {
            Some(ServerEvent::Ack { req_id: r, created }) if r == req_id => return Ok(created),
            Some(ServerEvent::Error {
                req_id: Some(r),
                message,
            }) if r == req_id => bail!("{message}"),
            Some(ServerEvent::EntityUpserted { entity }) => upserts.push(entity),
            Some(_) => continue,
            None => bail!("daemon closed the connection before replying"),
        }
    }
}

/// `nebula worktree new <name> [--from <ref>] [--project <name>]`.
pub async fn worktree_new(
    name: String,
    from: Option<String>,
    project_flag: Option<String>,
) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let branch = crate::branch_name::slugify(&name);
    if branch.is_empty() {
        bail!("worktree name slugifies to nothing: {name:?}");
    }
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::CreateWorktree {
            req_id,
            project: project.id.clone(),
            branch: branch.clone(),
            base: from,
        },
    )
    .await?;
    let mut upserts = Vec::new();
    let created = await_ack(&mut conn, req_id, &mut upserts).await?;
    let find_path = |upserts: &[nebula_core::Entity]| {
        upserts.iter().find_map(|e| match e {
            nebula_core::Entity::Worktree(w)
                if Some(&nebula_core::EntityId::Worktree(w.id.clone())) == created.as_ref() =>
            {
                Some(w.path.display().to_string())
            }
            _ => None,
        })
    };
    let mut path = find_path(&upserts);
    // The row's upsert can land just after the Ack — give it a moment so
    // the caller (an orchestrator about to spawn a worker) gets the path.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while path.is_none() && tokio::time::Instant::now() < deadline {
        let Ok(Ok(Some(event))) = tokio::time::timeout(
            Duration::from_millis(250),
            read_frame::<ServerEvent, _>(&mut conn.stream),
        )
        .await
        else {
            continue;
        };
        if let ServerEvent::EntityUpserted { entity } = event {
            upserts.push(entity);
            path = find_path(&upserts);
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "project": project.name,
            "branch": branch,
            "path": path,
        })
    );
    Ok(())
}

/// `nebula agent new` flags, bundled — the CLI surface mirrors the TUI's
/// new-session picker plus the orchestration extras.
pub struct NewAgentOpts {
    pub worktree: Option<String>,
    pub project: Option<String>,
    pub kind: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub name: Option<String>,
    pub orchestrator: bool,
    pub prompt: Option<String>,
}

/// `nebula agent new`: spawn a session — for orchestrators on the root
/// worktree by default, for workers wherever `--worktree` points.
pub async fn agent_new(opts: NewAgentOpts) -> Result<()> {
    let kind = nebula_core::AgentKind::parse(&opts.kind)
        .with_context(|| format!("unknown agent kind {:?} (claude|codex|cursor|pi)", opts.kind))?;
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, opts.project.as_deref())?;
    let of_project: Vec<&nebula_core::Worktree> = worktrees
        .iter()
        .filter(|w| w.project_id == project.id)
        .collect();
    let target = match opts.worktree.as_deref() {
        // Match by branch, by directory name, or "root" for the main checkout.
        Some(sel) => of_project
            .iter()
            .find(|w| {
                w.branch == sel
                    || (sel == "root" && w.is_main)
                    || w.path.file_name().is_some_and(|n| n == sel)
            })
            .with_context(|| {
                format!(
                    "no worktree \"{sel}\" in {} (have: {})",
                    project.name,
                    of_project
                        .iter()
                        .map(|w| w.branch.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?,
        // Orchestrators default to the root checkout; workers must say where.
        None if opts.orchestrator => of_project
            .iter()
            .find(|w| w.is_main)
            .context("project has no root worktree")?,
        None => bail!("pass --worktree <branch> (or --orchestrator for the root checkout)"),
    };
    let auto_title = opts.name.is_none();
    let name = opts.name.unwrap_or_else(|| {
        let n = agents
            .iter()
            .filter(|a| a.worktree_id == target.id)
            .count()
            + 1;
        if opts.orchestrator {
            format!("orchestrator-{n}")
        } else {
            format!("agent-{n}")
        }
    });
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::CreateAgent {
            req_id,
            worktree: target.id.clone(),
            name: name.clone(),
            kind,
            model: opts.model,
            effort: opts.effort,
            auto_title,
            orchestrator: opts.orchestrator,
            prompt: opts.prompt,
        },
    )
    .await?;
    let mut upserts = Vec::new();
    let created = await_ack(&mut conn, req_id, &mut upserts).await?;
    let id = match created {
        Some(nebula_core::EntityId::Agent(id)) => id.to_string(),
        _ => String::new(),
    };
    println!(
        "{}",
        serde_json::json!({
            "id": id,
            "name": name,
            "kind": opts.kind,
            "project": project.name,
            "worktree": target.branch,
            "orchestrator": opts.orchestrator,
        })
    );
    Ok(())
}

/// `nebula agent list [--project <name>] [--all]`: one JSON array of the
/// project's sessions (or every project's, with --all), status included —
/// the orchestrator's view of its workers.
pub async fn agent_list(project_flag: Option<String>, all: bool) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let scope = if all {
        None
    } else {
        Some(resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?)
    };
    let rows: Vec<serde_json::Value> = agents
        .iter()
        .filter_map(|a| {
            let worktree = worktrees.iter().find(|w| w.id == a.worktree_id)?;
            let project = projects.iter().find(|p| p.id == worktree.project_id)?;
            if let Some(scope) = &scope {
                if project.id != scope.id {
                    return None;
                }
            }
            Some(serde_json::json!({
                "id": a.id.to_string(),
                "name": a.name,
                "kind": a.kind.as_str(),
                "status": a.status.as_str(),
                "project": project.name,
                "worktree": worktree.branch,
                "path": worktree.path.display().to_string(),
                "orchestrator": a.orchestrator,
                "archived": a.archived,
            }))
        })
        .collect();
    println!("{}", serde_json::Value::Array(rows));
    Ok(())
}

pub enum NotesOp {
    List,
    Add { text: String, worktree: bool },
    Done { index: usize },
}

/// `nebula notes [list|add|done]` — the notes lists (project + current
/// worktree) from the CLI, so agents can read and work the user's to-do
/// list. Targets the caller's session (NEBULA_AGENT_ID) when run inside
/// one, else whatever project/worktree owns the current directory.
pub async fn run_notes(op: NotesOp) -> Result<()> {
    use nebula_core::{Note, NoteOwner};
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    write_frame(&mut conn.stream, &ClientRequest::Subscribe).await?;
    let (projects, worktrees, agents, notes) = loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Snapshot {
                projects,
                worktrees,
                agents,
                notes,
                ..
            }) => break (projects, worktrees, agents, notes),
            Some(_) => continue,
            None => bail!("daemon closed the connection before the snapshot"),
        }
    };

    // Whose notes: the session's worktree when inside one, else the deepest
    // worktree (or failing that, the project) holding the cwd.
    let by_agent = std::env::var("NEBULA_AGENT_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .and_then(|id| agents.iter().find(|a| a.id.as_str() == id))
        .and_then(|a| worktrees.iter().find(|w| w.id == a.worktree_id));
    let worktree = by_agent.or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        worktrees
            .iter()
            .filter(|w| cwd.starts_with(&w.path))
            .max_by_key(|w| w.path.components().count())
    });
    let project = match worktree {
        Some(w) => projects
            .iter()
            .find(|p| p.id == w.project_id)
            .context("this worktree's project is gone from the daemon")?,
        None => {
            let cwd = std::env::current_dir().context("resolve current directory")?;
            projects
                .iter()
                .find(|p| cwd.starts_with(&p.repo_path))
                .context(
                    "not inside a nebula agent session or a known project — \
                     run from a project directory",
                )?
        }
    };

    // One numbered list: the project's notes, then the worktree's. Indices
    // are what `done <n>` takes, recomputed per invocation.
    let mut visible: Vec<&Note> = notes
        .iter()
        .filter(|n| n.owner == NoteOwner::Project(project.id.clone()))
        .collect();
    let project_count = visible.len();
    if let Some(w) = worktree {
        visible.extend(
            notes
                .iter()
                .filter(|n| n.owner == NoteOwner::Worktree(w.id.clone())),
        );
    }

    let req_id = 1u64;
    match op {
        NotesOp::List => {
            if visible.is_empty() {
                println!(
                    "no notes for {} — add one with `nebula notes add <text>`",
                    project.name
                );
                return Ok(());
            }
            for (i, n) in visible.iter().enumerate() {
                if i == 0 && project_count > 0 {
                    println!("{} — project notes", project.name);
                }
                if i == project_count {
                    // Reachable only when a worktree was resolved.
                    println!(
                        "{}/{} — worktree notes",
                        project.name,
                        worktree.map(|w| w.branch.as_str()).unwrap_or("?")
                    );
                }
                let mark = if n.done { "x" } else { " " };
                println!("  {}. [{mark}] {}", i + 1, n.text);
            }
            Ok(())
        }
        NotesOp::Add {
            text,
            worktree: to_worktree,
        } => {
            let owner = if to_worktree {
                let w = worktree.context(
                    "--worktree needs a current worktree (run inside a session or a checkout)",
                )?;
                NoteOwner::Worktree(w.id.clone())
            } else {
                NoteOwner::Project(project.id.clone())
            };
            write_frame(
                &mut conn.stream,
                &ClientRequest::CreateNote {
                    req_id,
                    owner,
                    text: text.clone(),
                },
            )
            .await?;
            await_ack(&mut conn, req_id, &mut Vec::new()).await?;
            println!("note added: {text}");
            Ok(())
        }
        NotesOp::Done { index } => {
            let n = index
                .checked_sub(1)
                .and_then(|i| visible.get(i))
                .with_context(|| {
                    format!(
                        "no note {index} — `nebula notes` lists {} note{}",
                        visible.len(),
                        if visible.len() == 1 { "" } else { "s" }
                    )
                })?;
            write_frame(
                &mut conn.stream,
                &ClientRequest::SetNoteDone {
                    req_id,
                    id: n.id.clone(),
                    done: true,
                },
            )
            .await?;
            await_ack(&mut conn, req_id, &mut Vec::new()).await?;
            println!("note {index} done: {}", n.text);
            Ok(())
        }
    }
}

/// `nebula agent promote|demote <name>`: flip a session's orchestrator
/// role. Promotion is refused daemon-side off the root checkout.
pub async fn agent_set_orchestrator(
    name: String,
    project_flag: Option<String>,
    orchestrator: bool,
) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let of_project: Vec<&nebula_core::Agent> = agents
        .iter()
        .filter(|a| {
            !a.archived
                && worktrees
                    .iter()
                    .any(|w| w.id == a.worktree_id && w.project_id == project.id)
        })
        .collect();
    let agent = of_project
        .iter()
        .find(|a| a.name == name)
        .with_context(|| {
            format!(
                "no session \"{name}\" in {} (have: {})",
                project.name,
                of_project
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::SetAgentOrchestrator {
            req_id,
            id: agent.id.clone(),
            orchestrator,
        },
    )
    .await?;
    let mut upserts = Vec::new();
    await_ack(&mut conn, req_id, &mut upserts).await?;
    println!(
        "{}",
        serde_json::json!({
            "name": name,
            "project": project.name,
            "orchestrator": orchestrator,
        })
    );
    Ok(())
}
