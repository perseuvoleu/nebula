//! Relay: a remote project's sessions live in the *host's* daemon; this
//! daemon mirrors them and forwards what its clients do to them.
//!
//! Why not spawn the CLI over ssh from here (the first remote design)? Then
//! the session's PTY is a local ssh client, and closing the laptop kills
//! the agent mid-task. Here the host's daemon owns the PTY: the laptop is
//! only ever a viewer, the agent keeps working, and coming back is a fresh
//! attach to a still-running screen.
//!
//! **Transport.** One byte pipe per host, speaking the ordinary nebula
//! protocol (`Hello`, `Subscribe`, frames): today `ssh host nebula proxy`,
//! which pumps stdin/stdout to the host's daemon socket (and boots the
//! daemon if needed); tomorrow anything that carries bytes — a TCP port on
//! a tailnet, say. `NEBULA_RELAY_CMD` swaps the command for tests.
//!
//! **Mirror.** The relay subscribes to the host daemon and keeps the
//! entities under the projects this daemon *wants* from that host (the
//! `host:/path` rows in the local store), re-broadcasting each upsert,
//! removal and status change to local subscribers with the project stamped
//! `host` so the panels badge it. The host daemon's ids are used verbatim
//! (ULIDs, globally unique), which is what makes routing trivial: a client
//! request that names a mirrored id belongs to that host.
//!
//! **Forwarding.** Requests naming mirrored ids go to the host daemon.
//! RPCs get a relay-owned `req_id`, and the reply comes back to the asking
//! client under its own; `Attach` registers the client for that session's
//! PTY stream, which the relay fans out as frames arrive. A dropped link
//! reconnects with backoff, re-subscribes (a fresh snapshot diffs against
//! the mirror), and re-attaches every session a client still watches, so
//! the pane repaints from the live ring instead of going dark.

use crate::registry::Daemon;
use anyhow::{bail, Context, Result};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{
    Agent, ClientRequest, Entity, EntityId, Link, Note, Project, ServerEvent, SessionRef,
    TerminalTab, Todo, Worktree, PROTOCOL_VERSION,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;

/// What this daemon shows of one host.
#[derive(Default, Clone)]
pub struct Mirror {
    pub projects: Vec<Project>,
    pub worktrees: Vec<Worktree>,
    pub agents: Vec<Agent>,
    pub terminals: Vec<TerminalTab>,
    pub notes: Vec<Note>,
    pub todos: Vec<Todo>,
    pub links: Vec<Link>,
    /// Every id above, as the string the protocol carries — the routing key.
    pub ids: HashSet<String>,
}

pub struct Relay {
    pub host: String,
    /// Frames bound for the host daemon; the run loop writes them.
    out: mpsc::Sender<ClientRequest>,
    pub mirror: Mutex<Mirror>,
    /// Clients watching a mirrored session's PTY, by session.
    attached: Mutex<HashMap<SessionRef, Vec<(mpsc::Sender<ServerEvent>, (u16, u16))>>>,
    /// Relay req_id → (asking client, its own req_id).
    pending: Mutex<HashMap<u64, (mpsc::Sender<ServerEvent>, u64)>>,
    next_req: AtomicU64,
    pub connected: AtomicBool,
    /// AddProject requests this relay sent for wanted paths the host lacked,
    /// by relay req_id → the path asked for.
    pending_adds: Mutex<HashMap<u64, PathBuf>>,
    /// Host project ids in scope regardless of path: the ones the host
    /// adopted on our request (its toplevel may spell the path differently
    /// — `/private/tmp` for `/tmp` on macOS — until the anchor is rewritten).
    adopted: Mutex<HashSet<String>>,
    /// Everything the host reports, in scope or not, kept current from its
    /// deltas; the mirror is a pure function of it (`compute_scope`).
    host_tree: Mutex<Mirror>,
}

const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

impl Relay {
    pub fn spawn(daemon: Arc<Daemon>, host: String) -> Arc<Relay> {
        let (out, out_rx) = mpsc::channel(256);
        let relay = Arc::new(Relay {
            host,
            out,
            mirror: Mutex::new(Mirror::default()),
            attached: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            connected: AtomicBool::new(false),
            pending_adds: Mutex::new(HashMap::new()),
            adopted: Mutex::new(HashSet::new()),
            host_tree: Mutex::new(Mirror::default()),
        });
        tokio::spawn(run(relay.clone(), daemon, out_rx));
        relay
    }

    /// Does this request name something mirrored from this host?
    pub fn owns_request(&self, req: &ClientRequest) -> bool {
        let ids = self.mirror.lock().unwrap().ids.clone();
        request_ids(req).iter().any(|id| ids.contains(id))
    }

    pub fn owns_session(&self, sref: &SessionRef) -> bool {
        let id = match sref {
            SessionRef::Agent(id) => id.as_str().to_string(),
            SessionRef::Terminal(id) => id.as_str().to_string(),
        };
        self.mirror.lock().unwrap().ids.contains(&id)
    }

    /// Hand a client's request to the host daemon. `client` is the
    /// connection's writer: replies and PTY frames come back through it.
    pub async fn forward(&self, req: ClientRequest, client: &mpsc::Sender<ServerEvent>) {
        let req = match req {
            ClientRequest::Attach {
                session,
                from_seq,
                cols,
                rows,
            } => {
                let mut attached = self.attached.lock().unwrap();
                let watchers = attached.entry(session.clone()).or_default();
                watchers.retain(|(tx, _)| !tx.is_closed() && !tx.same_channel(client));
                watchers.push((client.clone(), (cols, rows)));
                ClientRequest::Attach {
                    session,
                    from_seq,
                    cols,
                    rows,
                }
            }
            ClientRequest::Detach { session } => {
                let mut attached = self.attached.lock().unwrap();
                let Some(watchers) = attached.get_mut(&session) else {
                    return;
                };
                watchers.retain(|(tx, _)| !tx.is_closed() && !tx.same_channel(client));
                if !watchers.is_empty() {
                    return; // someone else still watches; the host keeps streaming
                }
                attached.remove(&session);
                ClientRequest::Detach { session }
            }
            other => match request_req_id(&other) {
                Some(local) => {
                    let remote = self.next_req.fetch_add(1, Ordering::Relaxed);
                    self.pending
                        .lock()
                        .unwrap()
                        .insert(remote, (client.clone(), local));
                    match with_req_id(&other, remote) {
                        Some(r) => r,
                        None => other,
                    }
                }
                None => other,
            },
        };
        if !self.connected.load(Ordering::Relaxed) {
            let waiting = request_req_id(&req)
                .and_then(|remote| self.pending.lock().unwrap().remove(&remote));
            if let Some((client, local)) = waiting {
                let _ = client
                    .send(ServerEvent::Error {
                        req_id: Some(local),
                        message: format!("{} is unreachable right now", self.host),
                    })
                    .await;
            }
            return;
        }
        let _ = self.out.send(req).await;
    }

    /// Deliver a PTY-plane frame to the clients watching that session.
    async fn fan_out(&self, session: &SessionRef, ev: ServerEvent) {
        let watchers: Vec<mpsc::Sender<ServerEvent>> = {
            let mut attached = self.attached.lock().unwrap();
            let Some(list) = attached.get_mut(session) else {
                return;
            };
            list.retain(|(tx, _)| !tx.is_closed());
            list.iter().map(|(tx, _)| tx.clone()).collect()
        };
        for tx in watchers {
            let _ = tx.send(ev.clone()).await;
        }
    }

    /// Route a reply to the client whose request it answers, under that
    /// client's own req_id.
    async fn answer(&self, remote_req: u64, ev: ServerEvent) {
        let Some((client, local)) = self.pending.lock().unwrap().remove(&remote_req) else {
            return;
        };
        let ev = match ev {
            ServerEvent::Ack { created, .. } => ServerEvent::Ack {
                req_id: local,
                created,
            },
            ServerEvent::Error { message, .. } => ServerEvent::Error {
                req_id: Some(local),
                message,
            },
            ServerEvent::Metrics { snapshot, .. } => ServerEvent::Metrics {
                req_id: local,
                snapshot,
            },
            ServerEvent::SessionText {
                cols, rows, text, ..
            } => ServerEvent::SessionText {
                req_id: local,
                cols,
                rows,
                text,
            },
            other => other,
        };
        let _ = client.send(ev).await;
    }
}

/// The command whose stdin/stdout reach the host daemon's socket.
/// `NEBULA_RELAY_CMD` (tests) is a shell line with `{host}` substituted;
/// otherwise `ssh host nebula proxy`, BatchMode so a daemon never hangs
/// on a prompt, keepalives so a dead link is noticed within a minute.
fn relay_command(host: &str) -> tokio::process::Command {
    if let Ok(tmpl) = std::env::var("NEBULA_RELAY_CMD") {
        let line = tmpl.replace("{host}", host);
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", &line]);
        return cmd;
    }
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(nebula_core::remote::SSH_BATCH_OPTS);
    cmd.args([
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ControlPath=none",
        "--",
        host,
        "export PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:$PATH\"; exec nebula proxy",
    ]);
    cmd
}

async fn run(relay: Arc<Relay>, daemon: Arc<Daemon>, mut out_rx: mpsc::Receiver<ClientRequest>) {
    let mut backoff = RECONNECT_MIN;
    loop {
        if daemon.shutdown.is_cancelled() {
            return;
        }
        match connect_and_pump(&relay, &daemon, &mut out_rx).await {
            Ok(()) => backoff = RECONNECT_MIN,
            Err(e) => {
                tracing::warn!(host = %relay.host, error = %e, "relay link down");
            }
        }
        relay.connected.store(false, Ordering::Relaxed);
        // Pending RPCs can't be answered by a link that died under them.
        let pending: Vec<_> = relay.pending.lock().unwrap().drain().collect();
        for (_, (client, local)) in pending {
            let _ = client
                .send(ServerEvent::Error {
                    req_id: Some(local),
                    message: format!("{}: connection lost", relay.host),
                })
                .await;
        }
        tokio::select! {
            _ = daemon.shutdown.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// One connection's lifetime: handshake, subscribe, then pump both ways
/// until the link breaks. Ok on a clean EOF, Err on anything else.
async fn connect_and_pump(
    relay: &Arc<Relay>,
    daemon: &Arc<Daemon>,
    out_rx: &mut mpsc::Receiver<ClientRequest>,
) -> Result<()> {
    let mut child = relay_command(&relay.host)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("spawn relay transport")?;
    let mut writer = BufWriter::new(child.stdin.take().context("relay stdin")?);
    let mut reader = BufReader::new(child.stdout.take().context("relay stdout")?);

    write_frame(
        &mut writer,
        &ClientRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    match read_frame::<ServerEvent, _>(&mut reader).await? {
        Some(ServerEvent::HelloOk { .. }) => {}
        Some(ServerEvent::Incompatible {
            daemon_protocol_version,
        }) => bail!(
            "{} runs protocol {daemon_protocol_version}, this daemon {PROTOCOL_VERSION} — upgrade nebula on one side",
            relay.host
        ),
        other => bail!("unexpected handshake reply: {other:?}"),
    }
    write_frame(&mut writer, &ClientRequest::Subscribe).await?;
    relay.connected.store(true, Ordering::Relaxed);
    tracing::info!(host = %relay.host, "relay link up");

    loop {
        tokio::select! {
            frame = read_frame::<ServerEvent, _>(&mut reader) => {
                match frame? {
                    None => return Ok(()),
                    Some(ev) => {
                        if let Some(reqs) = handle_event(relay, daemon, ev).await {
                            for r in reqs {
                                write_frame(&mut writer, &r).await?;
                            }
                        }
                    }
                }
            }
            req = out_rx.recv() => {
                match req {
                    Some(r) => write_frame(&mut writer, &r).await?,
                    None => return Ok(()),
                }
            }
            _ = daemon.shutdown.cancelled() => {
                let _ = writer.shutdown().await;
                return Ok(());
            }
        }
    }
}

/// Does a host project cover a wanted path? Equal, or the wanted path is
/// inside it — the host normalizes what it was handed to the repo's
/// toplevel, so `host:~/repo/sub` is the project at `~/repo`.
fn covers(project_root: &std::path::Path, wanted: &[PathBuf]) -> bool {
    wanted.iter().any(|w| w.starts_with(project_root))
}

/// Repo paths this daemon wants mirrored from `host`.
fn wanted_paths(daemon: &Daemon, host: &str) -> Vec<PathBuf> {
    daemon
        .store
        .load_tree()
        .map(|(projects, ..)| {
            projects
                .into_iter()
                .filter(|p| p.host.as_deref() == Some(host))
                .map(|p| p.repo_path)
                .collect()
        })
        .unwrap_or_default()
}

/// The slice of the host tree this daemon shows: projects that cover a
/// wanted path or were adopted on our request, and everything under them.
fn compute_scope(host: &Mirror, wanted: &[PathBuf], adopted: &HashSet<String>, host_name: &str) -> Mirror {
    let mut m = Mirror::default();
    for p in &host.projects {
        if covers(&p.repo_path, wanted) || adopted.contains(p.id.as_str()) {
            let mut p = p.clone();
            p.host = Some(host_name.to_string());
            m.upsert(Entity::Project(p));
        }
    }
    for w in &host.worktrees {
        if m.ids.contains(w.project_id.as_str()) {
            m.upsert(Entity::Worktree(w.clone()));
        }
    }
    for a in &host.agents {
        if m.ids.contains(a.worktree_id.as_str()) {
            m.upsert(Entity::Agent(a.clone()));
        }
    }
    for t in &host.terminals {
        if m.ids.contains(t.worktree_id.as_str()) {
            m.upsert(Entity::Terminal(t.clone()));
        }
    }
    for l in &host.links {
        if m.ids.contains(l.worktree_id.as_str()) {
            m.upsert(Entity::Link(l.clone()));
        }
    }
    for t in &host.todos {
        if parent_ids(&Entity::Todo(t.clone())).iter().any(|id| m.ids.contains(id)) {
            m.upsert(Entity::Todo(t.clone()));
        }
    }
    for n in &host.notes {
        if parent_ids(&Entity::Note(n.clone())).iter().any(|id| m.ids.contains(id)) {
            m.upsert(Entity::Note(n.clone()));
        }
    }
    m
}

/// Replace the mirror with a freshly computed scope and tell subscribers
/// what changed: rows that left, rows that arrived — and, after a
/// reconnect (`announce_all`), every row, since values may have moved
/// while the link was down.
fn apply_scope(relay: &Relay, daemon: &Daemon, fresh: Mirror, announce_all: bool) {
    let old = std::mem::replace(&mut *relay.mirror.lock().unwrap(), fresh.clone());
    for id in old.all_entity_ids() {
        if !fresh.ids.contains(&entity_id_string(&id)) {
            daemon.broadcast(ServerEvent::EntityRemoved { id });
        }
    }
    for entity in fresh.entities() {
        if announce_all || !old.ids.contains(&entity_own_id(&entity)) {
            daemon.broadcast(ServerEvent::EntityUpserted { entity });
        }
    }
    for p in &fresh.projects {
        normalize_anchor(daemon, &relay.host, p);
    }
    register_paths(&fresh, &relay.host);
}

/// Recompute scope from the host tree (after the adopted set or the
/// anchors changed) and announce the difference.
fn rescope(relay: &Relay, daemon: &Daemon, announce_all: bool) {
    let wanted = wanted_paths(daemon, &relay.host);
    let adopted = relay.adopted.lock().unwrap().clone();
    let fresh = {
        let host = relay.host_tree.lock().unwrap();
        compute_scope(&host, &wanted, &adopted, &relay.host)
    };
    apply_scope(relay, daemon, fresh, announce_all);
}

/// The anchor row for a mirrored project takes the host's own spelling of
/// the repo root (`host:~/repo/sub`, or `/tmp` vs `/private/tmp`), so the
/// next boot matches it by path alone.
fn normalize_anchor(daemon: &Daemon, host: &str, p: &Project) {
    let Ok((projects, ..)) = daemon.store.load_tree() else {
        return;
    };
    for anchor in projects.iter().filter(|a| a.host.as_deref() == Some(host)) {
        let inside = anchor.repo_path.starts_with(&p.repo_path);
        let same_dir = anchor.repo_path.file_name() == p.repo_path.file_name();
        if anchor.repo_path != p.repo_path && (inside || same_dir) {
            let _ = daemon.store.set_project_repo_path(&anchor.id, &p.repo_path);
        }
    }
}

/// Apply one host event to the host tree and the mirror, and to local
/// subscribers. Returns requests the relay should send back (AddProject
/// for a wanted path the host lacks, re-attaches after a fresh snapshot).
async fn handle_event(
    relay: &Arc<Relay>,
    daemon: &Arc<Daemon>,
    ev: ServerEvent,
) -> Option<Vec<ClientRequest>> {
    match ev {
        ServerEvent::Snapshot {
            projects,
            worktrees,
            agents,
            terminals,
            notes,
            todos,
            links,
            ..
        } => {
            let mut tree = Mirror::default();
            for e in projects.into_iter().map(Entity::Project) {
                tree.upsert(e);
            }
            for e in worktrees.into_iter().map(Entity::Worktree) {
                tree.upsert(e);
            }
            for e in agents.into_iter().map(Entity::Agent) {
                tree.upsert(e);
            }
            for e in terminals.into_iter().map(Entity::Terminal) {
                tree.upsert(e);
            }
            for e in notes.into_iter().map(Entity::Note) {
                tree.upsert(e);
            }
            for e in todos.into_iter().map(Entity::Todo) {
                tree.upsert(e);
            }
            for e in links.into_iter().map(Entity::Link) {
                tree.upsert(e);
            }
            *relay.host_tree.lock().unwrap() = tree;
            rescope(relay, daemon, true);

            let mut follow_ups = Vec::new();
            let wanted = wanted_paths(daemon, &relay.host);
            let known: Vec<PathBuf> = relay
                .mirror
                .lock()
                .unwrap()
                .projects
                .iter()
                .map(|p| p.repo_path.clone())
                .collect();
            for path in &wanted {
                if !known.iter().any(|root| path.starts_with(root)) && !relay.pending_adds.lock().unwrap().values().any(|p| p == path) {
                    // The host has never seen this checkout: register it
                    // there; the Ack's id enters the adopted set.
                    let req_id = relay.next_req.fetch_add(1, Ordering::Relaxed);
                    relay.pending_adds.lock().unwrap().insert(req_id, path.clone());
                    follow_ups.push(ClientRequest::AddProject {
                        req_id,
                        path: path.clone(),
                        name: None,
                        create_missing: false,
                        host: None,
                    });
                }
            }
            // Re-attach what clients still watch: the host replays each
            // ring and the panes repaint.
            let attached = relay.attached.lock().unwrap();
            for (session, watchers) in attached.iter() {
                if let Some((_, (cols, rows))) = watchers.iter().find(|(tx, _)| !tx.is_closed()) {
                    follow_ups.push(ClientRequest::Attach {
                        session: session.clone(),
                        from_seq: None,
                        cols: *cols,
                        rows: *rows,
                    });
                }
            }
            Some(follow_ups)
        }
        ServerEvent::EntityUpserted { entity } => {
            relay.host_tree.lock().unwrap().upsert(entity.clone());
            let project_arrived = matches!(&entity, Entity::Project(p)
                if !relay.mirror.lock().unwrap().ids.contains(p.id.as_str()));
            if project_arrived {
                // A project entering scope brings its subtree along.
                rescope(relay, daemon, false);
                return None;
            }
            let mut mirror = relay.mirror.lock().unwrap();
            let in_scope = match &entity {
                Entity::Project(p) => mirror.ids.contains(p.id.as_str()),
                other => parent_ids(other).iter().any(|id| mirror.ids.contains(id)),
            };
            if !in_scope {
                return None;
            }
            let mut entity = entity;
            if let Entity::Project(p) = &mut entity {
                p.host = Some(relay.host.clone());
            }
            mirror.upsert(entity.clone());
            register_paths(&mirror, &relay.host);
            drop(mirror);
            daemon.broadcast(ServerEvent::EntityUpserted { entity });
            None
        }
        ServerEvent::EntityRemoved { id } => {
            relay.host_tree.lock().unwrap().remove(&id);
            let mut mirror = relay.mirror.lock().unwrap();
            if !mirror.ids.contains(&entity_id_string(&id)) {
                return None;
            }
            mirror.remove(&id);
            drop(mirror);
            daemon.broadcast(ServerEvent::EntityRemoved { id });
            None
        }
        ServerEvent::StatusChanged {
            agent,
            status,
            changed_at,
        } => {
            if let Some(a) = relay.host_tree.lock().unwrap().agents.iter_mut().find(|a| a.id == agent) {
                a.status = status;
                a.status_changed_at = changed_at;
            }
            let mut mirror = relay.mirror.lock().unwrap();
            let Some(a) = mirror.agents.iter_mut().find(|a| a.id == agent) else {
                return None;
            };
            a.status = status;
            a.status_changed_at = changed_at;
            drop(mirror);
            daemon.broadcast(ServerEvent::StatusChanged {
                agent,
                status,
                changed_at,
            });
            None
        }
        ServerEvent::Ack { req_id, created } => {
            let asked = relay.pending_adds.lock().unwrap().remove(&req_id);
            if asked.is_some() {
                // Our own AddProject: the host's id is in scope from now on.
                if let Some(EntityId::Project(id)) = &created {
                    relay.adopted.lock().unwrap().insert(id.as_str().to_string());
                    rescope(relay, daemon, false);
                }
                return None;
            }
            relay.answer(req_id, ServerEvent::Ack { req_id, created }).await;
            None
        }
        ServerEvent::Error {
            req_id: Some(r),
            message,
        } => {
            let asked = relay.pending_adds.lock().unwrap().remove(&r);
            if let Some(path) = asked {
                if message.contains("already added") {
                    // The host knows the repo under another spelling of the
                    // path (a symlinked /tmp, say): adopt the host project
                    // with that directory name. The anchor is rewritten to
                    // the host's spelling right after, so this bridge is
                    // crossed once.
                    let name = path.file_name().map(|n| n.to_os_string());
                    let candidate = relay
                        .host_tree
                        .lock()
                        .unwrap()
                        .projects
                        .iter()
                        .find(|p| p.repo_path.file_name().map(|n| n.to_os_string()) == name && p.host.is_none())
                        .map(|p| p.id.as_str().to_string());
                    match candidate {
                        Some(id) => {
                            relay.adopted.lock().unwrap().insert(id);
                            rescope(relay, daemon, false);
                        }
                        None => tracing::warn!(host = %relay.host, path = %path.display(), "host says the repo is already added but no project matches its name"),
                    }
                } else {
                    tracing::warn!(host = %relay.host, path = %path.display(), error = %message, "host refused the checkout");
                }
                return None;
            }
            relay
                .answer(
                    r,
                    ServerEvent::Error {
                        req_id: Some(r),
                        message,
                    },
                )
                .await;
            None
        }
        ev @ ServerEvent::Metrics { .. } => {
            if let ServerEvent::Metrics { req_id, .. } = &ev {
                let r = *req_id;
                relay.answer(r, ev).await;
            }
            None
        }
        ev @ ServerEvent::SessionText { .. } => {
            if let ServerEvent::SessionText { req_id, .. } = &ev {
                let r = *req_id;
                relay.answer(r, ev).await;
            }
            None
        }
        ev @ (ServerEvent::Scrollback { .. }
        | ServerEvent::Output { .. }
        | ServerEvent::SessionExited { .. }
        | ServerEvent::KittyFlags { .. }) => {
            let session = match &ev {
                ServerEvent::Scrollback { session, .. }
                | ServerEvent::Output { session, .. }
                | ServerEvent::SessionExited { session, .. }
                | ServerEvent::KittyFlags { session, .. } => session.clone(),
                _ => unreachable!(),
            };
            relay.fan_out(&session, ev).await;
            None
        }
        // HelloOk/Incompatible are handled at connect; the host's workspace
        // switching and unaddressed errors are its own business.
        _ => None,
    }
}

impl Mirror {
    fn entities(&self) -> Vec<Entity> {
        let mut out = Vec::new();
        out.extend(self.projects.iter().cloned().map(Entity::Project));
        out.extend(self.worktrees.iter().cloned().map(Entity::Worktree));
        out.extend(self.agents.iter().cloned().map(Entity::Agent));
        out.extend(self.terminals.iter().cloned().map(Entity::Terminal));
        out.extend(self.notes.iter().cloned().map(Entity::Note));
        out.extend(self.todos.iter().cloned().map(Entity::Todo));
        out.extend(self.links.iter().cloned().map(Entity::Link));
        out
    }

    fn all_entity_ids(&self) -> Vec<EntityId> {
        let mut out = Vec::new();
        out.extend(self.projects.iter().map(|p| EntityId::Project(p.id.clone())));
        out.extend(self.worktrees.iter().map(|w| EntityId::Worktree(w.id.clone())));
        out.extend(self.agents.iter().map(|a| EntityId::Agent(a.id.clone())));
        out.extend(self.terminals.iter().map(|t| EntityId::Terminal(t.id.clone())));
        out.extend(self.notes.iter().map(|n| EntityId::Note(n.id.clone())));
        out.extend(self.todos.iter().map(|t| EntityId::Todo(t.id.clone())));
        out.extend(self.links.iter().map(|l| EntityId::Link(l.id.clone())));
        out
    }

    fn upsert(&mut self, entity: Entity) {
        fn put<T: Clone>(list: &mut Vec<T>, item: T, same: impl Fn(&T) -> bool) {
            match list.iter_mut().find(|x| same(x)) {
                Some(slot) => *slot = item,
                None => list.push(item),
            }
        }
        let id = entity_own_id(&entity);
        self.ids.insert(id);
        match entity {
            Entity::Project(p) => put(&mut self.projects, p.clone(), |x| x.id == p.id),
            Entity::Worktree(w) => put(&mut self.worktrees, w.clone(), |x| x.id == w.id),
            Entity::Agent(a) => put(&mut self.agents, a.clone(), |x| x.id == a.id),
            Entity::Terminal(t) => put(&mut self.terminals, t.clone(), |x| x.id == t.id),
            Entity::Note(n) => put(&mut self.notes, n.clone(), |x| x.id == n.id),
            Entity::Todo(t) => put(&mut self.todos, t.clone(), |x| x.id == t.id),
            Entity::Link(l) => put(&mut self.links, l.clone(), |x| x.id == l.id),
            Entity::Workspace(_) => {}
        }
    }

    fn remove(&mut self, id: &EntityId) {
        self.ids.remove(&entity_id_string(id));
        match id {
            EntityId::Project(i) => self.projects.retain(|x| &x.id != i),
            EntityId::Worktree(i) => self.worktrees.retain(|x| &x.id != i),
            EntityId::Agent(i) => self.agents.retain(|x| &x.id != i),
            EntityId::Terminal(i) => self.terminals.retain(|x| &x.id != i),
            EntityId::Note(i) => self.notes.retain(|x| &x.id != i),
            EntityId::Todo(i) => self.todos.retain(|x| &x.id != i),
            EntityId::Link(i) => self.links.retain(|x| &x.id != i),
            EntityId::Workspace(_) => {}
        }
    }
}

/// Teach the path→host map every mirrored checkout, so the panels' git
/// hops for it go over ssh.
fn register_paths(m: &Mirror, host: &str) {
    for p in &m.projects {
        nebula_core::remote::register(&p.repo_path, host);
    }
    for w in &m.worktrees {
        nebula_core::remote::register(&w.path, host);
    }
}

pub fn entity_id_string(id: &EntityId) -> String {
    match id {
        EntityId::Workspace(i) => i.as_str().to_string(),
        EntityId::Project(i) => i.as_str().to_string(),
        EntityId::Worktree(i) => i.as_str().to_string(),
        EntityId::Agent(i) => i.as_str().to_string(),
        EntityId::Terminal(i) => i.as_str().to_string(),
        EntityId::Note(i) => i.as_str().to_string(),
        EntityId::Link(i) => i.as_str().to_string(),
        EntityId::Todo(i) => i.as_str().to_string(),
    }
}

fn entity_own_id(e: &Entity) -> String {
    match e {
        Entity::Workspace(w) => w.id.as_str().to_string(),
        Entity::Project(p) => p.id.as_str().to_string(),
        Entity::Worktree(w) => w.id.as_str().to_string(),
        Entity::Agent(a) => a.id.as_str().to_string(),
        Entity::Terminal(t) => t.id.as_str().to_string(),
        Entity::Note(n) => n.id.as_str().to_string(),
        Entity::Link(l) => l.id.as_str().to_string(),
        Entity::Todo(t) => t.id.as_str().to_string(),
    }
}

/// The ids an entity hangs under (its scope parents).
fn parent_ids(e: &Entity) -> Vec<String> {
    match e {
        Entity::Workspace(_) | Entity::Project(_) => vec![],
        Entity::Worktree(w) => vec![w.project_id.as_str().to_string()],
        Entity::Agent(a) => vec![a.worktree_id.as_str().to_string()],
        Entity::Terminal(t) => vec![t.worktree_id.as_str().to_string()],
        Entity::Link(l) => vec![l.worktree_id.as_str().to_string()],
        Entity::Note(n) => vec![match &n.owner {
            nebula_core::NoteOwner::Project(i) => i.as_str().to_string(),
            nebula_core::NoteOwner::Worktree(i) => i.as_str().to_string(),
            nebula_core::NoteOwner::Todo(i) => i.as_str().to_string(),
        }],
        Entity::Todo(t) => vec![match &t.owner {
            nebula_core::TodoOwner::Project(i) => i.as_str().to_string(),
            nebula_core::TodoOwner::Worktree(i) => i.as_str().to_string(),
        }],
    }
}

/// The id-shaped values a request names: `id`, `worktree`, `project`,
/// `session` (both variants), `owner`. Read off the serialized form so a
/// new request variant with those field names routes without a new match
/// arm — and one with new names fails loud in the routing test.
pub fn request_ids(req: &ClientRequest) -> Vec<String> {
    const KEYS: [&str; 5] = ["id", "worktree", "project", "session", "owner"];
    let Ok(v) = serde_json::to_value(req) else {
        return vec![];
    };
    let mut out = Vec::new();
    fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Object(m) => m.values().for_each(|x| collect(x, out)),
            serde_json::Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
            _ => {}
        }
    }
    if let serde_json::Value::Object(variant) = &v {
        for body in variant.values() {
            if let serde_json::Value::Object(fields) = body {
                for k in KEYS {
                    if let Some(x) = fields.get(k) {
                        collect(x, &mut out);
                    }
                }
            }
        }
    }
    out
}

/// The RPC id a request carries, if it is an RPC.
pub fn request_req_id(req: &ClientRequest) -> Option<u64> {
    let v = serde_json::to_value(req).ok()?;
    let variant = v.as_object()?;
    let body = variant.values().next()?.as_object()?;
    body.get("req_id")?.as_u64()
}

/// The same request under another req_id.
fn with_req_id(req: &ClientRequest, req_id: u64) -> Option<ClientRequest> {
    let mut v = serde_json::to_value(req).ok()?;
    let body = v.as_object_mut()?.values_mut().next()?.as_object_mut()?;
    body.insert("req_id".into(), serde_json::Value::from(req_id));
    serde_json::from_value(v).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nebula_core::{AgentId, WorktreeId};

    #[test]
    fn request_ids_cover_every_addressing_shape() {
        let wt = WorktreeId("w1".into());
        let a = AgentId("a1".into());
        assert_eq!(
            request_ids(&ClientRequest::Attach {
                session: SessionRef::Agent(a.clone()),
                from_seq: None,
                cols: 1,
                rows: 1
            }),
            vec!["a1"]
        );
        assert_eq!(
            request_ids(&ClientRequest::CreateTerminal {
                req_id: 7,
                worktree: wt.clone(),
                name: None
            }),
            vec!["w1"]
        );
        assert_eq!(
            request_ids(&ClientRequest::MoveAgent {
                req_id: 1,
                id: a,
                worktree: wt
            }),
            vec!["a1", "w1"]
        );
        // Subscribe names nothing: never routed.
        assert!(request_ids(&ClientRequest::Subscribe).is_empty());
    }

    #[test]
    fn req_id_round_trips_through_rewrite() {
        let req = ClientRequest::CreateTerminal {
            req_id: 7,
            worktree: WorktreeId("w1".into()),
            name: Some("t".into()),
        };
        assert_eq!(request_req_id(&req), Some(7));
        let re = with_req_id(&req, 99).unwrap();
        assert_eq!(request_req_id(&re), Some(99));
        assert!(matches!(re, ClientRequest::CreateTerminal { worktree, .. } if worktree.as_str() == "w1"));
        assert_eq!(request_req_id(&ClientRequest::Subscribe), None);
    }
}
