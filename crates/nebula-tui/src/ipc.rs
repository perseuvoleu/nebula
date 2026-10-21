//! Client-side IPC: connect to the daemon (auto-spawning it when absent) and
//! perform the version handshake.

use anyhow::{bail, Context, Result};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{paths, AgentId, ClientRequest, ServerEvent, PROTOCOL_VERSION};
use std::{path::Path, time::Duration};
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

/// `nebula proxy`: pump stdin/stdout to this machine's daemon socket, no
/// framing, no handshake — the peer speaks the protocol itself. It is what
/// another nebula's relay runs over `ssh host nebula proxy` to reach this
/// daemon; spawning the daemon when none is listening keeps a fresh host
/// zero-setup. Exits when either side closes.
pub async fn proxy() -> Result<()> {
    let sock = paths::socket_path();
    let mut stream = match try_connect(&sock).await {
        Ok(s) => s,
        Err(_) => {
            spawn_daemon()?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                match try_connect(&sock).await {
                    Ok(s) => break s,
                    Err(_) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(e) => return Err(e).context("daemon did not come up"),
                }
            }
        }
    };
    let mut stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    let _ = tokio::io::copy_bidirectional(&mut stdio, &mut stream).await;
    Ok(())
}

pub(crate) async fn try_connect(sock: &std::path::Path) -> Result<UnixStream> {
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

pub(crate) async fn handshake(mut stream: UnixStream) -> Result<Connection> {
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

/// NEBULA_AGENT_ID when it names a real agent session. Shell tabs export a
/// `term:`-prefixed id (hook routing for hand-run CLIs) that no agent row
/// matches — commands that resolve their own agent treat it as absent.
fn env_agent_id() -> Option<String> {
    std::env::var("NEBULA_AGENT_ID")
        .ok()
        .filter(|v| !v.is_empty() && !v.starts_with("term:"))
}

/// The calling agent session as `(id, name)`, for `nebula diagram new` to
/// record a diagram's owner. The id comes from the environment alone; the
/// name is a best-effort daemon lookup that never blocks or fails the verb.
pub async fn calling_session() -> Option<(String, Option<String>)> {
    let id = env_agent_id()?;
    let lookup = async {
        let stream = try_connect(&paths::socket_path()).await.ok()?;
        let mut conn = handshake(stream).await.ok()?;
        let (_, _, agents) = subscribe_snapshot(&mut conn).await.ok()?;
        agents
            .into_iter()
            .find(|a| a.id.as_str() == id)
            .map(|a| a.name)
    };
    let name = tokio::time::timeout(Duration::from_secs(2), lookup)
        .await
        .ok()
        .flatten();
    Some((id, name))
}

/// One-shot client for `nebula rename`, run from inside an agent session's
/// CLI: resolve the agent from NEBULA_AGENT_ID and ask the daemon to title
/// it. Never spawns a daemon — no daemon means no session worth titling.
///
/// Daemon-reported outcomes (renamed, or "already titled" on the non-force
/// path) both print and exit 0: for the model running this, a declined
/// auto-title is a settled answer, not a failure to retry.
pub async fn rename_current_agent(title: String, force: bool) -> Result<()> {
    let agent_id = env_agent_id().context(
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
    // `host:/path` is a checkout on another machine — nothing to check
    // here; the daemon reads it over ssh (and expands a `~`).
    let (dir, host) = match nebula_core::remote::parse_spec(&path) {
        Some((host, remote_path)) => (std::path::PathBuf::from(remote_path), Some(host)),
        None => {
            let expanded = match (path.strip_prefix("~/"), std::env::var("HOME")) {
                (Some(rest), Ok(home)) => std::path::PathBuf::from(home).join(rest),
                _ => std::path::PathBuf::from(&path),
            };
            let dir = std::fs::canonicalize(&expanded)
                .with_context(|| format!("{} does not exist", expanded.display()))?;
            if !dir.is_dir() {
                bail!("{} is not a directory", dir.display());
            }
            (dir, None)
        }
    };
    let mut conn = connect_or_spawn().await?;
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::AddProject {
            req_id,
            path: dir.clone(),
            name: None,
            create_missing: false,
            host: host.clone(),
        },
    )
    .await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Ack { req_id: r, .. }) if r == req_id => {
                match host {
                    Some(h) => println!("added remote project {h}:{}", dir.display()),
                    None => println!("added project {}", dir.display()),
                }
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
pub(crate) async fn subscribe_snapshot(
    conn: &mut Connection,
) -> Result<(
    Vec<nebula_core::Project>,
    Vec<nebula_core::Worktree>,
    Vec<nebula_core::Agent>,
)> {
    let (projects, worktrees, agents, _) = subscribe_snapshot_with_terminals(conn).await?;
    Ok((projects, worktrees, agents))
}

/// Save a preview URL on its exact checkout so the TUI keeps it beside that
/// checkout's sessions. Preview startup remains successful when no daemon runs.
pub async fn pin_checkout_link(checkout: &Path, url: &str) -> Result<()> {
    let sock = paths::socket_path();
    let stream = try_connect(&sock)
        .await
        .context("no nebula daemon is running")?;
    let mut conn = handshake(stream).await?;
    write_frame(&mut conn.stream, &ClientRequest::Subscribe).await?;
    let (worktrees, links) = loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Snapshot {
                worktrees, links, ..
            }) => break (worktrees, links),
            Some(_) => continue,
            None => bail!("daemon closed the connection before the snapshot"),
        }
    };
    let checkout = std::fs::canonicalize(checkout)?;
    let worktree = worktrees
        .iter()
        .find(|w| std::fs::canonicalize(&w.path).ok().as_ref() == Some(&checkout))
        .context("preview checkout is not registered in nebula")?;
    if links
        .iter()
        .any(|link| link.worktree_id == worktree.id && link.url == url)
    {
        return Ok(());
    }
    let req_id = 1;
    write_frame(
        &mut conn.stream,
        &ClientRequest::CreateLink {
            req_id,
            worktree: worktree.id.clone(),
            url: url.to_string(),
        },
    )
    .await?;
    let mut upserts = Vec::new();
    await_ack(&mut conn, req_id, &mut upserts).await?;
    Ok(())
}

async fn subscribe_snapshot_with_terminals(
    conn: &mut Connection,
) -> Result<(
    Vec<nebula_core::Project>,
    Vec<nebula_core::Worktree>,
    Vec<nebula_core::Agent>,
    Vec<nebula_core::TerminalTab>,
)> {
    write_frame(&mut conn.stream, &ClientRequest::Subscribe).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Snapshot {
                projects,
                worktrees,
                agents,
                terminals,
                ..
            }) => return Ok((projects, worktrees, agents, terminals)),
            Some(_) => continue,
            None => bail!("daemon closed the connection before the snapshot"),
        }
    }
}

/// `--project name@host` split into name and host; `None` for a local name.
/// A bare name means the local project when both exist (the panel's
/// `name @host` spelling, minus the space).
pub fn project_flag_host(flag: Option<&str>) -> Option<(&str, &str)> {
    flag?
        .rsplit_once('@')
        .filter(|(n, h)| !n.is_empty() && !h.is_empty())
}

/// Resolve which project a control verb targets: `--project <name>` wins,
/// otherwise the caller's own session (NEBULA_AGENT_ID) names it.
pub(crate) fn resolve_project(
    projects: &[nebula_core::Project],
    worktrees: &[nebula_core::Worktree],
    agents: &[nebula_core::Agent],
    project_flag: Option<&str>,
) -> Result<nebula_core::Project> {
    if let Some(flag) = project_flag {
        let (name, host) = match project_flag_host(Some(flag)) {
            Some((n, h)) => (n, Some(h)),
            None => (flag, None),
        };
        let mut named = projects.iter().filter(|p| p.name == name);
        let hit = match host {
            Some(h) => named.find(|p| p.host.as_deref() == Some(h)),
            None => {
                let all: Vec<_> = named.collect();
                all.iter()
                    .find(|p| p.host.is_none())
                    .or_else(|| all.first())
                    .copied()
            }
        };
        let hit = hit.with_context(|| {
            format!(
                "no project named \"{flag}\" (have: {})",
                projects
                    .iter()
                    .map(|p| match &p.host {
                        Some(h) => format!("{}@{h}", p.name),
                        None => p.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        return Ok(hit.clone());
    }
    let agent_id = env_agent_id()
        .context("not inside a nebula agent session — pass --project <name> to pick the target")?;
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
    let path = create_worktree_path(&mut conn, &project.id, &branch, from).await?;
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

/// Ask the daemon for a worktree on `branch` (created from `base`, or the
/// primary's HEAD) and return the checkout's path once its row lands.
async fn create_worktree_path(
    conn: &mut Connection,
    project: &nebula_core::ProjectId,
    branch: &str,
    base: Option<String>,
) -> Result<Option<String>> {
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::CreateWorktree {
            req_id,
            project: project.clone(),
            branch: branch.to_string(),
            base,
        },
    )
    .await?;
    let mut upserts = Vec::new();
    let created = await_ack(conn, req_id, &mut upserts).await?;
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
    // the caller (a session about to spawn a worker) gets the path.
    // One deadline owns the whole read loop. A timeout returns an error and
    // the caller abandons this connection; never retry a partially read frame.
    tokio::time::timeout(Duration::from_secs(2), async {
        while path.is_none() {
            let Some(event) = read_frame::<ServerEvent, _>(&mut conn.stream).await? else {
                bail!("connection closed while waiting for the created worktree row");
            };
            if let ServerEvent::EntityUpserted { entity } = event {
                upserts.push(entity);
                path = find_path(&upserts);
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("worktree was acknowledged but its row did not arrive; inspect before retrying")??;
    Ok(path)
}

/// `nebula switch <branch> [--from <base>] [--worktree] [--project <name>]`
/// — land the calling shell on a branch. A branch is *where the project
/// is*: without `--worktree` it is checked out in the primary checkout
/// (created first, like `git checkout -b`, from the branch the caller is
/// on) and the shell `cd`s to the project's own path. `--worktree` gives
/// the branch its own directory instead — the way parallel work runs. A
/// branch always targets primary unless --worktree explicitly selects a
/// separate checkout. Inside a nebula terminal the `cd` is
/// typed into that tab's PTY, so the shell lands there once this command
/// returns; elsewhere the path is printed for the caller to `cd` into.
pub async fn switch(
    branch: String,
    from: Option<String>,
    worktree: bool,
    project_flag: Option<String>,
    stop_busy: bool,
) -> Result<()> {
    let branch = branch.trim().to_string();
    if branch.is_empty() {
        bail!("branch name is empty");
    }
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = match resolve_project(&projects, &worktrees, &agents, project_flag.as_deref()) {
        Ok(p) => p,
        // A shell tab (or any shell) names no agent: the checkout the
        // caller stands in picks the project.
        Err(e) => project_owning_cwd(&projects, &worktrees).ok_or(e)?,
    };
    let primary = worktrees
        .iter()
        .find(|w| w.project_id == project.id && w.is_main)
        .context("the project has no primary checkout row")?;
    let existing = worktrees
        .iter()
        .find(|w| w.project_id == project.id && w.branch == branch);
    let local = project.host.is_none();
    let base = from.or_else(|| local.then(current_branch_here).flatten());
    // Host paths must never be interpreted as paths on this CLI's machine.
    // CheckoutPrimary resolves/fetches remote branches on their owning daemon.
    let pre_existing = local && git_branch_exists(&project.repo_path, &branch);
    let mover = calling_session_ref(&agents).filter(|_| local);
    let terminal = match &mover {
        Some(nebula_core::SessionRef::Terminal(id)) => Some(id.0.clone()),
        _ => None,
    };
    let (path, created, checked_out) = match existing.filter(|_| worktree) {
        Some(w) => (Some(w.path.display().to_string()), false, false),
        None if worktree => (
            create_worktree_path(
                &mut conn,
                &project.id,
                &branch,
                base.filter(|_| !pre_existing),
            )
            .await?,
            true,
            false,
        ),
        None => {
            if local && !pre_existing {
                git_create_branch(&project.repo_path, &branch, base.as_deref())?;
            }
            let req_id = 1u64;
            write_frame(
                &mut conn.stream,
                &ClientRequest::CheckoutPrimary {
                    req_id,
                    project: project.id.clone(),
                    branch: branch.clone(),
                    mover: mover.clone(),
                    stop_busy,
                },
            )
            .await?;
            let mut upserts = Vec::new();
            await_ack(&mut conn, req_id, &mut upserts)
                .await
                .map_err(|e| with_busy_hint(e, RERUN_WITH_STOP_BUSY))?;
            (
                Some(primary.path.display().to_string()),
                local && !pre_existing,
                true,
            )
        }
    };
    let Some(path) = path else {
        bail!("the daemon created the checkout but never reported its path");
    };
    if let Some(host) = &project.host {
        // Relay snapshots can retain vanished paths. Verify on the owning host,
        // never against a coincident directory on this CLI's machine.
        let git_dir = std::path::Path::new(&path).join(".git");
        let check = format!(
            "test -e {}",
            nebula_core::remote::ssh_quote(&git_dir.to_string_lossy())
        );
        crate::remote_cli::ssh(host, &check, false).with_context(|| {
            format!("checkout at {host}:{path} is unavailable; restore it or reconnect the host and retry; no cd was sent")
        })?;
    } else if !std::path::Path::new(&path).join(".git").exists() {
        bail!("checkout at {path} is unavailable; restore it and retry; no cd was sent");
    }
    if let Some(id) = &terminal {
        // The shell is blocked on this very command; the tty queues the
        // line and the shell runs it at its next prompt, like typed-ahead
        // input.
        let line = format!("cd '{}'\n", path.replace('\'', "'\\''"));
        write_frame(
            &mut conn.stream,
            &ClientRequest::Input {
                session: nebula_core::SessionRef::Terminal(nebula_core::TerminalId(id.clone())),
                data: line.into_bytes(),
            },
        )
        .await?;
    }
    println!(
        "{}",
        serde_json::json!({
            "project": project.name,
            "branch": branch,
            "path": path,
            "created": created,
            "checked_out_in_primary": checked_out,
            "cd": terminal.is_some(),
        })
    );
    if let Some(host) = &project.host {
        eprintln!("checkout on {host}: {path} (local shell unchanged)");
    } else if terminal.is_none() {
        eprintln!("cd '{path}'");
    }
    Ok(())
}

fn git_branch_exists(repo: &std::path::Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(repo)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `git branch <name> [<base>]` in the repo — no base means the primary's
/// HEAD, like git itself.
fn git_create_branch(repo: &std::path::Path, branch: &str, base: Option<&str>) -> Result<()> {
    let mut args = vec!["branch", branch];
    args.extend(base);
    let out = std::process::Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git branch {branch} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The branch checked out in the current directory; `None` when detached
/// or outside a checkout (the daemon then branches from the primary's
/// HEAD).
fn current_branch_here() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The project whose checkout contains the current directory (deepest
/// match), for verbs run from a plain shell.
fn project_owning_cwd(
    projects: &[nebula_core::Project],
    worktrees: &[nebula_core::Worktree],
) -> Option<nebula_core::Project> {
    let cwd = std::env::current_dir().ok()?;
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let owner = worktrees
        .iter()
        .map(|w| {
            (
                w,
                std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()),
            )
        })
        .filter(|(_, p)| cwd.starts_with(p))
        .max_by_key(|(_, p)| p.components().count())?
        .0;
    projects.iter().find(|p| p.id == owner.project_id).cloned()
}

/// Print a removal-style request's verdict lines, then its outcome.
async fn await_report(conn: &mut Connection, req_id: u64) -> Result<()> {
    loop {
        let event = tokio::time::timeout(
            Duration::from_secs(120),
            read_frame::<ServerEvent, _>(&mut conn.stream),
        )
        .await
        .context("timed out waiting for the daemon's reply")??;
        match event {
            Some(ServerEvent::Report { req_id: r, lines }) if r == req_id => {
                for line in lines {
                    println!("{line}");
                }
            }
            Some(ServerEvent::Ack { req_id: r, .. }) if r == req_id => return Ok(()),
            Some(ServerEvent::Error {
                req_id: Some(r),
                message,
            }) if r == req_id => bail!("{message}"),
            Some(_) => continue,
            None => bail!("daemon closed the connection before replying"),
        }
    }
}

/// Check or remove a clean, merged, session-free checkout by stable ID; retain its branch.
pub async fn worktree_delete_safe(
    id: String,
    branch: String,
    path: std::path::PathBuf,
    check: bool,
    with_archived: bool,
    project_flag: Option<String>,
) -> Result<()> {
    let stream = try_connect(&paths::socket_path())
        .await
        .context("no nebula daemon is running")?;
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let worktree = worktrees
        .iter()
        .find(|w| w.id.as_str() == id && w.project_id == project.id)
        .context("Checkout no longer available in this project")?;
    write_frame(
        &mut conn.stream,
        &ClientRequest::CheckWorktreeRemoval {
            req_id: 1,
            id: worktree.id.clone(),
            branch: branch.clone(),
            path: path.clone(),
            delete: !check,
            report: Some(nebula_core::RemovalFlags { with_archived }),
        },
    )
    .await?;
    await_report(&mut conn, 1).await?;
    match check {
        true => println!("verdict: safe to remove (--check: nothing changed)"),
        false => println!("removed {}; branch {branch} kept", path.display()),
    }
    Ok(())
}

/// `nebula worktree prune [--check] [--with-archived] [--project <name>]`.
pub async fn worktree_prune(
    check: bool,
    with_archived: bool,
    project_flag: Option<String>,
) -> Result<()> {
    let stream = try_connect(&paths::socket_path())
        .await
        .context("no nebula daemon is running")?;
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    write_frame(
        &mut conn.stream,
        &ClientRequest::PruneWorktrees {
            req_id: 1,
            project: project.id.clone(),
            delete: !check,
            with_archived,
        },
    )
    .await?;
    await_report(&mut conn, 1).await?;
    if check {
        println!("--check: nothing changed; rows marked ok are dropped without --check");
    }
    Ok(())
}

/// `nebula worktree delete <name> [--force] [--project <name>]` — the
/// daemon's delete flow, so the checkout AND nebula's row go together
/// (raw `git worktree remove` leaves a ghost row in the Worktrees panel).
pub async fn worktree_delete(sel: String, force: bool, project_flag: Option<String>) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let of_project: Vec<&nebula_core::Worktree> = worktrees
        .iter()
        .filter(|w| w.project_id == project.id)
        .collect();
    // The same selectors `agent new --worktree` takes, minus
    // "root"/"primary" (the main checkout is never deletable).
    let worktree = resolve_worktree(&of_project, &sel, &project.name, false)?;
    if worktree.is_main {
        bail!("cannot delete the primary checkout — remove the project instead");
    }
    // Deleting the worktree kills every session on it — refuse to saw off
    // the branch the caller itself sits on.
    if let Ok(agent_id) = std::env::var("NEBULA_AGENT_ID") {
        if agents
            .iter()
            .any(|a| a.id.as_str() == agent_id && a.worktree_id == worktree.id)
        {
            bail!("that is this session's own worktree — a delete would kill this session");
        }
    }
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::DeleteWorktree {
            req_id,
            id: worktree.id.clone(),
            force,
        },
    )
    .await?;
    let mut upserts = Vec::new();
    await_ack(&mut conn, req_id, &mut upserts).await?;
    println!(
        "{}",
        serde_json::json!({
            "project": project.name,
            "branch": worktree.branch,
            "deleted": true,
        })
    );
    Ok(())
}

/// Resolve one worktree row of a project by selector: a branch name, a
/// directory name, or (when `allow_primary`) "root"/"primary" for the main
/// checkout ("root" is the historical spelling, kept). A selector that
/// names one row by branch and a *different* row by directory is refused
/// rather than silently picking either — that happens after a branch
/// moves between checkouts (say `feature/x` now on the primary while
/// nebula's `feature-x` checkout sits detached), and a worker landing on
/// the wrong one is exactly the confusion the error prevents.
pub(crate) fn resolve_worktree<'a>(
    of_project: &[&'a nebula_core::Worktree],
    sel: &str,
    project_name: &str,
    allow_primary: bool,
) -> Result<&'a nebula_core::Worktree> {
    let by_branch = of_project.iter().copied().find(|w| w.branch == sel);
    let by_primary = (allow_primary && (sel == "root" || sel == "primary"))
        .then(|| of_project.iter().copied().find(|w| w.is_main))
        .flatten();
    let by_dir = of_project
        .iter()
        .copied()
        .find(|w| w.path.file_name().is_some_and(|n| n == sel));
    let mut hits: Vec<&nebula_core::Worktree> = Vec::new();
    for w in [by_branch, by_primary, by_dir].into_iter().flatten() {
        if !hits.iter().any(|h| h.id == w.id) {
            hits.push(w);
        }
    }
    match hits.as_slice() {
        [one] => Ok(one),
        [] => bail!(
            "no worktree \"{sel}\" in {project_name} (have: {})",
            of_project
                .iter()
                .map(|w| w.branch.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        many => bail!(
            "\"{sel}\" is ambiguous in {project_name}: {} — pass the directory name or the exact branch",
            many.iter()
                .map(|w| format!("{} at {}", w.branch, w.path.display()))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

fn calling_session_ref(agents: &[nebula_core::Agent]) -> Option<nebula_core::SessionRef> {
    let own = std::env::var("NEBULA_AGENT_ID").ok()?;
    match own.strip_prefix("term:") {
        Some(id) if !id.is_empty() => Some(nebula_core::SessionRef::Terminal(
            nebula_core::TerminalId(id.to_owned()),
        )),
        Some(_) => None,
        None => agents
            .iter()
            .find(|a| a.id.as_str() == own)
            .map(|a| nebula_core::SessionRef::Agent(a.id.clone())),
    }
}

fn switch_first_hint(project: &nebula_core::Project, branch: &str) -> String {
    let selector = match &project.host {
        Some(host) => format!("{}@{host}", project.name),
        None => project.name.clone(),
    };
    format!(
        "run `nebula switch {} --project {} --stop-busy` first, then retry",
        nebula_core::remote::ssh_quote(branch),
        nebula_core::remote::ssh_quote(&selector)
    )
}

const RERUN_WITH_STOP_BUSY: &str = "rerun with --stop-busy to stop them and switch";

fn with_busy_hint(err: anyhow::Error, hint: &str) -> anyhow::Error {
    match err.to_string() {
        m if m.starts_with(nebula_core::BUSY_SESSIONS_REFUSAL) => anyhow::anyhow!("{m}; {hint}"),
        _ => err,
    }
}

/// `nebula worktree checkout <branch>` explicitly switches primary. Refused
/// while other live sessions run on primary unless `stop_busy` confirms
/// stopping them, while primary has uncommitted changes or an untracked file
/// the branch would overwrite, or while the branch has another checkout. A
/// local caller whose session sits on primary moves along with it.
pub async fn worktree_checkout(
    branch: String,
    project_flag: Option<String>,
    stop_busy: bool,
) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents, terminals) =
        subscribe_snapshot_with_terminals(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    // The caller's own checkout is the one about to be removed: refuse
    // rather than saw off the branch this session sits on.
    if let Ok(agent_id) = std::env::var("NEBULA_AGENT_ID") {
        let own = agents.iter().find(|a| a.id.as_str() == agent_id);
        if let Some(w) = own.and_then(|a| worktrees.iter().find(|w| w.id == a.worktree_id)) {
            if !w.is_main && w.branch == branch {
                bail!("that is this session's own worktree — run this from the primary checkout");
            }
        }
    }
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::CheckoutPrimary {
            req_id,
            project: project.id.clone(),
            branch: branch.clone(),
            mover: calling_session_ref(&agents).filter(|caller| {
                let home = match caller {
                    nebula_core::SessionRef::Agent(id) => {
                        agents.iter().find(|a| &a.id == id).map(|a| &a.worktree_id)
                    }
                    nebula_core::SessionRef::Terminal(id) => terminals
                        .iter()
                        .find(|t| &t.id == id)
                        .map(|t| &t.worktree_id),
                };
                worktrees
                    .iter()
                    .any(|w| Some(&w.id) == home && w.is_main && w.project_id == project.id)
                    && project.host.is_none()
            }),
            stop_busy,
        },
    )
    .await?;
    let mut upserts = Vec::new();
    await_ack(&mut conn, req_id, &mut upserts)
        .await
        .map_err(|e| with_busy_hint(e, RERUN_WITH_STOP_BUSY))?;
    let primary = worktrees
        .iter()
        .find(|w| w.project_id == project.id && w.is_main)
        .map(|w| w.path.display().to_string());
    println!(
        "{}",
        serde_json::json!({
            "project": project.name,
            "branch": branch,
            "path": primary,
            "checked_out": true,
        })
    );
    Ok(())
}

/// Resolve one agent of `project` by session name (the name shown in the
/// panels) — the shared lookup behind read/send/show/archive/delete/restart.
fn resolve_agent<'a>(
    agents: &'a [nebula_core::Agent],
    worktrees: &'a [nebula_core::Worktree],
    project: &nebula_core::Project,
    name: &str,
) -> Result<(&'a nebula_core::Agent, &'a nebula_core::Worktree)> {
    let in_project = |a: &nebula_core::Agent| {
        worktrees
            .iter()
            .find(|w| w.id == a.worktree_id)
            .filter(|w| w.project_id == project.id)
    };
    agents
        .iter()
        .find_map(|a| {
            if a.name != name {
                return None;
            }
            in_project(a).map(|w| (a, w))
        })
        .with_context(|| {
            format!(
                "no agent named \"{name}\" in {} (have: {})",
                project.name,
                agents
                    .iter()
                    .filter(|a| in_project(a).is_some())
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

/// `nebula worktree list [--project <name>] [--all]`: one JSON array of the
/// project's worktrees (or every project's), each with its unarchived
/// sessions — the orchestration surface's map of who lives where.
pub async fn worktree_list(project_flag: Option<String>, all: bool) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let scope = if all {
        None
    } else {
        Some(resolve_project(
            &projects,
            &worktrees,
            &agents,
            project_flag.as_deref(),
        )?)
    };
    let rows: Vec<serde_json::Value> = worktrees
        .iter()
        .filter_map(|w| {
            let project = projects.iter().find(|p| p.id == w.project_id)?;
            if let Some(scope) = &scope {
                if project.id != scope.id {
                    return None;
                }
            }
            let sessions: Vec<serde_json::Value> = agents
                .iter()
                .filter(|a| a.worktree_id == w.id && !a.archived)
                .map(|a| {
                    serde_json::json!({
                        "name": a.name,
                        "kind": a.kind.as_str(),
                        "status": a.status.as_str(),
                        "alive": a.alive,
                    })
                })
                .collect();
            Some(serde_json::json!({
                "project": project.name,
                "branch": w.branch,
                "path": w.path.display().to_string(),
                "is_main": w.is_main,
                "id": w.id,
                "pinned": w.pinned,
                "created_from": w.created_from,
                "sessions": sessions,
            }))
        })
        .collect();
    println!("{}", serde_json::Value::Array(rows));
    Ok(())
}

/// `nebula agent show <name>`: every field of one session as one JSON
/// object — `agent list`'s row plus model/effort/pinned/timestamps.
pub async fn agent_show(name: String, project_flag: Option<String>) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let (a, w) = resolve_agent(&agents, &worktrees, &project, &name)?;
    println!(
        "{}",
        serde_json::json!({
            "id": a.id.to_string(),
            "name": a.name,
            "kind": a.kind.as_str(),
            "status": a.status.as_str(),
            "status_changed_at": a.status_changed_at,
            "project": project.name,
            "worktree": w.branch,
            "path": w.path.display().to_string(),
            "model": a.model,
            "effort": a.effort,
            "archived": a.archived,
            "pinned": a.pinned,
            "alive": a.alive,
            // Native CLI session id (claude/codex/cursor/pi) the daemon stored for
            // resume; read-only consumers (the web gateway) map it to native history.
            "session_id": a.session_id,
        })
    );
    Ok(())
}

/// `nebula agent read <name> [--lines N]`: the session's screen (and
/// scrollback tail) as plain text — rendered daemon-side at the live grid
/// size, so reading never attaches, resizes, or respawns anything. A
/// stopped session prints the screen the daemon saved when it stopped.
pub async fn agent_read(
    name: String,
    lines: Option<usize>,
    project_flag: Option<String>,
) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let (a, _) = resolve_agent(&agents, &worktrees, &project, &name)?;
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::ReadSession {
            req_id,
            session: nebula_core::SessionRef::Agent(a.id.clone()),
            lines,
        },
    )
    .await?;
    loop {
        let event = tokio::time::timeout(
            Duration::from_secs(30),
            read_frame::<ServerEvent, _>(&mut conn.stream),
        )
        .await
        .context("timed out waiting for the session text")??;
        match event {
            Some(ServerEvent::SessionText {
                req_id: r, text, ..
            }) if r == req_id => {
                println!("{text}");
                return Ok(());
            }
            // A stopped session with no saved screen: point at the respawn.
            Some(ServerEvent::Error {
                req_id: Some(r), ..
            }) if r == req_id && !a.alive => bail!(
                "\"{name}\" holds no live PTY (status: {}) — `nebula agent restart {name}` respawns it",
                a.status.as_str()
            ),
            Some(ServerEvent::Error {
                req_id: Some(r),
                message,
            }) if r == req_id => bail!("{message}"),
            Some(_) => continue,
            None => bail!("daemon closed the connection before replying"),
        }
    }
}

/// The longest single-line text `agent send` types raw. A tty hands a
/// raw-mode reader at most 1,022 bytes per read on macOS, and Claude Code
/// keeps only the last read of an unbracketed burst, so anything that could
/// span two reads goes as a bracketed paste instead.
const RAW_SEND_MAX: usize = 1000;

/// The bytes `agent send` writes for `text`, or why it refuses. Text that
/// must go as a bracketed paste is refused unless `paste` asks for one: a
/// Claude worker reads a paste as quoted content, not as an instruction.
pub fn send_payload(text: &str, paste: bool) -> Result<Vec<u8>, String> {
    if text.contains('\n') || text.len() > RAW_SEND_MAX {
        if !paste {
            return Err(format!(
                "refusing to send {} bytes{}: it would arrive as a paste, which a worker reads as quoted content, not as an instruction. Write the text to a file under the project's .scratch/ and send `Read <path> and act on it.` (`--file <path>` does that), or pass --paste to send it as a bracketed paste anyway",
                text.len(),
                if text.contains('\n') { " over several lines" } else { "" },
            ));
        }
        Ok(format!("\x1b[200~{text}\x1b[201~").into_bytes())
    } else {
        Ok(text.as_bytes().to_vec())
    }
}

/// The one-line pointer `agent send --file` sends instead of the file's text.
pub fn file_pointer(path: &Path, _remote: bool) -> Result<String, String> {
    let absolute =
        std::fs::canonicalize(path).map_err(|e| format!("--file {}: {e}", path.display()))?;
    if !absolute.is_file() {
        return Err(format!("--file {}: not a regular file", path.display()));
    }
    Ok(format!("Read {} and act on it.", absolute.display()))
}

/// `nebula agent send <name> <text…>`: type a follow-up prompt into a
/// running worker's CLI and submit it — the steering half of delegation
/// (`--prompt` only covers the first task). `payload` is the text as
/// [`send_payload`] encoded it; Enter follows
/// after a beat, once the CLI has ingested the text.
pub async fn agent_send(
    name: String,
    text: String,
    payload: Vec<u8>,
    project_flag: Option<String>,
) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let (a, w) = resolve_agent(&agents, &worktrees, &project, &name)?;
    if !a.alive {
        bail!(
            "\"{name}\" holds no live PTY (status: {}) — `nebula agent restart {name}` respawns it",
            a.status.as_str()
        );
    }
    // Acknowledged writes: "sent" is printed only for text the PTY took.
    for (req_id, data) in [(1, payload), (2, b"\r".to_vec())] {
        if req_id == 2 {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        write_frame(
            &mut conn.stream,
            &ClientRequest::InputAcknowledged {
                req_id,
                id: a.id.clone(),
                project: project.id.clone(),
                data,
            },
        )
        .await?;
        await_ack(&mut conn, req_id, &mut Vec::new()).await?;
    }
    println!(
        "{}",
        serde_json::json!({
            "name": a.name,
            "worktree": w.branch,
            "sent": text,
        })
    );
    Ok(())
}

/// Act on a stable agent ID: Some(key) writes one key; None explicitly deletes the agent.
pub async fn agent_action_id(
    id: String,
    key: Option<String>,
    project_flag: Option<String>,
) -> Result<()> {
    let stream = try_connect(&paths::socket_path())
        .await
        .context("no nebula daemon is running")?;
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let agent = agents
        .iter()
        .find(|a| {
            a.id.as_str() == id
                && !a.archived
                && worktrees
                    .iter()
                    .any(|w| w.id == a.worktree_id && w.project_id == project.id)
        })
        .context("Session no longer available in this project")?;
    let req_id = 1;
    let request = if let Some(key) = key {
        if !agent.alive {
            bail!("Session has no live PTY");
        }
        let data: &[u8] = match key.as_str() {
            "Up" => b"\x1b[A",
            "Down" => b"\x1b[B",
            "Right" => b"\x1b[C",
            "Left" => b"\x1b[D",
            "Enter" => b"\r",
            "Escape" => b"\x1b",
            "Tab" => b"\t",
            _ => bail!("Unsupported terminal key"),
        };
        ClientRequest::InputAcknowledged {
            req_id,
            id: agent.id.clone(),
            project: project.id.clone(),
            data: data.to_vec(),
        }
    } else {
        if std::env::var("NEBULA_AGENT_ID").is_ok_and(|own| own == id) {
            bail!("Cannot delete this session itself");
        }
        ClientRequest::DeleteAgentScoped {
            req_id,
            id: agent.id.clone(),
            project: project.id.clone(),
        }
    };
    write_frame(&mut conn.stream, &request).await?;
    await_ack(&mut conn, req_id, &mut Vec::new()).await?;
    println!("{}", serde_json::json!({"ok": true}));
    Ok(())
}

/// The lifecycle verbs `nebula agent archive|unarchive|delete|restart`
/// share one resolve-then-RPC shape; this enum picks the request.
pub enum AgentCtl {
    /// Kill the PTY, keep the row (archived).
    Archive,
    /// Bring an archived row back (PTY respawns on next attach).
    Unarchive,
    /// Kill the PTY and remove the row entirely.
    Delete,
    /// Respawn the CLI, resuming its stored session when one exists.
    Restart,
}

pub async fn agent_ctl(op: AgentCtl, name: String, project_flag: Option<String>) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;
    let (a, w) = resolve_agent(&agents, &worktrees, &project, &name)?;
    // Archive/delete/restart all kill the PTY — refuse to saw off the
    // session the caller itself runs in (same guard as worktree delete).
    if std::env::var("NEBULA_AGENT_ID").is_ok_and(|id| id == a.id.as_str()) {
        bail!("that is this session itself — it cannot archive/delete/restart its own PTY");
    }
    let req_id = 1u64;
    let (request, verb) = match op {
        AgentCtl::Archive => (
            ClientRequest::ArchiveAgent {
                req_id,
                id: a.id.clone(),
            },
            "archived",
        ),
        AgentCtl::Unarchive => (
            ClientRequest::UnarchiveAgent {
                req_id,
                id: a.id.clone(),
            },
            "unarchived",
        ),
        AgentCtl::Delete => (
            ClientRequest::DeleteAgent {
                req_id,
                id: a.id.clone(),
            },
            "deleted",
        ),
        AgentCtl::Restart => (
            ClientRequest::RestartAgent {
                req_id,
                id: a.id.clone(),
            },
            "restarted",
        ),
    };
    write_frame(&mut conn.stream, &request).await?;
    await_ack(&mut conn, req_id, &mut Vec::new()).await?;
    println!(
        "{}",
        serde_json::json!({
            "name": a.name,
            "worktree": w.branch,
            "result": verb,
        })
    );
    Ok(())
}

/// `nebula agent new` flags, bundled — the CLI surface mirrors the TUI's
/// new-session picker.
pub struct NewAgentOpts {
    pub worktree: Option<String>,
    pub project: Option<String>,
    pub kind: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub name: Option<String>,
    pub prompt: Option<String>,
}

/// Session name when the caller passed no `--name`: derived from the
/// delegated task's prompt so the row is findable by search (`/`, ⌘K)
/// from the moment it appears — auto-title stays pending, so the worker
/// may still refine it. Fallback: the first free `agent-N` not in
/// `taken`.
fn default_agent_name(prompt: Option<&str>, taken: &[String]) -> String {
    if let Some(title) = prompt.and_then(crate::branch_name::title_from_prompt) {
        return title;
    }
    let mut n = 1;
    loop {
        let candidate = format!("agent-{n}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// `nebula agent new`: spawn a session on the given worktree.
pub async fn agent_new(opts: NewAgentOpts) -> Result<()> {
    let kind = nebula_core::AgentKind::parse(&opts.kind).with_context(|| {
        format!(
            "unknown agent kind {:?} (claude|codex|cursor|pi)",
            opts.kind
        )
    })?;
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
    let (target, expected_branch) = match opts.worktree.as_deref() {
        Some(sel) => match resolve_worktree(&of_project, sel, &project.name, true) {
            Ok(w) => (
                w.clone(),
                (sel != "primary" && sel != "root" && sel == w.branch).then(|| sel.to_owned()),
            ),
            Err(error) => {
                // An explicit checkout selector keeps its meaning, including ambiguity.
                if sel == "root"
                    || sel == "primary"
                    || of_project
                        .iter()
                        .any(|w| w.branch == sel || w.path.file_name().is_some_and(|n| n == sel))
                {
                    return Err(error);
                }
                // A missing selector is branch intent: the owning daemon must
                // use guarded primary resolution, never provision a worktree.
                write_frame(
                    &mut conn.stream,
                    &ClientRequest::ResolveBranchCheckout {
                        req_id: 2,
                        project: project.id.clone(),
                        branch: sel.to_owned(),
                    },
                )
                .await?;
                let mut upserts = Vec::new();
                let created = await_ack(&mut conn, 2, &mut upserts)
                    .await
                    .map_err(|e| with_busy_hint(e, &switch_first_hint(&project, sel)))?;
                let Some(nebula_core::EntityId::Worktree(id)) = created else {
                    bail!("host did not resolve a branch checkout");
                };
                let (_, current, _) = subscribe_snapshot(&mut conn).await?;
                let target = current
                    .into_iter()
                    .find(|w| w.id == id && w.project_id == project.id)
                    .context("host did not report the resolved checkout")?;
                // Keep the original branch intent even if a later snapshot shows
                // a concurrent primary switch. The host must reject that race.
                (target, Some(sel.to_owned()))
            }
        },
        None => bail!("pass --worktree <branch>"),
    };
    let auto_title = opts.name.is_none();
    let name = opts.name.unwrap_or_else(|| {
        let taken: Vec<String> = agents
            .iter()
            .filter(|a| a.worktree_id == target.id)
            .map(|a| a.name.clone())
            .collect();
        default_agent_name(opts.prompt.as_deref(), &taken)
    });
    let req_id = 1u64;
    write_frame(
        &mut conn.stream,
        &ClientRequest::CreateAgent {
            req_id,
            worktree: target.id.clone(),
            expected_branch,
            name: name.clone(),
            kind,
            model: opts.model,
            effort: opts.effort,
            auto_title,
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
        })
    );
    Ok(())
}

/// `nebula agent list [--project <name>] [--all] [--worktree <branch>]`:
/// one JSON array of the project's sessions (or every project's, with
/// --all), status included — the delegating session's view of its workers.
/// --worktree narrows to one checkout, by branch or directory name.
pub async fn agent_list(
    project_flag: Option<String>,
    all: bool,
    worktree_flag: Option<String>,
) -> Result<()> {
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, worktrees, agents) = subscribe_snapshot(&mut conn).await?;
    let scope = if all {
        None
    } else {
        Some(resolve_project(
            &projects,
            &worktrees,
            &agents,
            project_flag.as_deref(),
        )?)
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
            if let Some(sel) = worktree_flag.as_deref() {
                // Same selectors `agent new --worktree` takes.
                let hit = worktree.branch == sel
                    || ((sel == "root" || sel == "primary") && worktree.is_main)
                    || worktree.path.file_name().is_some_and(|n| n == sel);
                if !hit {
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
                "archived": a.archived,
                "session_id": a.session_id,
            }))
        })
        .collect();
    println!("{}", serde_json::Value::Array(rows));
    Ok(())
}

/// A worker has settled once it is out of the busy states: `fresh` (spawned,
/// first turn not started) and `running` both mean "still working" from the
/// delegator's seat; everything else (finished, needs_feedback,
/// terminated, disconnected) is something to act on.
fn agent_settled(status: nebula_core::AgentStatus) -> bool {
    !matches!(
        status,
        nebula_core::AgentStatus::Fresh | nebula_core::AgentStatus::Running
    )
}

/// Why `agent wait` stopped short of its timeout.
pub enum WaitEnd {
    Settled,
    /// A message was submitted to the waiting session itself.
    Message,
}

/// `nebula agent wait [<name>...] [--any] [--timeout <secs>] [--project <name>]`:
/// block until the named workers settle (with --any, until the first one
/// does) — or, with no names, until every unarchived worker of the target
/// project has. Also returns once a message is sent to the calling session.
/// Prints the settled targets as JSON (same shape as `agent list`); errors
/// out past --timeout. Composes the existing Subscribe stream (snapshot +
/// StatusChanged / EntityUpserted deltas) and the inbox file the session's
/// daemon appends to, so no protocol bump — works against a running daemon
/// (one older than the inbox just never ends a wait early).
pub async fn agent_wait(
    names: Vec<String>,
    any: bool,
    timeout_secs: u64,
    project_flag: Option<String>,
) -> Result<WaitEnd> {
    let self_id = std::env::var("NEBULA_AGENT_ID")
        .ok()
        .filter(|v| !v.is_empty());
    // Measured before any await: a message submitted while this wait is
    // still connecting is newer than the baseline and ends it. Anything
    // older reached the session before it called `agent wait`.
    let inbox = self_id.as_deref().map(paths::inbox_path);
    let inbox_len = |inbox: &Option<std::path::PathBuf>| {
        inbox
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len())
    };
    let inbox_start = inbox_len(&inbox);
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    let (projects, mut worktrees, mut agents) = subscribe_snapshot(&mut conn).await?;
    let project = resolve_project(&projects, &worktrees, &agents, project_flag.as_deref())?;

    let in_project = |a: &nebula_core::Agent, worktrees: &[nebula_core::Worktree]| {
        worktrees
            .iter()
            .any(|w| w.id == a.worktree_id && w.project_id == project.id)
    };
    // Which agents to wait on: the named ones, or every unarchived worker of
    // the project (the caller's own session excluded).
    let targets: Vec<nebula_core::AgentId> = if names.is_empty() {
        agents
            .iter()
            .filter(|a| {
                in_project(a, &worktrees)
                    && !a.archived
                    && self_id.as_deref() != Some(a.id.as_str())
            })
            .map(|a| a.id.clone())
            .collect()
    } else {
        names
            .iter()
            .map(|name| {
                agents
                    .iter()
                    .find(|a| a.name == *name && in_project(a, &worktrees))
                    .map(|a| a.id.clone())
                    .with_context(|| {
                        format!(
                            "no agent named \"{name}\" in {} (have: {})",
                            project.name,
                            agents
                                .iter()
                                .filter(|a| in_project(a, &worktrees))
                                .map(|a| a.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
            })
            .collect::<Result<_>>()?
    };

    // Keep the subscription's view of the world current until every target
    // settles (a removed row counts as settled — there is nothing left to
    // wait for). StatusChanged is the signal that matters; upserts keep
    // names/worktrees fresh for the final JSON.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    let pending = |agents: &[nebula_core::Agent]| -> Vec<String> {
        targets
            .iter()
            .filter_map(|id| agents.iter().find(|a| &a.id == id))
            .filter(|a| !agent_settled(a.status))
            .map(|a| a.name.clone())
            .collect()
    };
    let done = |agents: &[nebula_core::Agent]| {
        let pending = pending(agents).len();
        pending == 0 || (any && pending < targets.len())
    };
    // Frames come off a reader task so the inbox check below can interrupt
    // the wait without cancelling a half-read frame.
    let (frame_tx, mut frames) = tokio::sync::mpsc::unbounded_channel();
    let mut stream = conn.stream;
    tokio::spawn(async move {
        loop {
            let frame = read_frame::<ServerEvent, _>(&mut stream).await;
            let last = !matches!(frame, Ok(Some(_)));
            if frame_tx.send(frame).is_err() || last {
                break;
            }
        }
    });
    let mut inbox_tick = tokio::time::interval(Duration::from_millis(500));
    let end = loop {
        if done(&agents) {
            break WaitEnd::Settled;
        }
        if inbox_len(&inbox) > inbox_start {
            break WaitEnd::Message;
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "timed out after {timeout_secs}s waiting for: {}",
                pending(&agents).join(", ")
            );
        }
        let event = tokio::select! {
            frame = frames.recv() => frame.context("daemon connection reader stopped")??,
            _ = inbox_tick.tick() => continue,
            _ = tokio::time::sleep_until(deadline) => continue,
        };
        match event {
            Some(ServerEvent::StatusChanged { agent, status, .. }) => {
                if let Some(a) = agents.iter_mut().find(|a| a.id == agent) {
                    a.status = status;
                }
            }
            Some(ServerEvent::EntityUpserted { entity }) => match entity {
                nebula_core::Entity::Agent(a) => {
                    match agents.iter_mut().find(|old| old.id == a.id) {
                        Some(old) => *old = a,
                        None => agents.push(a),
                    }
                }
                nebula_core::Entity::Worktree(w) => {
                    match worktrees.iter_mut().find(|old| old.id == w.id) {
                        Some(old) => *old = w,
                        None => worktrees.push(w),
                    }
                }
                _ => {}
            },
            Some(ServerEvent::EntityRemoved {
                id: nebula_core::EntityId::Agent(id),
            }) => agents.retain(|a| a.id != id),
            Some(_) => {}
            None => bail!("daemon closed the connection while waiting"),
        }
    };

    let rows: Vec<serde_json::Value> = targets
        .iter()
        .filter_map(|id| {
            let a = agents
                .iter()
                .find(|a| &a.id == id && agent_settled(a.status))?;
            let worktree = worktrees.iter().find(|w| w.id == a.worktree_id)?;
            Some(serde_json::json!({
                "id": a.id.to_string(),
                "name": a.name,
                "kind": a.kind.as_str(),
                "status": a.status.as_str(),
                "project": project.name,
                "worktree": worktree.branch,
                "path": worktree.path.display().to_string(),
                "archived": a.archived,
                "session_id": a.session_id,
            }))
        })
        .collect();
    println!("{}", serde_json::Value::Array(rows));
    Ok(end)
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

pub enum TodoOp {
    List,
    Add { text: String, worktree: bool },
    Done { index: usize },
    Reopen { index: usize },
    Show { index: usize },
    NoteAdd { index: usize, text: String },
    NoteDone { index: usize, note: usize },
}

/// `nebula todo [list|add|done|reopen|show|note|note-done]` — the todo
/// lists (project + current worktree) from the CLI, so agents can read and
/// work the user's task list, including each todo's own notes. Target
/// resolution mirrors `nebula notes`: the caller's session
/// (NEBULA_AGENT_ID) when run inside one, else whatever project/worktree
/// owns the current directory.
pub async fn run_todo(op: TodoOp) -> Result<()> {
    use nebula_core::{Note, NoteOwner, Todo, TodoOwner};
    let sock = paths::socket_path();
    let Ok(stream) = try_connect(&sock).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    write_frame(&mut conn.stream, &ClientRequest::Subscribe).await?;
    let (projects, worktrees, agents, notes, todos) = loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Snapshot {
                projects,
                worktrees,
                agents,
                notes,
                todos,
                ..
            }) => break (projects, worktrees, agents, notes, todos),
            Some(_) => continue,
            None => bail!("daemon closed the connection before the snapshot"),
        }
    };

    // Whose todos: the session's worktree when inside one, else the deepest
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

    // One numbered list: the project's todos, then the worktree's. Indices
    // are what done/reopen/show/note take, recomputed per invocation.
    let mut visible: Vec<&Todo> = todos
        .iter()
        .filter(|t| t.owner == TodoOwner::Project(project.id.clone()))
        .collect();
    let project_count = visible.len();
    if let Some(w) = worktree {
        visible.extend(
            todos
                .iter()
                .filter(|t| t.owner == TodoOwner::Worktree(w.id.clone())),
        );
    }
    // A todo's child notes, in snapshot (per-owner list) order.
    let notes_of = |todo: &Todo| -> Vec<&Note> {
        notes
            .iter()
            .filter(|n| n.owner == NoteOwner::Todo(todo.id.clone()))
            .collect()
    };
    let nth = |index: usize| -> Result<&&Todo> {
        index
            .checked_sub(1)
            .and_then(|i| visible.get(i))
            .with_context(|| {
                format!(
                    "no todo {index} — `nebula todo list` lists {} todo{}",
                    visible.len(),
                    if visible.len() == 1 { "" } else { "s" }
                )
            })
    };

    let req_id = 1u64;
    match op {
        TodoOp::List => {
            if visible.is_empty() {
                println!(
                    "no todos for {} — add one with `nebula todo add <text>`",
                    project.name
                );
                return Ok(());
            }
            for (i, t) in visible.iter().enumerate() {
                if i == 0 && project_count > 0 {
                    println!("{} — project todos", project.name);
                }
                if i == project_count {
                    // Reachable only when a worktree was resolved.
                    println!(
                        "{}/{} — worktree todos",
                        project.name,
                        worktree.map(|w| w.branch.as_str()).unwrap_or("?")
                    );
                }
                let mark = if t.done { "x" } else { " " };
                let child = notes_of(t).len();
                let suffix = match child {
                    0 => String::new(),
                    1 => "  (1 note)".to_string(),
                    n => format!("  ({n} notes)"),
                };
                println!("  {}. [{mark}] {}{suffix}", i + 1, t.text);
            }
            Ok(())
        }
        TodoOp::Add {
            text,
            worktree: to_worktree,
        } => {
            let owner = if to_worktree {
                let w = worktree.context(
                    "--worktree needs a current worktree (run inside a session or a checkout)",
                )?;
                TodoOwner::Worktree(w.id.clone())
            } else {
                TodoOwner::Project(project.id.clone())
            };
            write_frame(
                &mut conn.stream,
                &ClientRequest::CreateTodo {
                    req_id,
                    owner,
                    text: text.clone(),
                },
            )
            .await?;
            await_ack(&mut conn, req_id, &mut Vec::new()).await?;
            println!("todo added: {text}");
            Ok(())
        }
        TodoOp::Done { index } | TodoOp::Reopen { index } => {
            let done = matches!(op, TodoOp::Done { .. });
            let t = nth(index)?;
            write_frame(
                &mut conn.stream,
                &ClientRequest::SetTodoDone {
                    req_id,
                    id: t.id.clone(),
                    done,
                },
            )
            .await?;
            await_ack(&mut conn, req_id, &mut Vec::new()).await?;
            println!(
                "todo {index} {}: {}",
                if done { "done" } else { "reopened" },
                t.text
            );
            Ok(())
        }
        TodoOp::Show { index } => {
            let t = nth(index)?;
            let mark = if t.done { "x" } else { " " };
            println!("{index}. [{mark}] {}", t.text);
            let child = notes_of(t);
            if child.is_empty() {
                println!("   no notes — add one with `nebula todo note {index} <text>`");
            } else {
                for (i, n) in child.iter().enumerate() {
                    let mark = if n.done { "x" } else { " " };
                    println!("   {}. [{mark}] {}", i + 1, n.text);
                }
            }
            Ok(())
        }
        TodoOp::NoteAdd { index, text } => {
            let t = nth(index)?;
            write_frame(
                &mut conn.stream,
                &ClientRequest::CreateNote {
                    req_id,
                    owner: NoteOwner::Todo(t.id.clone()),
                    text: text.clone(),
                },
            )
            .await?;
            await_ack(&mut conn, req_id, &mut Vec::new()).await?;
            println!("note added under todo {index}: {text}");
            Ok(())
        }
        TodoOp::NoteDone { index, note } => {
            let t = nth(index)?;
            let child = notes_of(t);
            let n = note.checked_sub(1).and_then(|i| child.get(i)).with_context(|| {
                format!(
                    "no note {note} under todo {index} — `nebula todo show {index}` lists {} note{}",
                    child.len(),
                    if child.len() == 1 { "" } else { "s" }
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
            println!("todo {index} note {note} done: {}", n.text);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_first_hint_names_the_resolved_project_and_host() {
        let project = |name: &str, host: Option<&str>| nebula_core::Project {
            workspace_id: Default::default(),
            id: nebula_core::ProjectId("p".into()),
            name: name.into(),
            repo_path: "/srv/app".into(),
            sort_order: 0,
            divider_after: false,
            divider_before: false,
            divider_label: None,
            divider_before_label: None,
            host: host.map(str::to_owned),
        };
        assert_eq!(
            switch_first_hint(&project("nebula", Some("findl")), "feature"),
            "run `nebula switch 'feature' --project 'nebula@findl' --stop-busy` first, then retry"
        );
        assert_eq!(
            switch_first_hint(&project("my app", None), "it's"),
            "run `nebula switch 'it'\\''s' --project 'my app' --stop-busy` first, then retry"
        );
    }

    /// Delegating with `--prompt` and no `--name` gets a session named
    /// after the task, not `agent-N` — the name is what the user searches
    /// by.
    #[test]
    fn unnamed_prompted_agents_are_named_after_the_task() {
        assert_eq!(
            default_agent_name(
                Some("please fix the login redirect flow"),
                &["agent-1".into(), "agent-2".into(), "agent-3".into()]
            ),
            "Fix Login Redirect Flow"
        );
    }

    /// Numbered fallbacks take the first FREE slot among the taken names —
    /// a deleted agent-2 is reused, an existing agent-3 is never
    /// duplicated.
    #[test]
    fn no_prompt_or_empty_prompt_falls_back_to_numbered_names() {
        assert_eq!(default_agent_name(None, &["agent-1".into()]), "agent-2");
        assert_eq!(default_agent_name(Some("the…"), &[]), "agent-1");
        assert_eq!(
            default_agent_name(None, &["agent-1".into(), "agent-3".into()]),
            "agent-2"
        );
    }

    #[test]
    fn a_send_goes_raw_up_to_1000_bytes_on_one_line() {
        let line = "x".repeat(1000);
        assert_eq!(send_payload(&line, false), Ok(line.into_bytes()));
    }

    #[test]
    fn a_send_past_1000_bytes_or_with_a_newline_is_refused_and_names_the_pointer() {
        for text in ["x".repeat(1001), "two\nlines".to_string()] {
            let why = send_payload(&text, false).unwrap_err();
            assert!(
                why.contains(".scratch/") && why.contains("Read <path> and act on it."),
                "{why}"
            );
        }
    }

    #[test]
    fn paste_sends_long_or_multi_line_text_as_one_bracketed_paste() {
        let long = "x".repeat(1001);
        assert_eq!(
            send_payload(&long, true),
            Ok(format!("\x1b[200~{long}\x1b[201~").into_bytes())
        );
        assert_eq!(
            send_payload("two\nlines", true),
            Ok(b"\x1b[200~two\nlines\x1b[201~".to_vec())
        );
    }

    #[test]
    fn send_file_to_a_remote_project_points_at_the_hosts_absolute_path_as_given() {
        let remote = project_flag_host(Some("nebula@findl")).is_some();
        assert_eq!(
            file_pointer(
                Path::new("/home/nebula/nowhere-on-this-laptop/msg.md"),
                remote
            ),
            Ok("Read /home/nebula/nowhere-on-this-laptop/msg.md and act on it.".to_string())
        );
    }

    #[test]
    fn send_file_to_a_remote_project_refuses_a_relative_path_and_names_the_fix() {
        let remote = project_flag_host(Some("nebula@findl")).is_some();
        let why = file_pointer(Path::new(".scratch/msg.md"), remote).unwrap_err();
        assert!(
            why.contains("nebula sync") && why.contains("absolute path"),
            "{why}"
        );
    }

    #[test]
    fn send_file_to_a_local_project_checks_and_canonicalizes_the_file_here() {
        for flag in [None, Some("nebula"), Some("@findl"), Some("nebula@")] {
            assert_eq!(project_flag_host(flag), None, "{flag:?}");
        }
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert_eq!(
            file_pointer(&manifest, false),
            Ok(format!(
                "Read {} and act on it.",
                manifest.canonicalize().unwrap().display()
            ))
        );
        assert!(file_pointer(
            Path::new("/home/nebula/nowhere-on-this-laptop/msg.md"),
            false
        )
        .is_err());
        assert!(file_pointer(Path::new(env!("CARGO_MANIFEST_DIR")), false).is_err());
    }
}
