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
use tokio::io::{AsyncRead, AsyncWrite, BufReader, BufWriter};
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

/// A local connection receiving remote replies and ordered terminal frames.
/// Overflow closes the entire connection; reconnecting clients get a snapshot.
#[derive(Clone)]
pub struct RelayClient {
    tx: mpsc::Sender<ServerEvent>,
    closed: tokio_util::sync::CancellationToken,
}

impl RelayClient {
    /// Bind delivery to the server connection's cancellation signal.
    pub fn new(tx: mpsc::Sender<ServerEvent>, closed: tokio_util::sync::CancellationToken) -> Self {
        Self { tx, closed }
    }

    fn is_closed(&self) -> bool {
        self.closed.is_cancelled() || self.tx.is_closed()
    }

    fn same_channel(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }

    fn deliver(&self, event: ServerEvent) {
        if !self.is_closed() && self.tx.try_send(event).is_err() {
            // Never skip VT bytes and then continue the same stream.
            self.closed.cancel();
        }
    }
}

struct QueuedRequest {
    request: ClientRequest,
    enqueued: tokio::time::Instant,
}

pub struct Relay {
    pub host: String,
    /// Frames bound for the host daemon; the run loop writes them.
    out: mpsc::Sender<QueuedRequest>,
    /// Serializes request admission with connection teardown and queue draining.
    admission: Mutex<()>,
    pub mirror: Mutex<Mirror>,
    /// Clients watching a mirrored session's PTY, by session.
    attached: Mutex<HashMap<SessionRef, Vec<(RelayClient, (u16, u16))>>>,
    /// Relay req_id → (asking client, its own req_id, admission time).
    pending: Mutex<HashMap<u64, (RelayClient, u64, tokio::time::Instant)>>,
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
    /// The run loop, so a host with no anchors left can be dropped.
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

impl Relay {
    pub fn spawn(daemon: Arc<Daemon>, host: String) -> Arc<Relay> {
        let (out, out_rx) = mpsc::channel(256);
        let relay = Arc::new(Relay {
            host,
            out,
            admission: Mutex::new(()),
            mirror: Mutex::new(Mirror::default()),
            attached: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            connected: AtomicBool::new(false),
            pending_adds: Mutex::new(HashMap::new()),
            adopted: Mutex::new(HashSet::new()),
            host_tree: Mutex::new(Mirror::default()),
            task: Mutex::new(None),
        });
        let task = tokio::spawn(run(relay.clone(), daemon, out_rx));
        *relay.task.lock().unwrap() = Some(task);
        relay
    }

    /// The anchors for this host changed: mirror what they now cover, ask
    /// the host to adopt any it doesn't have, and take back rows that no
    /// anchor covers any more.
    pub fn refresh_anchors(&self, daemon: &Daemon) {
        rescope(self, daemon, false);
        let _admission = self.admission.lock().unwrap();
        if !self.connected.load(Ordering::Relaxed) { return; }
        for req in missing_adds(self, daemon) {
            if let Err(error) = self.out.try_send(QueuedRequest { request: req, enqueued: tokio::time::Instant::now() }) {
                if let Some(id) = request_req_id(&error.into_inner().request) {
                    self.pending_adds.lock().unwrap().remove(&id);
                }
            }
        }
    }

    /// Tear the link down (its ssh dies with the task) and take every
    /// mirrored row off the subscribers' screens. For a host with no
    /// anchors left — it also stops us from keeping a daemon alive there.
    pub fn stop(&self, daemon: &Daemon) {
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        self.connected.store(false, Ordering::Relaxed);
        let old = std::mem::take(&mut *self.mirror.lock().unwrap());
        for id in old.all_entity_ids() {
            daemon.broadcast(ServerEvent::EntityRemoved { id });
        }
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
    pub async fn forward(&self, req: ClientRequest, client: &RelayClient) {
        let _admission = self.admission.lock().unwrap();
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
                    if self.pending.lock().unwrap().len() >= 256 {
                        client.deliver(ServerEvent::Error {
                            req_id: Some(local),
                            message: format!(
                                "{}: too many pending requests; request was not sent",
                                self.host
                            ),
                        });
                        return;
                    }
                    let remote = self.next_req.fetch_add(1, Ordering::Relaxed);
                    self.pending
                        .lock()
                        .unwrap()
                        .insert(remote, (client.clone(), local, tokio::time::Instant::now()));
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
            if matches!(req, ClientRequest::Attach { .. }) {
                client.deliver(ServerEvent::Error {
                    req_id: None,
                    message: nebula_core::relay_status::RelayStatus {
                        host: self.host.clone(),
                        connected: false,
                    }
                    .message(),
                });
            }
            if let Some((client, local, _)) = waiting {
                client.deliver(ServerEvent::Error {
                    req_id: Some(local),
                    message: format!("{} is unreachable right now", self.host),
                });
            }
            return;
        }
        if let Err(error) = self.out.try_send(QueuedRequest {
            request: req,
            enqueued: tokio::time::Instant::now(),
        }) {
            let req = error.into_inner().request;
            let req_id = request_req_id(&req).and_then(|remote| {
                self.pending
                    .lock()
                    .unwrap()
                    .remove(&remote)
                    .map(|(_, local, _)| local)
            });
            client.deliver(ServerEvent::Error {
                req_id,
                message: format!("{}: relay queue full; request was not sent", self.host),
            });
        }
    }

    fn notify_status(&self, connected: bool) {
        let event = ServerEvent::Error {
            req_id: None,
            message: nebula_core::relay_status::RelayStatus {
                host: self.host.clone(),
                connected,
            }
            .message(),
        };
        let mut clients: Vec<RelayClient> = Vec::new();
        for (client, _) in self.attached.lock().unwrap().values().flatten() {
            if !clients.iter().any(|known| known.same_channel(client)) {
                clients.push(client.clone());
            }
        }
        for client in clients {
            client.deliver(event.clone());
        }
    }
    /// Deliver a PTY-plane frame to the clients watching that session.
    async fn fan_out(&self, session: &SessionRef, ev: ServerEvent) {
        let watchers: Vec<RelayClient> = {
            let mut attached = self.attached.lock().unwrap();
            let Some(list) = attached.get_mut(session) else {
                return;
            };
            list.retain(|(tx, _)| !tx.is_closed());
            list.iter().map(|(tx, _)| tx.clone()).collect()
        };
        for tx in watchers {
            tx.deliver(ev.clone());
        }
    }

    /// Route a reply to the client whose request it answers, under that
    /// client's own req_id.
    async fn answer(&self, remote_req: u64, ev: ServerEvent) {
        let Some((client, local, _)) = self.pending.lock().unwrap().remove(&remote_req) else {
            return;
        };
        let ev = match ev {
            ServerEvent::Ack { created, .. } => ServerEvent::Ack {
                req_id: local,
                created,
            },
            ServerEvent::AgentRow { agent, .. } => ServerEvent::AgentRow {
                req_id: local,
                agent,
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
        client.deliver(ev);
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

async fn run(relay: Arc<Relay>, daemon: Arc<Daemon>, mut out_rx: mpsc::Receiver<QueuedRequest>) {
    let mut backoff = RECONNECT_MIN;
    loop {
        if daemon.shutdown.is_cancelled() {
            return;
        }
        let connected_at = tokio::time::Instant::now();
        match connect_and_pump(&relay, &daemon, &mut out_rx).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(host = %relay.host, error = %e, "relay link down");
            }
        }
        relay.connected.store(false, Ordering::Relaxed);
        if connected_at.elapsed() >= RECONNECT_MAX {
            backoff = RECONNECT_MIN;
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
    out_rx: &mut mpsc::Receiver<QueuedRequest>,
) -> Result<()> {
    let mut child = relay_command(&relay.host)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("spawn relay transport")?;
    let writer = BufWriter::new(child.stdin.take().context("relay stdin")?);
    let reader = BufReader::new(child.stdout.take().context("relay stdout")?);
    pump_transport(
        relay,
        daemon,
        out_rx,
        reader,
        writer,
        Duration::from_secs(10),
    )
    .await
}

/// Speak the relay protocol over one transport; all frame state belongs to
/// this connection and is discarded when the transport disconnects.
async fn pump_transport<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    relay: &Arc<Relay>,
    daemon: &Arc<Daemon>,
    out_rx: &mut mpsc::Receiver<QueuedRequest>,
    reader: R,
    writer: W,
    progress_timeout: Duration,
) -> Result<()> {
    let result = pump_connection(relay, daemon, out_rx, reader, writer, progress_timeout).await;
    let _admission = relay.admission.lock().unwrap();
    relay.connected.store(false, Ordering::Relaxed);
    // Admission never awaits: once this lock is held no old request can
    // arrive after the drain. The new handshake reopens admission.
    while out_rx.try_recv().is_ok() {}
    let pending = std::mem::take(&mut *relay.pending.lock().unwrap());
    for (_, (client, local, _)) in pending {
        client.deliver(ServerEvent::Error {
            req_id: Some(local),
            message: format!(
                "{}: connection lost; request outcome unknown; not replayed",
                relay.host
            ),
        });
    }
    relay.notify_status(false);
    // Reconcile adoption against the next full snapshot before asking again.
    relay.pending_adds.lock().unwrap().clear();
    relay.attached.lock().unwrap().retain(|_, clients| {
        clients.retain(|(client, _)| !client.is_closed());
        !clients.is_empty()
    });
    result
}

async fn pump_connection<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    relay: &Arc<Relay>,
    daemon: &Arc<Daemon>,
    out_rx: &mut mpsc::Receiver<QueuedRequest>,
    mut reader: R,
    mut writer: W,
    progress_timeout: Duration,
) -> Result<()> {
    tokio::time::timeout(progress_timeout, async {
    write_frame(
        &mut writer,
        &ClientRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    match read_frame::<ServerEvent, _>(&mut reader).await? {
        Some(ServerEvent::HelloOk { protocol_version, .. }) if protocol_version == PROTOCOL_VERSION => {}
        Some(ServerEvent::Incompatible {
            daemon_protocol_version,
        }) => bail!(
            "{} runs protocol {daemon_protocol_version}, this daemon {PROTOCOL_VERSION} — upgrade nebula on one side",
            relay.host
        ),
        other => bail!("unexpected handshake reply: {other:?}"),
    }
    write_frame(&mut writer, &ClientRequest::Subscribe).await?;
        Ok::<_, anyhow::Error>(())
    }).await.context("relay handshake timed out")??;
    {
        let _admission = relay.admission.lock().unwrap();
        relay.connected.store(true, Ordering::Relaxed);
    }
    tracing::info!(host = %relay.host, "relay link up");

    let mut probe_due = tokio::time::Instant::now() + progress_timeout;
    let mut awaiting_probe = false;
    let snapshot_due = tokio::time::Instant::now() + progress_timeout * 3;
    let mut snapshot_received = false;
    loop {
        // read_frame owns partial header/payload bytes. Keep that future alive
        // across outgoing requests: cancelling and recreating it loses framing.
        let frame = {
            let read = read_frame::<ServerEvent, _>(&mut reader);
            tokio::pin!(read);
            loop {
                tokio::select! {
                    biased;
                    _ = daemon.shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep_until(snapshot_due), if !snapshot_received => {
                        bail!("relay initial snapshot timed out");
                    }
                    _ = tokio::time::sleep_until(probe_due) => {
                        if relay.pending.lock().unwrap().values().any(|(_, _, started)|
                            started.elapsed() >= progress_timeout * 3) {
                            bail!("relay request deadline exceeded; outcome unknown");
                        }
                        if awaiting_probe {
                            bail!("relay application probe timed out");
                        }
                        // Hello is an existing read-only request serviced on the
                        // subscribed connection. Only one probe is outstanding.
                        tokio::time::timeout(progress_timeout, write_frame(&mut writer,
                            &ClientRequest::Hello { protocol_version: PROTOCOL_VERSION }))
                            .await.context("relay probe write timed out")??;
                        awaiting_probe = true;
                        probe_due = tokio::time::Instant::now() + progress_timeout;
                    }
                    event = async {
                        // Fairly choose between frames and requests under load;
                        // the outer select still gives deadlines priority.
                        tokio::select! {
                            frame = &mut read => Ok(frame),
                            request = out_rx.recv() => Err(request),
                        }
                    } => match event {
                        Ok(frame) => break frame?,
                        Err(Some(queued)) => {
                            if queued.enqueued.elapsed() >= progress_timeout {
                                bail!("relay request queue expired; queued actions discarded");
                            }
                            tokio::time::timeout(progress_timeout, write_frame(&mut writer, &queued.request))
                                .await.context("relay write timed out")??;
                        }
                        Err(None) => return Ok(()),
                    }
                }
            }
        };
        let Some(ev) = frame else {
            return Ok(());
        };
        if matches!(ev, ServerEvent::HelloOk { protocol_version, .. } if protocol_version == PROTOCOL_VERSION)
        {
            if awaiting_probe {
                awaiting_probe = false;
                probe_due = tokio::time::Instant::now() + progress_timeout;
            }
            continue;
        }
        if matches!(ev, ServerEvent::Snapshot { .. }) { snapshot_received = true; }
        // Abandon the whole connection on timeout: never resume a partial write.
        tokio::time::timeout(progress_timeout, async {
            if let Some(reqs) = handle_event(relay, daemon, ev).await {
                for r in reqs {
                    write_frame(&mut writer, &r).await?;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("relay event delivery timed out")??;
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
fn compute_scope(
    host: &Mirror,
    wanted: &[PathBuf],
    adopted: &HashSet<String>,
    host_name: &str,
) -> Mirror {
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
        if parent_ids(&Entity::Todo(t.clone()))
            .iter()
            .any(|id| m.ids.contains(id))
        {
            m.upsert(Entity::Todo(t.clone()));
        }
    }
    for n in &host.notes {
        if parent_ids(&Entity::Note(n.clone()))
            .iter()
            .any(|id| m.ids.contains(id))
        {
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
    // An adopted id is a bridge until its anchor takes the host's spelling;
    // once no anchor names that directory at all, the bridge is gone too
    // (a detached anchor must not keep its project in scope).
    let adopted = {
        let mut adopted = relay.adopted.lock().unwrap();
        let tree = relay.host_tree.lock().unwrap();
        adopted.retain(|id| {
            tree.projects.iter().any(|p| {
                p.id.as_str() == id
                    && wanted.iter().any(|w| {
                        w.starts_with(&p.repo_path) || w.file_name() == p.repo_path.file_name()
                    })
            })
        });
        adopted.clone()
    };
    let fresh = {
        let host = relay.host_tree.lock().unwrap();
        compute_scope(&host, &wanted, &adopted, &relay.host)
    };
    apply_scope(relay, daemon, fresh, announce_all);
}

/// AddProject requests for anchors no mirrored project covers yet: the
/// host has never seen that checkout, so it adopts it on our behalf and
/// the Ack's id enters the adopted set. Already-asked paths are skipped.
fn missing_adds(relay: &Relay, daemon: &Daemon) -> Vec<ClientRequest> {
    let wanted = wanted_paths(daemon, &relay.host);
    let known: Vec<PathBuf> = relay
        .mirror
        .lock()
        .unwrap()
        .projects
        .iter()
        .map(|p| p.repo_path.clone())
        .collect();
    let mut out = Vec::new();
    for path in &wanted {
        let covered = known.iter().any(|root| path.starts_with(root));
        let asked = relay
            .pending_adds
            .lock()
            .unwrap()
            .values()
            .any(|p| p == path);
        if covered || asked {
            continue;
        }
        let req_id = relay.next_req.fetch_add(1, Ordering::Relaxed);
        relay
            .pending_adds
            .lock()
            .unwrap()
            .insert(req_id, path.clone());
        out.push(ClientRequest::AddProject {
            req_id,
            path: path.clone(),
            name: None,
            create_missing: false,
            host: None,
        });
    }
    out
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

            relay.notify_status(true);
            let mut follow_ups = missing_adds(relay, daemon);
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
            if let Some(a) = relay
                .host_tree
                .lock()
                .unwrap()
                .agents
                .iter_mut()
                .find(|a| a.id == agent)
            {
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
                    relay
                        .adopted
                        .lock()
                        .unwrap()
                        .insert(id.as_str().to_string());
                    rescope(relay, daemon, false);
                }
                return None;
            }
            relay
                .answer(req_id, ServerEvent::Ack { req_id, created })
                .await;
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
                        .find(|p| {
                            p.repo_path.file_name().map(|n| n.to_os_string()) == name
                                && p.host.is_none()
                        })
                        .map(|p| p.id.as_str().to_string());
                    match candidate {
                        Some(id) => {
                            relay.adopted.lock().unwrap().insert(id);
                            rescope(relay, daemon, false);
                        }
                        None => {
                            tracing::warn!(host = %relay.host, path = %path.display(), "host says the repo is already added but no project matches its name")
                        }
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
        ev @ ServerEvent::AgentRow { .. } => {
            if let ServerEvent::AgentRow { req_id, .. } = &ev {
                let r = *req_id;
                relay.answer(r, ev).await;
            }
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
        out.extend(
            self.projects
                .iter()
                .map(|p| EntityId::Project(p.id.clone())),
        );
        out.extend(
            self.worktrees
                .iter()
                .map(|w| EntityId::Worktree(w.id.clone())),
        );
        out.extend(self.agents.iter().map(|a| EntityId::Agent(a.id.clone())));
        out.extend(
            self.terminals
                .iter()
                .map(|t| EntityId::Terminal(t.id.clone())),
        );
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
    use nebula_core::{AgentId, AgentKind, AgentStatus, WorktreeId};
    use tokio::io::AsyncWriteExt;

    /// A byte transport that acknowledges when a supplied fragment has been
    /// consumed and the relay is waiting for more bytes. No timing sleeps.
    struct ObservedReader {
        inner: tokio::io::ReadHalf<tokio::io::DuplexStream>,
        waiting: mpsc::UnboundedSender<()>,
    }

    impl AsyncRead for ObservedReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let result = std::pin::Pin::new(&mut self.inner).poll_read(cx, buf);
            if result.is_pending() {
                let _ = self.waiting.send(());
            }
            result
        }
    }

    fn test_relay() -> (Arc<Daemon>, Arc<Relay>, mpsc::Receiver<QueuedRequest>) {
        let daemon = Daemon::new(
            Arc::new(crate::store::Store::open_in_memory().unwrap()),
            crate::hooks::HookEnv {
                port: 0,
                token: String::new(),
            },
        );
        let (out, out_rx) = mpsc::channel(256);
        let relay = Arc::new(Relay {
            host: "test-host".into(),
            out,
            admission: Mutex::new(()),
            mirror: Mutex::new(Mirror::default()),
            attached: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            connected: AtomicBool::new(false),
            pending_adds: Mutex::new(HashMap::new()),
            adopted: Mutex::new(HashSet::new()),
            host_tree: Mutex::new(Mirror::default()),
            task: Mutex::new(None),
        });
        (daemon, relay, out_rx)
    }

    #[tokio::test]
    async fn answering_probes_without_initial_snapshot_is_not_ready() {
        let (daemon, relay, mut requests) = test_relay();
        let (transport, mut host) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(transport);
        let scenario = async {
            while let Some(request) = read_frame::<ClientRequest, _>(&mut host).await.unwrap() {
                if matches!(request, ClientRequest::Hello { .. }) {
                    write_frame(&mut host, &ServerEvent::HelloOk { protocol_version: PROTOCOL_VERSION, daemon_pid: 7 }).await.unwrap();
                }
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(pump_transport(&relay, &daemon, &mut requests, read, write, Duration::from_millis(20)), scenario)
        }).await.expect("host never supplied the subscribed snapshot");
        assert!(result.unwrap_err().to_string().contains("snapshot"));
    }

    /// A host whose output is always readable, even between scheduler polls.
    struct BusyHostReader {
        hello: Vec<u8>,
        output: Vec<u8>,
        offset: usize,
        greeted: bool,
    }

    impl AsyncRead for BusyHostReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let bytes = if self.greeted {
                &self.output
            } else {
                &self.hello
            };
            let end = (self.offset + buf.remaining()).min(bytes.len());
            buf.put_slice(&bytes[self.offset..end]);
            let complete = end == bytes.len();
            self.offset = if complete { 0 } else { end };
            if complete {
                self.greeted = true;
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn continuous_host_output_does_not_starve_input() {
        let (daemon, relay, mut requests) = test_relay();
        let session = SessionRef::Agent(AgentId("busy-session".into()));
        let mut hello = Vec::new();
        write_frame(
            &mut hello,
            &ServerEvent::HelloOk {
                protocol_version: PROTOCOL_VERSION,
                daemon_pid: 7,
            },
        )
        .await
        .unwrap();
        let mut output = Vec::new();
        write_frame(
            &mut output,
            &ServerEvent::Output {
                session: session.clone(),
                seq: 0,
                data: b"busy".to_vec(),
            },
        )
        .await
        .unwrap();
        let reader = BusyHostReader {
            hello,
            output,
            offset: 0,
            greeted: false,
        };
        let (writer, mut host) = tokio::io::duplex(4096);
        let host_relay = relay.clone();
        let host_daemon = daemon.clone();
        let scenario = tokio::spawn(async move {
            read_frame::<ClientRequest, _>(&mut host)
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                Some(ClientRequest::Subscribe)
            ));
            let (client, _replies) = mpsc::channel(4);
            let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
            host_relay
                .forward(
                    ClientRequest::Input {
                        session,
                        data: b"typed".to_vec(),
                    },
                    &client,
                )
                .await;
            let request = read_frame::<ClientRequest, _>(&mut host).await.unwrap();
            host_daemon.shutdown.cancel();
            assert!(
                matches!(request, Some(ClientRequest::Input { data, .. }) if data == b"typed"),
                "readable output starved input until the probe deadline"
            );
        });
        let result = pump_transport(
            &relay,
            &daemon,
            &mut requests,
            reader,
            writer,
            Duration::from_millis(100),
        )
        .await;
        scenario.await.unwrap();
        result.unwrap();
    }

    #[tokio::test]
    async fn reconnect_resubscribes_idle_session_and_replays_full_screen() {
        let (daemon, relay, mut requests) = test_relay();
        let (client, mut replies) = mpsc::channel(32);
        let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
        let session = SessionRef::Agent(AgentId("same-live-session".into()));
        relay
            .forward(
                ClientRequest::Attach {
                    session: session.clone(),
                    from_seq: None,
                    cols: 80,
                    rows: 24,
                },
                &client,
            )
            .await;
        for generation in 0..3 {
            let (transport, mut host) = tokio::io::duplex(4096);
            let (read, write) = tokio::io::split(transport);
            let scenario = async {
                assert!(matches!(
                    read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                    Some(ClientRequest::Hello { .. })
                ));
                write_frame(
                    &mut host,
                    &ServerEvent::HelloOk {
                        protocol_version: PROTOCOL_VERSION,
                        daemon_pid: 7,
                    },
                )
                .await
                .unwrap();
                assert!(matches!(
                    read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                    Some(ClientRequest::Subscribe)
                ));
                write_frame(&mut host, &daemon.snapshot().unwrap())
                    .await
                    .unwrap();
                assert!(
                    matches!(read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                    Some(ClientRequest::Attach { session: actual, from_seq: None, .. }) if actual == session)
                );
                let data = format!("screen {generation}").into_bytes();
                write_frame(
                    &mut host,
                    &ServerEvent::Scrollback {
                        session: session.clone(),
                        base_seq: 0,
                        data: data.clone(),
                    },
                )
                .await
                .unwrap();
                loop {
                    let reply = replies.recv().await.unwrap();
                    if let ServerEvent::Scrollback { data: actual, .. } = reply {
                        assert_eq!(actual, data);
                        break;
                    }
                }
                // No terminal output for several probe periods is healthy.
                for _ in 0..4 {
                    assert!(matches!(
                        read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                        Some(ClientRequest::Hello { .. })
                    ));
                    write_frame(
                        &mut host,
                        &ServerEvent::HelloOk {
                            protocol_version: PROTOCOL_VERSION,
                            daemon_pid: 7,
                        },
                    )
                    .await
                    .unwrap();
                }
                host.shutdown().await.unwrap();
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(
                    pump_transport(
                        &relay,
                        &daemon,
                        &mut requests,
                        read,
                        write,
                        Duration::from_millis(20)
                    ),
                    scenario
                )
            })
            .await
            .expect("idle session did not recover");
            result.unwrap();
        }
    }

    #[tokio::test]
    async fn blocked_writer_abandons_the_transport() {
        let (daemon, relay, mut requests) = test_relay();
        let (client, _replies) = mpsc::channel(4);
        let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
        let (transport, mut host) = tokio::io::duplex(256);
        let (read, write) = tokio::io::split(transport);
        let (finished, wait) = tokio::sync::oneshot::channel();
        let scenario = async {
            read_frame::<ClientRequest, _>(&mut host)
                .await
                .unwrap()
                .unwrap();
            write_frame(
                &mut host,
                &ServerEvent::HelloOk {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: 0,
                },
            )
            .await
            .unwrap();
            read_frame::<ClientRequest, _>(&mut host)
                .await
                .unwrap()
                .unwrap();
            relay
                .forward(
                    ClientRequest::Input {
                        session: SessionRef::Agent(AgentId("session".into())),
                        data: vec![b'x'; 8192],
                    },
                    &client,
                )
                .await;
            // Keep SSH alive but stop draining its stdin entirely.
            wait.await.unwrap();
            drop(host);
        };
        let pump = async {
            let result = pump_transport(
                &relay,
                &daemon,
                &mut requests,
                read,
                write,
                Duration::from_millis(20),
            )
            .await;
            let _ = finished.send(());
            result
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(pump, scenario)
        })
        .await
        .expect("blocked writer held the connection forever");
        assert!(result.unwrap_err().to_string().contains("write timed out"));
    }

    #[tokio::test]
    async fn responsive_probes_do_not_hide_a_stuck_request() {
        let (daemon, relay, mut requests) = test_relay();
        let (client, mut replies) = mpsc::channel(16);
        let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
        let (transport, mut host) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(transport);
        let scenario = async {
            while let Some(request) = read_frame::<ClientRequest, _>(&mut host).await.unwrap() {
                match request {
                    ClientRequest::Hello { .. } => write_frame(
                        &mut host,
                        &ServerEvent::HelloOk {
                            protocol_version: PROTOCOL_VERSION,
                            daemon_pid: 0,
                        },
                    )
                    .await
                    .unwrap(),
                    ClientRequest::Subscribe => {
                        write_frame(&mut host, &daemon.snapshot().unwrap()).await.unwrap();
                        relay
                            .forward(
                                ClientRequest::CreateTerminal {
                                    expected_branch: Some("feature".into()),
                                    req_id: 42,
                                    worktree: WorktreeId("worktree".into()),
                                    name: None,
                                },
                                &client,
                            )
                            .await
                    }
                    _ => {} // Accept the mutation but never acknowledge it.
                }
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                pump_transport(
                    &relay,
                    &daemon,
                    &mut requests,
                    read,
                    write,
                    Duration::from_millis(20)
                ),
                scenario
            )
        })
        .await
        .expect("probe success must not leave a mutation pending forever");
        assert!(result.is_err());
        assert!(matches!(
            replies.recv().await,
            Some(ServerEvent::Error {
                req_id: Some(42),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn a_v31_host_is_refused_before_the_relay_subscribes() {
        let (daemon, relay, mut requests) = test_relay();
        let (transport, mut host) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(transport);
        let host_side = async {
            let hello = read_frame::<ClientRequest, _>(&mut host).await.unwrap();
            assert!(
                matches!(
                    hello,
                    Some(ClientRequest::Hello {
                        protocol_version: 32
                    })
                ),
                "{hello:?}"
            );
            write_frame(
                &mut host,
                &ServerEvent::Incompatible {
                    daemon_protocol_version: 31,
                },
            )
            .await
            .unwrap();
            read_frame::<ClientRequest, _>(&mut host).await.unwrap()
        };
        let (result, after) = tokio::join!(
            pump_transport(
                &relay,
                &daemon,
                &mut requests,
                read,
                write,
                Duration::from_secs(1)
            ),
            host_side
        );
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("runs protocol 31, this daemon 32"),
            "{error}"
        );
        assert!(after.is_none(), "the relay went on to send {after:?}");
    }

    #[tokio::test]
    async fn a_forced_start_reaches_the_host_and_its_row_answers_the_asking_client() {
        let (daemon, relay, mut requests) = test_relay();
        let (client, mut replies) = mpsc::channel(16);
        let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
        let (transport, mut host) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(transport);
        let row = Agent {
            id: AgentId("queued".into()),
            worktree_id: WorktreeId("worktree".into()),
            name: "worker".into(),
            status: AgentStatus::Fresh,
            archived: false,
            archived_at: 0,
            pinned: false,
            status_changed_at: 0,
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            session_id: None,
            sort_order: 0,
            alive: true,
            branch: None,
        };
        let scenario = async {
            while let Some(request) = read_frame::<ClientRequest, _>(&mut host).await.unwrap() {
                match request {
                    ClientRequest::Hello { .. } => write_frame(
                        &mut host,
                        &ServerEvent::HelloOk {
                            protocol_version: PROTOCOL_VERSION,
                            daemon_pid: 0,
                        },
                    )
                    .await
                    .unwrap(),
                    ClientRequest::Subscribe => {
                        write_frame(&mut host, &daemon.snapshot().unwrap())
                            .await
                            .unwrap();
                        relay
                            .forward(
                                ClientRequest::ForceStartAgent {
                                    req_id: 42,
                                    id: AgentId("queued".into()),
                                },
                                &client,
                            )
                            .await
                    }
                    ClientRequest::ForceStartAgent { req_id, id } => {
                        assert_eq!(id.as_str(), "queued");
                        write_frame(
                            &mut host,
                            &ServerEvent::AgentRow {
                                req_id,
                                agent: row.clone(),
                            },
                        )
                        .await
                        .unwrap();
                        break;
                    }
                    _ => {}
                }
            }
            match tokio::time::timeout(Duration::from_secs(1), replies.recv()).await {
                Ok(Some(ServerEvent::AgentRow { req_id, agent })) => {
                    assert_eq!((req_id, agent.id.as_str()), (42, "queued"))
                }
                other => panic!("the asking client got {other:?}"),
            }
        };
        tokio::select! {
            _ = pump_transport(&relay, &daemon, &mut requests, read, write, Duration::from_secs(5)) => {
                panic!("the relay stopped before the reply arrived")
            }
            () = scenario => {}
        }
    }

    #[tokio::test]
    async fn connection_loss_fails_uncertain_mutation_and_discards_queued_input() {
        let (daemon, relay, mut requests) = test_relay();
        let (client, mut replies) = mpsc::channel(16);
        let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
        for _ in 0..3 {
            let (transport, mut host) = tokio::io::duplex(4096);
            let (read, write) = tokio::io::split(transport);
            let scenario = async {
                assert!(matches!(
                    read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                    Some(ClientRequest::Hello { .. })
                ));
                write_frame(
                    &mut host,
                    &ServerEvent::HelloOk {
                        protocol_version: PROTOCOL_VERSION,
                        daemon_pid: 0,
                    },
                )
                .await
                .unwrap();
                assert!(matches!(
                    read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                    Some(ClientRequest::Subscribe)
                ));
                assert!(
                    matches!(
                        read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                        Some(ClientRequest::Hello { .. })
                    ),
                    "a previous connection's action was replayed"
                );
                write_frame(
                    &mut host,
                    &ServerEvent::HelloOk {
                        protocol_version: PROTOCOL_VERSION,
                        daemon_pid: 0,
                    },
                )
                .await
                .unwrap();
                // Give the relay a queued mutation and input, then EOF before it
                // gets to send either. Neither may survive into the next pipe.
                relay
                    .forward(
                        ClientRequest::CreateTerminal {
                            expected_branch: Some("feature".into()),
                            req_id: 42,
                            worktree: WorktreeId("worktree".into()),
                            name: None,
                        },
                        &client,
                    )
                    .await;
                relay
                    .forward(
                        ClientRequest::Input {
                            session: SessionRef::Agent(AgentId("session".into())),
                            data: b"do not replay".to_vec(),
                        },
                        &client,
                    )
                    .await;
                drop(host);
            };
            let _ = tokio::join!(
                pump_transport(
                    &relay,
                    &daemon,
                    &mut requests,
                    read,
                    write,
                    Duration::from_millis(20)
                ),
                scenario
            );
            let reply = tokio::time::timeout(Duration::from_millis(100), replies.recv())
                .await
                .expect("uncertain mutation was left pending");
            assert!(matches!(
                reply,
                Some(ServerEvent::Error {
                    req_id: Some(42),
                    ..
                })
            ));
        }
    }

    #[tokio::test]
    async fn full_request_queue_rejects_mutations_without_waiting() {
        let (_, relay, _requests) = test_relay();
        relay.connected.store(true, Ordering::Relaxed);
        let (client, mut replies) = mpsc::channel(4);
        let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
        for _ in 0..256 {
            relay
                .forward(
                    ClientRequest::Input {
                        session: SessionRef::Agent(AgentId("session".into())),
                        data: b"x".to_vec(),
                    },
                    &client,
                )
                .await;
        }
        tokio::time::timeout(
            Duration::from_millis(50),
            relay.forward(
                ClientRequest::CreateTerminal {
                    expected_branch: Some("feature".into()),
                    req_id: 42,
                    worktree: WorktreeId("worktree".into()),
                    name: None,
                },
                &client,
            ),
        )
        .await
        .expect("full transport queue blocked a caller");
        assert!(matches!(
            replies.recv().await,
            Some(ServerEvent::Error {
                req_id: Some(42),
                ..
            })
        ));
    }

    #[tokio::test]
    async fn slow_subscriber_does_not_block_healthy_session_output() {
        let (daemon, relay, mut requests) = test_relay();
        let (slow, _slow_rx) = mpsc::channel(1);
        let (healthy, mut healthy_rx) = mpsc::channel(4);
        let slow_closed = tokio_util::sync::CancellationToken::new();
        let slow = RelayClient::new(slow, slow_closed.clone());
        let healthy = RelayClient::new(healthy, tokio_util::sync::CancellationToken::new());
        let session = SessionRef::Agent(AgentId("session".into()));
        let (transport, mut host) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(transport);
        let scenario = async {
            read_frame::<ClientRequest, _>(&mut host)
                .await
                .unwrap()
                .unwrap();
            write_frame(
                &mut host,
                &ServerEvent::HelloOk {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: 0,
                },
            )
            .await
            .unwrap();
            read_frame::<ClientRequest, _>(&mut host)
                .await
                .unwrap()
                .unwrap();
            for client in [&slow, &healthy] {
                relay
                    .forward(
                        ClientRequest::Attach {
                            session: session.clone(),
                            from_seq: None,
                            cols: 80,
                            rows: 24,
                        },
                        client,
                    )
                    .await;
                assert!(matches!(
                    read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                    Some(ClientRequest::Attach { .. })
                ));
            }
            for seq in 0..2 {
                write_frame(
                    &mut host,
                    &ServerEvent::Output {
                        session: session.clone(),
                        seq,
                        data: b"x".to_vec(),
                    },
                )
                .await
                .unwrap();
                let reply = tokio::time::timeout(Duration::from_millis(100), healthy_rx.recv())
                    .await
                    .expect("one full subscriber blocked the host");
                assert!(
                    matches!(reply, Some(ServerEvent::Output { seq: actual, .. }) if actual == seq)
                );
            }
            // This signal is the server connection's socket-close boundary.
            tokio::time::timeout(Duration::from_millis(100), slow_closed.cancelled())
                .await
                .expect("lagging socket was not told to disconnect");
            drop(host);
        };
        let (result, ()) = tokio::join!(
            pump_transport(
                &relay,
                &daemon,
                &mut requests,
                read,
                write,
                Duration::from_secs(1)
            ),
            scenario
        );
        result.unwrap();
    }

    #[tokio::test]
    async fn live_transport_without_application_replies_is_rejected() {
        let (daemon, relay, mut requests) = test_relay();
        let (transport, mut host) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(transport);
        let scenario = async {
            read_frame::<ClientRequest, _>(&mut host)
                .await
                .unwrap()
                .unwrap();
            write_frame(
                &mut host,
                &ServerEvent::HelloOk {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: 0,
                },
            )
            .await
            .unwrap();
            // SSH keeps accepting bytes, but its application never answers again.
            while read_frame::<ClientRequest, _>(&mut host)
                .await
                .unwrap()
                .is_some()
            {}
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                pump_transport(
                    &relay,
                    &daemon,
                    &mut requests,
                    read,
                    write,
                    Duration::from_millis(20)
                ),
                scenario
            )
        })
        .await
        .expect("alive transport hid an unresponsive application");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn silent_transport_handshake_is_bounded() {
        let (daemon, relay, mut requests) = test_relay();
        let (transport, _silent_host) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(transport);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            pump_transport(
                &relay,
                &daemon,
                &mut requests,
                read,
                write,
                Duration::from_millis(20),
            ),
        )
        .await
        .expect("silent SSH transport must not hang the relay handshake");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn session_reads_survive_bidirectional_fragmented_traffic() {
        let (daemon, relay, mut out_rx) = test_relay();
        let (transport, mut host) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(transport);
        let (waiting, mut waits) = mpsc::unbounded_channel();
        let reader = ObservedReader {
            inner: reader,
            waiting,
        };
        let (client, mut replies) = mpsc::channel(16);
        let client = RelayClient::new(client, tokio_util::sync::CancellationToken::new());
        let session = SessionRef::Agent(AgentId("test-session".into()));
        let pump = pump_transport(
            &relay,
            &daemon,
            &mut out_rx,
            BufReader::new(reader),
            BufWriter::new(writer),
            Duration::from_secs(10),
        );
        let scenario = async {
            assert!(matches!(
                read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                Some(ClientRequest::Hello { .. })
            ));
            write_frame(
                &mut host,
                &ServerEvent::HelloOk {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: 0,
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                Some(ClientRequest::Subscribe)
            ));

            // First split makes a lost payload prefix look like a valid 64 KiB
            // frame length: the broken relay stays connected but never replies.
            // Then exercise every header boundary and another payload boundary.
            for (index, split) in [None, Some(1), Some(2), Some(3), Some(4), Some(5)]
                .into_iter()
                .enumerate()
            {
                let local_id = 100 + index as u64;
                relay
                    .forward(
                        ClientRequest::ReadSession {
                            req_id: local_id,
                            session: session.clone(),
                            lines: Some(2),
                        },
                        &client,
                    )
                    .await;
                let Some(ClientRequest::ReadSession { req_id, .. }) =
                    read_frame::<ClientRequest, _>(&mut host).await.unwrap()
                else {
                    panic!("host did not receive session read");
                };
                let text = "\0\0\u{1}\0pane";
                let mut frame = Vec::new();
                write_frame(
                    &mut frame,
                    &ServerEvent::SessionText {
                        req_id,
                        cols: 80,
                        rows: 24,
                        text: text.into(),
                    },
                )
                .await
                .unwrap();
                let split = split.unwrap_or(frame.len() - 8);
                while waits.try_recv().is_ok() {}
                host.write_all(&frame[..split]).await.unwrap();
                waits.recv().await.unwrap();
                // The reader is now suspended inside this frame. Three outgoing
                // requests must progress without losing its header or payload.
                for _ in 0..3 {
                    relay
                        .forward(
                            ClientRequest::Input {
                                session: session.clone(),
                                data: b"x".to_vec(),
                            },
                            &client,
                        )
                        .await;
                    assert!(
                        matches!(read_frame::<ClientRequest, _>(&mut host).await.unwrap(),
                        Some(ClientRequest::Input { data, .. }) if data == b"x")
                    );
                }
                host.write_all(&frame[split..]).await.unwrap();
                let reply = tokio::time::timeout(Duration::from_secs(1), replies.recv())
                    .await
                    .expect("session read timed out on a healthy relay after interleaved input")
                    .expect("client disconnected");
                assert!(
                    matches!(reply, ServerEvent::SessionText { req_id, text: actual, .. }
                    if req_id == local_id && actual == text)
                );
            }
            host.shutdown().await.unwrap();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(pump, scenario)
        })
        .await
        .expect("relay stopped making bidirectional progress");
        result.unwrap();
    }
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
                expected_branch: None,
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
        assert_eq!(
            request_ids(&ClientRequest::ForceStartAgent {
                req_id: 1,
                id: AgentId("a1".into()),
            }),
            vec!["a1"]
        );
        // Subscribe names nothing: never routed.
        assert!(request_ids(&ClientRequest::Subscribe).is_empty());
    }

    #[test]
    fn req_id_round_trips_through_rewrite() {
        let req = ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 7,
            worktree: WorktreeId("w1".into()),
            name: Some("t".into()),
        };
        assert_eq!(request_req_id(&req), Some(7));
        let re = with_req_id(&req, 99).unwrap();
        assert_eq!(request_req_id(&re), Some(99));
        assert!(
            matches!(re, ClientRequest::CreateTerminal { worktree, .. } if worktree.as_str() == "w1")
        );
        assert_eq!(request_req_id(&ClientRequest::Subscribe), None);
    }
}
