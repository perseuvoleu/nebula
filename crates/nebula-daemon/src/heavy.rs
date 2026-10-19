//! Memory leases for heavy commands and agent sessions. One task owns the
//! dispatcher ledger. Each client connection holds at most one command
//! `Lease`, and each session process holds the `Lease` of its own launch;
//! dropping one (the connection ended, the process exited) frees the
//! reservation and lets the queue head in. A queued session's grant goes
//! out as `SessionNews` to the daemon's session starter, never spawned
//! here. A daemon starts by counting every wrapper that marked itself for
//! it (`HeavyMark`), since an earlier daemon may have granted it a lease.

use crate::config::{Config, HeavyConfig};
use crate::dispatcher::{Consumer, Dispatcher, LeaseId, Limits, Standing, UNBOUNDED_BYTES};
use anyhow::{anyhow, bail, Result};
use nebula_core::heavy::{daemon_id, processes, HeavyMark};
use nebula_core::mem::{machine_memory, MachineMemory, ProbeError};
use nebula_core::{
    paths, AgentId, Blocked, HeavyLease, HeavySnapshot, RunningLease, ServerEvent, SessionRef,
    Waiting,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const TICK: Duration = Duration::from_secs(2);

pub struct Desk {
    tx: mpsc::UnboundedSender<Msg>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<Msg>>>,
    ids: Arc<AtomicU64>,
}

pub struct Lease {
    consumer: Consumer,
    tx: mpsc::UnboundedSender<Msg>,
}

/// What the ledger tells the session starter.
pub enum SessionNews {
    /// A queued launch's turn: its process may start and holds `charge`.
    Granted {
        agent: AgentId,
        launch: LeaseId,
        charge: Lease,
    },
    /// Its caller stopped waiting before `admission` reached it; the starter
    /// acts on it instead, and owns any charge it carries.
    Unheard {
        agent: AgentId,
        launch: LeaseId,
        admission: Admission,
    },
    /// The launch can never fit the budget and left the queue.
    Refused {
        agent: AgentId,
        launch: LeaseId,
        reserved_bytes: u64,
        budget_bytes: u64,
    },
}

pub enum Admission {
    Admitted(Lease),
    /// Waits in the queue; its grant comes as `SessionNews::Granted`.
    Queued(LeaseId),
    TooLarge {
        reserved_bytes: u64,
        budget_bytes: u64,
    },
}

/// One client connection, as the ledger sees it.
#[derive(Clone)]
pub struct Link {
    pub events: mpsc::Sender<ServerEvent>,
    /// Cancelling it ends the connection, and with it the lease.
    pub closed: CancellationToken,
    /// From the socket's peer credentials.
    pub peer_pid: Option<u32>,
}

impl Link {
    fn answer_or_close(&self, event: ServerEvent) -> bool {
        let sent = self.events.try_send(event).is_ok();
        if !sent {
            self.closed.cancel();
        }
        sent
    }
}

enum Msg {
    Change(Change),
    Status(oneshot::Sender<LedgerView>),
}

struct LedgerView {
    running: Vec<HeavyLease>,
    queued: Vec<Waiting>,
}

enum Change {
    Request {
        id: LeaseId,
        reserve_bytes: Option<u64>,
        command: String,
        link: Link,
    },
    Running {
        id: LeaseId,
        reserved_bytes: u64,
        command: String,
        link: Link,
    },
    Session {
        id: LeaseId,
        agent: AgentId,
        reserved_bytes: u64,
        answer: Answer,
    },
    ForceSession {
        id: LeaseId,
        agent: AgentId,
        reserved_bytes: u64,
    },
    CancelSession(AgentId),
    SessionPid {
        id: LeaseId,
        pid: u32,
    },
    Release(Consumer),
}

impl Default for Desk {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Mutex::new(Some(rx)),
            ids: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl Desk {
    /// Start the ledger task; later calls do nothing.
    pub fn spawn(&self, shutdown: CancellationToken, news: mpsc::UnboundedSender<SessionNews>) {
        if let Some(rx) = self.rx.lock().unwrap().take() {
            tokio::spawn(run(rx, self.tx.clone(), self.ids.clone(), news, shutdown));
        }
    }

    /// Queue a command; the link gets its `HeavyQueued`/`HeavyGranted`.
    pub fn request(&self, reserve_bytes: Option<u64>, command: String, link: Link) -> Lease {
        let id = self.next_id();
        let lease = self.lease(Consumer::Command(id));
        let _ = self.tx.send(Msg::Change(Change::Request {
            id,
            reserve_bytes,
            command,
            link,
        }));
        lease
    }

    /// Re-attach a wrapper this daemon counted at startup.
    pub fn running(&self, reserved_bytes: u64, command: String, link: Link) -> Lease {
        let id = self.next_id();
        let lease = self.lease(Consumer::Command(id));
        let _ = self.tx.send(Msg::Change(Change::Running {
            id,
            reserved_bytes,
            command,
            link,
        }));
        lease
    }

    /// Ask for room to launch `agent`, behind everything already queued.
    /// From here on the answer reaches this caller or, if it stops waiting,
    /// the session starter.
    pub fn request_session(&self, agent: AgentId, reserved_bytes: u64) -> Result<PendingAdmission> {
        if self.rx.lock().unwrap().is_some() {
            bail!("the memory dispatcher is not running");
        }
        let (reply, offer) = oneshot::channel();
        self.tx
            .send(Msg::Change(Change::Session {
                id: self.next_id(),
                agent,
                reserved_bytes,
                answer: Answer::Caller(reply),
            }))
            .map_err(|_| anyhow!("the memory dispatcher has stopped"))?;
        Ok(PendingAdmission(offer))
    }

    /// Queue a launch whose answer comes only as `SessionNews`.
    pub fn queue_session(&self, agent: AgentId, reserved_bytes: u64) -> LeaseId {
        let id = self.next_id();
        let _ = self.tx.send(Msg::Change(Change::Session {
            id,
            agent,
            reserved_bytes,
            answer: Answer::News,
        }));
        id
    }

    /// Count a launch now, without asking; it leaves the queue if waiting.
    pub fn force_session(&self, agent: AgentId, reserved_bytes: u64) -> Lease {
        let id = self.next_id();
        let lease = self.lease(Consumer::SessionLaunch(id));
        let _ = self.tx.send(Msg::Change(Change::ForceSession {
            id,
            agent,
            reserved_bytes,
        }));
        lease
    }

    /// Take `launch` out of the queue, or free its charge if it was granted.
    pub fn give_back(&self, launch: LeaseId) {
        drop(self.lease(Consumer::SessionLaunch(launch)));
    }

    /// Drop the agent's queued launches; a running one keeps its charge.
    pub fn cancel_session(&self, agent: AgentId) {
        let _ = self.tx.send(Msg::Change(Change::CancelSession(agent)));
    }

    /// The process holding `launch`'s charge started; its tree's RSS counts from now.
    pub fn session_spawned(&self, launch: Consumer, pid: u32) {
        if let Consumer::SessionLaunch(id) = launch {
            let _ = self.tx.send(Msg::Change(Change::SessionPid { id, pid }));
        }
    }

    /// What `heavy status` shows; None once the ledger task has stopped.
    pub async fn status(&self, sessions: Vec<(SessionRef, u32)>) -> Option<HeavySnapshot> {
        let ledger = self.ledger().await?;
        let roots: Vec<u32> = ledger.running.iter().filter_map(|l| l.pid).collect();
        let (memory, trees) = tokio::task::spawn_blocking(move || {
            let trees = crate::metrics::sessions_and_trees(&sessions, &roots);
            (
                machine_memory(),
                trees.map(|(s, rss)| (s, roots.into_iter().zip(rss))),
            )
        })
        .await
        .ok()?;
        let (sessions, tree_rss): (_, HashMap<u32, Option<u64>>) = match trees {
            Ok((sessions, rss)) => (Ok(sessions), rss.collect()),
            Err(e) => (Err(e.to_string()), HashMap::new()),
        };
        let limits = Config::load().heavy.limits();
        Some(HeavySnapshot {
            memory: memory.map_err(|e| e.to_string()),
            budget_bytes: limits.budget_bytes,
            floor_bytes: limits.floor_bytes,
            sessions,
            running: ledger
                .running
                .into_iter()
                .map(|lease| RunningLease {
                    rss_bytes: lease
                        .pid
                        .and_then(|pid| tree_rss.get(&pid).copied().flatten()),
                    lease,
                })
                .collect(),
            queued: ledger.queued,
        })
    }

    async fn ledger(&self) -> Option<LedgerView> {
        let (reply, view) = oneshot::channel();
        self.tx.send(Msg::Status(reply)).ok()?;
        view.await.ok()
    }

    fn next_id(&self) -> LeaseId {
        LeaseId(self.ids.fetch_add(1, Ordering::Relaxed))
    }

    fn lease(&self, consumer: Consumer) -> Lease {
        Lease {
            consumer,
            tx: self.tx.clone(),
        }
    }
}

#[cfg(test)]
impl Desk {
    pub(crate) fn charge_for(&self, launch: LeaseId) -> Lease {
        self.lease(Consumer::SessionLaunch(launch))
    }

    pub(crate) fn released(&self) -> Vec<Consumer> {
        let mut rx = self.rx.lock().unwrap();
        let rx = rx.as_mut().expect("the ledger task is not running");
        let mut released = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let Msg::Change(Change::Release(consumer)) = msg {
                released.push(consumer);
            }
        }
        released
    }
}

#[cfg(test)]
pub(crate) struct Inbox {
    rx: mpsc::UnboundedReceiver<Msg>,
    ledger: mpsc::UnboundedSender<Msg>,
    news: mpsc::UnboundedSender<SessionNews>,
    unheard: mpsc::UnboundedReceiver<SessionNews>,
}

#[cfg(test)]
pub(crate) struct Reply {
    reply: oneshot::Sender<AdmissionOffer>,
    agent: AgentId,
    launch: LeaseId,
    ledger: mpsc::UnboundedSender<Msg>,
    news: mpsc::UnboundedSender<SessionNews>,
}

#[cfg(test)]
impl Reply {
    /// Delivers the answer as the ledger would; Err when nobody received it.
    pub(crate) fn send(self, admission: Admission) -> Result<(), ()> {
        self.reply
            .send(AdmissionOffer {
                admission: Some(admission),
                agent: self.agent,
                launch: self.launch,
                news: self.news,
                ledger: self.ledger,
            })
            .map_err(drop)
    }
}

#[cfg(test)]
impl Desk {
    /// The ledger's messages, for a test that answers admissions itself.
    pub(crate) fn take_inbox(&self) -> Inbox {
        let (news, unheard) = mpsc::unbounded_channel();
        Inbox {
            rx: self
                .rx
                .lock()
                .unwrap()
                .take()
                .expect("the ledger task is not running"),
            ledger: self.tx.clone(),
            news,
            unheard,
        }
    }
}

#[cfg(test)]
impl Inbox {
    pub(crate) async fn next_admission(&mut self) -> (LeaseId, Reply) {
        loop {
            if let Msg::Change(Change::Session {
                id,
                agent,
                answer: Answer::Caller(reply),
                ..
            }) = self.rx.recv().await.expect("the desk is gone")
            {
                let reply = Reply {
                    reply,
                    agent,
                    launch: id,
                    ledger: self.ledger.clone(),
                    news: self.news.clone(),
                };
                return (id, reply);
            }
        }
    }

    /// What an unclaimed answer handed to the session starter.
    pub(crate) fn unheard(&mut self) -> SessionNews {
        self.unheard.try_recv().expect("no answer was handed over")
    }

    pub(crate) fn nothing_more_unheard(&mut self) -> bool {
        self.unheard.try_recv().is_err()
    }

    pub(crate) fn asked_for_nothing(&mut self) -> bool {
        self.rx.try_recv().is_err()
    }

    pub(crate) fn released(&mut self) -> Vec<Consumer> {
        let mut released = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            if let Msg::Change(Change::Release(consumer)) = msg {
                released.push(consumer);
            }
        }
        released
    }
}

pub struct PendingAdmission(oneshot::Receiver<AdmissionOffer>);

impl PendingAdmission {
    pub async fn answer(self) -> Result<Admission> {
        let offer = self
            .0
            .await
            .map_err(|_| anyhow!("the memory dispatcher has stopped"))?;
        Ok(offer.claim())
    }
}

impl Lease {
    pub fn consumer(&self) -> &Consumer {
        &self.consumer
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self
            .tx
            .send(Msg::Change(Change::Release(self.consumer.clone())));
    }
}

struct Entry {
    command: String,
    reserved_bytes: u64,
    wrapper_pid: Option<u32>,
    holder: Holder,
    reported: Option<(u32, Blocked)>,
}

enum Holder {
    Wrapper(Link),
    /// Counted at startup, with no wrapper connected to answer for it.
    Unclaimed {
        mark: HeavyMark,
    },
}

impl Holder {
    fn link(&self) -> Option<&Link> {
        match self {
            Holder::Wrapper(link) => Some(link),
            Holder::Unclaimed { .. } => None,
        }
    }
}

impl Entry {
    fn rejoin_charge(&self, claimed_bytes: u64, current_default_bytes: u64) -> Option<u64> {
        let Holder::Unclaimed { mark } = self.holder else {
            return None;
        };
        match mark.reserve_bytes {
            Some(marked_bytes) => (marked_bytes == claimed_bytes).then_some(marked_bytes),
            None => Some(claimed_bytes.max(current_default_bytes)),
        }
    }
}

enum Answer {
    Caller(oneshot::Sender<AdmissionOffer>),
    News,
}

/// An answer on its way to the caller that asked. The caller claims it the
/// moment it reads it; dropped unclaimed (the send failed, or the caller was
/// cancelled before reading) it hands the answer to the session starter
/// instead, with any charge intact.
pub struct AdmissionOffer {
    admission: Option<Admission>,
    agent: AgentId,
    launch: LeaseId,
    news: mpsc::UnboundedSender<SessionNews>,
    ledger: mpsc::UnboundedSender<Msg>,
}

impl AdmissionOffer {
    pub fn claim(mut self) -> Admission {
        self.admission.take().expect("an offer is claimed once")
    }
}

impl Drop for AdmissionOffer {
    fn drop(&mut self) {
        let Some(admission) = self.admission.take() else {
            return;
        };
        tracing::info!(launch = self.launch.0, agent = %self.agent, "session start gave up waiting for its answer");
        let unheard = self.news.send(SessionNews::Unheard {
            agent: self.agent.clone(),
            launch: self.launch,
            admission,
        });
        // With no starter left, a queued launch nobody owns would block the
        // queue; an admitted charge frees itself as it drops here.
        if let Err(mpsc::error::SendError(SessionNews::Unheard {
            admission: Admission::Queued(_),
            ..
        })) = unheard
        {
            let _ = self
                .ledger
                .send(Msg::Change(Change::Release(Consumer::SessionLaunch(
                    self.launch,
                ))));
        }
    }
}

struct SessionEntry {
    agent: AgentId,
    reserved_bytes: u64,
    pid: Option<u32>,
    answer: Answer,
}

struct Undelivered;

struct Ledger {
    dispatcher: Dispatcher,
    leases: HashMap<LeaseId, Entry>,
    sessions: HashMap<LeaseId, SessionEntry>,
    tx: mpsc::UnboundedSender<Msg>,
    news: mpsc::UnboundedSender<SessionNews>,
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Msg>,
    tx: mpsc::UnboundedSender<Msg>,
    ids: Arc<AtomicU64>,
    news: mpsc::UnboundedSender<SessionNews>,
    shutdown: CancellationToken,
) {
    let heavy = Config::load().heavy;
    let mut ledger = Ledger::new(heavy.limits(), tx, news);
    ledger.recover(&our_wrappers().await, || {
        LeaseId(ids.fetch_add(1, Ordering::Relaxed))
    });
    let mut tick = tokio::time::interval(TICK);
    loop {
        let may_admit = tokio::select! {
            _ = shutdown.cancelled() => break,
            msg = rx.recv() => match msg {
                Some(Msg::Change(change)) => {
                    ledger.apply(change, &Config::load().heavy);
                    true
                }
                Some(Msg::Status(reply)) => {
                    let _ = reply.send(ledger.view());
                    false
                }
                None => break,
            },
            _ = tick.tick() => {
                let roots = ledger.roots();
                let session_roots = ledger.session_roots();
                if !roots.is_empty() || !session_roots.is_empty() {
                    let pids: Vec<u32> = roots.iter().map(|(_, pid)| *pid).collect();
                    let session_pids: Vec<u32> = session_roots.iter().map(|(_, pid)| *pid).collect();
                    let rss = tokio::task::spawn_blocking(move || {
                        crate::metrics::ledger_rss(&pids, &session_pids)
                    })
                    .await
                    .unwrap_or_else(|e| Err(ProbeError::Unreadable(e.to_string())));
                    let (rss, session_rss) = match rss {
                        Ok((commands, sessions)) => (Ok(commands), Some(sessions)),
                        Err(e) => (Err(e), None),
                    };
                    ledger.observe(roots, rss);
                    ledger.observe_sessions(session_roots, session_rss);
                }
                true
            }
        };
        if may_admit && ledger.dispatcher.has_queued() {
            let machine = tokio::task::spawn_blocking(machine_memory)
                .await
                .unwrap_or_else(|e| Err(ProbeError::Unreadable(e.to_string())));
            ledger.admit(Config::load().heavy.limits(), &machine);
        }
        ledger.answer_queued_sessions();
    }
}

/// A failed send drops the offer, which hands the answer on.
fn offer(
    news: &mpsc::UnboundedSender<SessionNews>,
    ledger: &mpsc::UnboundedSender<Msg>,
    reply: oneshot::Sender<AdmissionOffer>,
    agent: &AgentId,
    launch: LeaseId,
    admission: Admission,
) {
    let _ = reply.send(AdmissionOffer {
        admission: Some(admission),
        agent: agent.clone(),
        launch,
        news: news.clone(),
        ledger: ledger.clone(),
    });
}

async fn our_wrappers() -> Vec<(u32, HeavyMark)> {
    let ours = daemon_id(&paths::socket_path());
    tokio::task::spawn_blocking(move || {
        processes()
            .into_iter()
            .filter(|p| p.is_wrapper())
            .filter_map(|p| Some((p.pid, p.mark?)))
            .filter(|(_, mark)| mark.daemon == ours)
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn confirmed_dead(pid: u32) -> bool {
    let pid = nix::unistd::Pid::from_raw(pid as i32);
    nix::sys::signal::kill(pid, None) == Err(nix::errno::Errno::ESRCH)
}

impl Ledger {
    fn new(
        limits: Limits,
        tx: mpsc::UnboundedSender<Msg>,
        news: mpsc::UnboundedSender<SessionNews>,
    ) -> Self {
        Self {
            dispatcher: Dispatcher::new(limits),
            leases: HashMap::new(),
            sessions: HashMap::new(),
            tx,
            news,
        }
    }

    fn charge(&self, id: LeaseId) -> Lease {
        Lease {
            consumer: Consumer::SessionLaunch(id),
            tx: self.tx.clone(),
        }
    }

    /// A marked wrapper may hold a grant it has not read yet, so each one
    /// counts as running until it re-registers, asks again or exits.
    fn recover(&mut self, wrappers: &[(u32, HeavyMark)], mut next_id: impl FnMut() -> LeaseId) {
        for &(pid, mark) in wrappers {
            let id = next_id();
            let reserved_bytes = mark.reserve_bytes.unwrap_or(UNBOUNDED_BYTES);
            self.dispatcher
                .admit_running(Consumer::Command(id), reserved_bytes);
            tracing::info!(
                lease = id.0,
                pid,
                reserved = reserved_bytes,
                "heavy wrapper found from an earlier daemon"
            );
            self.leases.insert(
                id,
                Entry {
                    command: format!("pid {pid}"),
                    reserved_bytes,
                    wrapper_pid: Some(pid),
                    holder: Holder::Unclaimed { mark },
                    reported: None,
                },
            );
        }
    }

    fn unclaimed(&self, peer: Option<u32>) -> Option<LeaseId> {
        let peer = peer?;
        self.leases
            .iter()
            .find(|(_, e)| {
                e.wrapper_pid == Some(peer) && matches!(e.holder, Holder::Unclaimed { .. })
            })
            .map(|(id, _)| *id)
    }

    fn apply(&mut self, change: Change, heavy: &HeavyConfig) {
        match change {
            Change::Request {
                id,
                reserve_bytes,
                command,
                link,
            } => {
                let limits = heavy.limits();
                let reserved_bytes =
                    reserve_bytes.unwrap_or_else(|| heavy.command_reservation_bytes());
                if reserved_bytes > limits.budget_bytes {
                    tracing::info!(
                        reserved = reserved_bytes,
                        budget = limits.budget_bytes,
                        %command,
                        "heavy command larger than the whole budget; refused"
                    );
                    link.answer_or_close(ServerEvent::HeavyTooLarge {
                        reserved_bytes,
                        budget_bytes: limits.budget_bytes,
                    });
                    return;
                }
                if let Some(counted) = self.unclaimed(link.peer_pid) {
                    self.release(counted);
                }
                self.dispatcher.set_limits(limits);
                self.dispatcher
                    .enqueue(Consumer::Command(id), reserved_bytes);
                tracing::info!(lease = id.0, reserved = reserved_bytes, %command, "heavy command queued");
                self.leases.insert(
                    id,
                    Entry {
                        command,
                        reserved_bytes,
                        wrapper_pid: link.peer_pid,
                        holder: Holder::Wrapper(link),
                        reported: None,
                    },
                );
            }
            Change::Running {
                id,
                reserved_bytes,
                command,
                link,
            } => {
                let Some(counted) = self.unclaimed(link.peer_pid) else {
                    tracing::info!(peer = ?link.peer_pid, %command, "heavy re-registration refused: not a wrapper counted at startup");
                    link.answer_or_close(ServerEvent::HeavyRunningRefused);
                    return;
                };
                let Some(reserved_bytes) = self.leases[&counted]
                    .rejoin_charge(reserved_bytes, heavy.command_reservation_bytes())
                else {
                    tracing::info!(peer = ?link.peer_pid, claimed = reserved_bytes, %command, "heavy re-registration refused: not the size its mark names");
                    link.answer_or_close(ServerEvent::HeavyRunningRefused);
                    return;
                };
                if !link.answer_or_close(ServerEvent::HeavyGranted { reserved_bytes }) {
                    return;
                }
                self.dispatcher.rekey_running(
                    &Consumer::Command(counted),
                    Consumer::Command(id),
                    reserved_bytes,
                );
                self.release(counted);
                tracing::info!(lease = id.0, peer = ?link.peer_pid, reserved = reserved_bytes, %command, "heavy command re-registered");
                self.leases.insert(
                    id,
                    Entry {
                        command,
                        reserved_bytes,
                        wrapper_pid: link.peer_pid,
                        holder: Holder::Wrapper(link),
                        reported: None,
                    },
                );
            }
            Change::Session {
                id,
                agent,
                reserved_bytes,
                answer,
            } => {
                let budget_bytes = heavy.limits().budget_bytes;
                if reserved_bytes > budget_bytes {
                    tracing::info!(%agent, reserved = reserved_bytes, budget = budget_bytes, "session larger than the whole budget; refused");
                    self.refuse_session(id, agent, reserved_bytes, budget_bytes, answer);
                    return;
                }
                self.dispatcher.set_limits(heavy.limits());
                self.dispatcher
                    .enqueue(Consumer::SessionLaunch(id), reserved_bytes);
                tracing::info!(launch = id.0, %agent, reserved = reserved_bytes, "session queued");
                self.sessions.insert(
                    id,
                    SessionEntry {
                        agent,
                        reserved_bytes,
                        pid: None,
                        answer,
                    },
                );
            }
            Change::ForceSession {
                id,
                agent,
                reserved_bytes,
            } => {
                self.cancel_queued(&agent);
                self.dispatcher
                    .admit_running(Consumer::SessionLaunch(id), reserved_bytes);
                tracing::info!(launch = id.0, %agent, reserved = reserved_bytes, "session force-started");
                self.sessions.insert(
                    id,
                    SessionEntry {
                        agent,
                        reserved_bytes,
                        pid: None,
                        answer: Answer::News,
                    },
                );
            }
            Change::CancelSession(agent) => self.cancel_queued(&agent),
            Change::SessionPid { id, pid } => {
                if let Some(entry) = self.sessions.get_mut(&id) {
                    entry.pid = Some(pid);
                }
            }
            Change::Release(Consumer::Command(id)) => self.release(id),
            Change::Release(Consumer::SessionLaunch(id)) => self.release_session(id),
        }
    }

    fn refuse_session(
        &mut self,
        launch: LeaseId,
        agent: AgentId,
        reserved_bytes: u64,
        budget_bytes: u64,
        answer: Answer,
    ) {
        match answer {
            Answer::Caller(reply) => offer(
                &self.news,
                &self.tx,
                reply,
                &agent,
                launch,
                Admission::TooLarge {
                    reserved_bytes,
                    budget_bytes,
                },
            ),
            Answer::News => {
                let _ = self.news.send(SessionNews::Refused {
                    agent,
                    launch,
                    reserved_bytes,
                    budget_bytes,
                });
            }
        }
    }

    fn cancel_queued(&mut self, agent: &AgentId) {
        let queued: Vec<LeaseId> = self
            .sessions
            .iter()
            .filter(|(id, e)| {
                &e.agent == agent
                    && matches!(
                        self.dispatcher.standing(&Consumer::SessionLaunch(**id)),
                        Some(Standing::Queued(_))
                    )
            })
            .map(|(id, _)| *id)
            .collect();
        for id in queued {
            tracing::info!(launch = id.0, %agent, "queued session cancelled");
            self.release_session(id);
        }
    }

    fn release_session(&mut self, id: LeaseId) {
        self.dispatcher.release(&Consumer::SessionLaunch(id));
        if let Some(entry) = self.sessions.remove(&id) {
            tracing::info!(launch = id.0, agent = %entry.agent, reserved = entry.reserved_bytes, "session lease released");
        }
    }

    fn answer_queued_sessions(&mut self) {
        for (id, entry) in &mut self.sessions {
            if let Answer::Caller(reply) = std::mem::replace(&mut entry.answer, Answer::News) {
                offer(
                    &self.news,
                    &self.tx,
                    reply,
                    &entry.agent,
                    *id,
                    Admission::Queued(*id),
                );
            }
        }
    }

    fn grant_session(&mut self, id: LeaseId) -> Result<(), Undelivered> {
        if !self.sessions.contains_key(&id) {
            return Ok(());
        }
        let charge = self.charge(id);
        let entry = self.sessions.get_mut(&id).expect("checked above");
        let delivered = match std::mem::replace(&mut entry.answer, Answer::News) {
            Answer::Caller(reply) => {
                offer(
                    &self.news,
                    &self.tx,
                    reply,
                    &entry.agent,
                    id,
                    Admission::Admitted(charge),
                );
                true
            }
            Answer::News => self
                .news
                .send(SessionNews::Granted {
                    agent: entry.agent.clone(),
                    launch: id,
                    charge,
                })
                .is_ok(),
        };
        if delivered {
            tracing::info!(launch = id.0, agent = %entry.agent, reserved = entry.reserved_bytes, "session admitted");
            return Ok(());
        }
        tracing::info!(launch = id.0, agent = %entry.agent, "session grant undeliverable");
        self.release_session(id);
        Err(Undelivered)
    }

    fn session_roots(&self) -> Vec<(LeaseId, u32)> {
        self.sessions
            .iter()
            .filter_map(|(id, e)| Some((*id, e.pid?)))
            .collect()
    }

    fn observe_sessions(&mut self, roots: Vec<(LeaseId, u32)>, rss: Option<Vec<Option<u64>>>) {
        let Some(rss) = rss else {
            return;
        };
        for ((id, _), bytes) in roots.into_iter().zip(rss) {
            if let Some(bytes) = bytes {
                self.dispatcher
                    .observe_rss(&Consumer::SessionLaunch(id), bytes);
            }
        }
    }

    fn view(&self) -> LedgerView {
        let lease = |id: &LeaseId| {
            let entry = self.leases.get(id)?;
            Some(HeavyLease {
                lease: id.0,
                pid: entry.wrapper_pid,
                reserved_bytes: (entry.reserved_bytes != UNBOUNDED_BYTES)
                    .then_some(entry.reserved_bytes),
                command: entry.command.clone(),
            })
        };
        LedgerView {
            running: self
                .dispatcher
                .running()
                .filter_map(|consumer| match consumer {
                    Consumer::Command(id) => lease(id),
                    Consumer::SessionLaunch(_) => None,
                })
                .collect(),
            queued: self
                .dispatcher
                .queued()
                .filter_map(|(consumer, reserved_bytes)| match consumer {
                    Consumer::Command(id) => lease(id).map(Waiting::Command),
                    Consumer::SessionLaunch(id) => Some(Waiting::Session {
                        agent: self.sessions.get(id)?.agent.clone(),
                        reserved_bytes,
                    }),
                })
                .collect(),
        }
    }

    fn release(&mut self, id: LeaseId) {
        self.dispatcher.release(&Consumer::Command(id));
        if let Some(entry) = self.leases.remove(&id) {
            tracing::info!(lease = id.0, reserved = entry.reserved_bytes, command = %entry.command, "heavy lease released");
        }
    }

    fn admit(&mut self, limits: Limits, machine: &Result<MachineMemory, ProbeError>) {
        self.dispatcher.set_limits(limits);
        let blocked = loop {
            let pump = self.dispatcher.pump(machine);
            let mut freed = false;
            for consumer in pump.refused {
                match consumer {
                    Consumer::Command(id) => {
                        if let Some(entry) = self.leases.remove(&id) {
                            tracing::info!(lease = id.0, reserved = entry.reserved_bytes, command = %entry.command, "heavy command larger than the whole budget; refused at the head");
                            if let Some(link) = entry.holder.link() {
                                link.answer_or_close(ServerEvent::HeavyTooLarge {
                                    reserved_bytes: entry.reserved_bytes,
                                    budget_bytes: limits.budget_bytes,
                                });
                            }
                        }
                    }
                    Consumer::SessionLaunch(id) => {
                        if let Some(entry) = self.sessions.remove(&id) {
                            tracing::info!(launch = id.0, agent = %entry.agent, reserved = entry.reserved_bytes, "session larger than the whole budget; refused at the head");
                            self.refuse_session(
                                id,
                                entry.agent,
                                entry.reserved_bytes,
                                limits.budget_bytes,
                                entry.answer,
                            );
                        }
                    }
                }
            }
            for consumer in pump.admitted {
                freed |= match consumer {
                    Consumer::Command(id) => self.try_grant(id).is_err(),
                    Consumer::SessionLaunch(id) => self.grant_session(id).is_err(),
                };
            }
            if !freed {
                break pump.head_blocked;
            }
        };
        let Some(blocked) = blocked else {
            return;
        };
        for (id, entry) in &mut self.leases {
            let Some(Standing::Queued(index)) = self.dispatcher.standing(&Consumer::Command(*id))
            else {
                continue;
            };
            let report = (index as u32 + 1, blocked);
            if entry.reported != Some(report) {
                entry.reported = Some(report);
                if let Some(link) = entry.holder.link() {
                    let _ = link.events.try_send(ServerEvent::HeavyQueued {
                        position: report.0,
                        blocked,
                    });
                }
            }
        }
    }

    fn try_grant(&mut self, id: LeaseId) -> Result<(), Undelivered> {
        let Some(entry) = self.leases.get(&id) else {
            return Ok(());
        };
        let reserved_bytes = entry.reserved_bytes;
        if entry
            .holder
            .link()
            .is_some_and(|l| l.answer_or_close(ServerEvent::HeavyGranted { reserved_bytes }))
        {
            tracing::info!(lease = id.0, reserved = reserved_bytes, command = %entry.command, "heavy command admitted");
            return Ok(());
        }
        tracing::info!(lease = id.0, command = %entry.command, "heavy grant undeliverable; connection closed");
        self.release(id);
        Err(Undelivered)
    }

    fn roots(&self) -> Vec<(LeaseId, u32)> {
        self.leases
            .iter()
            .filter_map(|(id, e)| Some((*id, e.wrapper_pid?)))
            .filter(|(id, _)| {
                self.dispatcher.standing(&Consumer::Command(*id)) == Some(Standing::Running)
            })
            .collect()
    }

    fn observe(&mut self, roots: Vec<(LeaseId, u32)>, rss: Result<Vec<Option<u64>>, ProbeError>) {
        let Ok(rss) = rss else {
            return;
        };
        for ((id, pid), bytes) in roots.into_iter().zip(rss) {
            match bytes {
                Some(bytes) => self.dispatcher.observe_rss(&Consumer::Command(id), bytes),
                None if confirmed_dead(pid)
                    && self
                        .leases
                        .get(&id)
                        .is_some_and(|e| matches!(e.holder, Holder::Unclaimed { .. })) =>
                {
                    tracing::info!(lease = id.0, pid, "unclaimed heavy wrapper exited");
                    self.release(id);
                }
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn link(capacity: usize) -> (Link, mpsc::Receiver<ServerEvent>) {
        let (events, rx) = mpsc::channel(capacity);
        let link = Link {
            events,
            closed: CancellationToken::new(),
            peer_pid: None,
        };
        (link, rx)
    }

    struct Fixture {
        ledger: Ledger,
        released: mpsc::UnboundedReceiver<Msg>,
        news: mpsc::UnboundedReceiver<SessionNews>,
    }

    fn fixture(limits: Limits) -> Fixture {
        let (tx, released) = mpsc::unbounded_channel();
        let (news_tx, news) = mpsc::unbounded_channel();
        Fixture {
            ledger: Ledger::new(limits, tx, news_tx),
            released,
            news,
        }
    }

    fn request(ledger: &mut Ledger, id: u64, link: &Link) {
        let heavy = HeavyConfig {
            command_reservation_gib: 0.6,
            ..HeavyConfig::default()
        };
        let change = Change::Request {
            id: LeaseId(id),
            reserve_bytes: None,
            command: format!("command {id}"),
            link: link.clone(),
        };
        ledger.apply(change, &heavy);
    }

    #[test]
    fn a_grant_its_client_cannot_take_closes_the_connection_and_admits_the_next() {
        let limits = Limits {
            budget_bytes: GIB,
            floor_bytes: 0,
            psi_some_avg10_max: 100.0,
        };
        let roomy = Ok(MachineMemory::Linux {
            total_bytes: 64 * GIB,
            available_bytes: 64 * GIB,
            swap_total_bytes: 0,
            swap_free_bytes: 0,
            psi_some_avg10: 0.0,
        });
        let Fixture { mut ledger, .. } = fixture(limits);
        let (backed_up, _unread) = link(1);
        backed_up
            .events
            .try_send(ServerEvent::HeavyQueued {
                position: 1,
                blocked: Blocked::OverBudget,
            })
            .unwrap();
        let (next, mut next_events) = link(8);
        request(&mut ledger, 1, &backed_up);
        request(&mut ledger, 2, &next);

        ledger.admit(limits, &roomy);

        assert!(backed_up.closed.is_cancelled());
        assert!(
            matches!(next_events.try_recv(), Ok(ServerEvent::HeavyGranted { .. })),
            "the next command stayed queued behind an undelivered grant"
        );
    }

    const ROOMY: Result<MachineMemory, ProbeError> = Ok(MachineMemory::Linux {
        total_bytes: 64 * GIB,
        available_bytes: 64 * GIB,
        swap_total_bytes: 0,
        swap_free_bytes: 0,
        psi_some_avg10: 0.0,
    });

    const ONE_AT_A_TIME: Limits = Limits {
        budget_bytes: GIB,
        floor_bytes: 0,
        psi_some_avg10_max: 100.0,
    };

    fn agent(name: &str) -> AgentId {
        AgentId(name.into())
    }

    fn session(ledger: &mut Ledger, id: u64, name: &str, bytes: u64) {
        ledger.apply(
            Change::Session {
                id: LeaseId(id),
                agent: agent(name),
                reserved_bytes: bytes,
                answer: Answer::News,
            },
            &one_at_a_time(),
        );
    }

    fn one_at_a_time() -> HeavyConfig {
        HeavyConfig {
            budget_gib: 1.0,
            floor_gib: 0.0,
            psi_some_avg10_max: 100.0,
            command_reservation_gib: 1.0,
            ..HeavyConfig::default()
        }
    }

    fn granted(news: &mut mpsc::UnboundedReceiver<SessionNews>) -> Vec<(AgentId, Lease)> {
        let mut out = Vec::new();
        while let Ok(item) = news.try_recv() {
            match item {
                SessionNews::Granted { agent, charge, .. } => out.push((agent, charge)),
                SessionNews::Refused { agent, .. } => panic!("{agent} refused"),
                SessionNews::Unheard { agent, .. } => panic!("{agent} handed over"),
            }
        }
        out
    }

    fn was_granted(events: &mut mpsc::Receiver<ServerEvent>) -> bool {
        let mut granted = false;
        while let Ok(event) = events.try_recv() {
            granted |= matches!(event, ServerEvent::HeavyGranted { .. });
        }
        granted
    }

    fn release_dropped(ledger: &mut Ledger, released: &mut mpsc::UnboundedReceiver<Msg>) {
        while let Ok(Msg::Change(change)) = released.try_recv() {
            ledger.apply(change, &one_at_a_time());
        }
    }

    #[test]
    fn commands_and_sessions_are_admitted_in_one_fifo_as_room_frees() {
        let Fixture {
            mut ledger,
            mut released,
            mut news,
        } = fixture(ONE_AT_A_TIME);
        let (first, mut first_events) = link(8);
        let (third, mut third_events) = link(8);
        request(&mut ledger, 1, &first);
        session(&mut ledger, 2, "second", GIB);
        request(&mut ledger, 3, &third);

        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        assert!(was_granted(&mut first_events));
        assert!(
            granted(&mut news).is_empty(),
            "the session waits behind the command"
        );

        ledger.apply(
            Change::Release(Consumer::Command(LeaseId(1))),
            &one_at_a_time(),
        );
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        let grants = granted(&mut news);
        assert_eq!(
            grants.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>(),
            [agent("second")]
        );
        assert!(
            !was_granted(&mut third_events),
            "the command behind the session jumped it"
        );

        drop(grants);
        release_dropped(&mut ledger, &mut released);
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        assert!(
            was_granted(&mut third_events),
            "the session's exit admits the next command"
        );
    }

    #[test]
    fn a_forced_start_takes_the_queued_launch_out_so_it_is_never_granted() {
        let Fixture {
            mut ledger,
            mut news,
            ..
        } = fixture(ONE_AT_A_TIME);
        let (blocker, _events) = link(8);
        request(&mut ledger, 1, &blocker);
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        session(&mut ledger, 2, "queued", GIB);
        ledger.apply(
            Change::ForceSession {
                id: LeaseId(3),
                agent: agent("queued"),
                reserved_bytes: GIB,
            },
            &one_at_a_time(),
        );
        assert_eq!(
            ledger
                .dispatcher
                .standing(&Consumer::SessionLaunch(LeaseId(3))),
            Some(Standing::Running)
        );
        assert_eq!(
            ledger
                .dispatcher
                .standing(&Consumer::SessionLaunch(LeaseId(2))),
            None
        );

        ledger.apply(
            Change::Release(Consumer::Command(LeaseId(1))),
            &one_at_a_time(),
        );
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        assert!(
            granted(&mut news).is_empty(),
            "a grant for the forced row would start it twice"
        );
    }

    #[test]
    fn a_launch_releases_only_its_own_charge() {
        let Fixture { mut ledger, .. } = fixture(Limits {
            budget_bytes: 4 * GIB,
            ..ONE_AT_A_TIME
        });
        for id in [1, 2] {
            ledger.apply(
                Change::ForceSession {
                    id: LeaseId(id),
                    agent: agent("restarted"),
                    reserved_bytes: GIB,
                },
                &one_at_a_time(),
            );
        }
        ledger.apply(
            Change::Release(Consumer::SessionLaunch(LeaseId(1))),
            &one_at_a_time(),
        );
        assert_eq!(
            ledger
                .dispatcher
                .standing(&Consumer::SessionLaunch(LeaseId(1))),
            None
        );
        assert_eq!(
            ledger
                .dispatcher
                .standing(&Consumer::SessionLaunch(LeaseId(2))),
            Some(Standing::Running),
            "the old process's exit freed its replacement"
        );
    }

    #[test]
    fn a_queued_session_too_large_for_a_shrunk_budget_is_refused_and_the_next_admitted() {
        let Fixture {
            mut ledger,
            mut news,
            ..
        } = fixture(ONE_AT_A_TIME);
        let (blocker, _events) = link(8);
        let (next, mut next_events) = link(8);
        request(&mut ledger, 1, &blocker);
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        session(&mut ledger, 2, "big", GIB);
        request(&mut ledger, 3, &next);
        ledger.apply(
            Change::Release(Consumer::Command(LeaseId(1))),
            &one_at_a_time(),
        );

        ledger.admit(
            Limits {
                budget_bytes: GIB * 8 / 10,
                ..ONE_AT_A_TIME
            },
            &ROOMY,
        );
        assert!(matches!(
            news.try_recv(),
            Ok(SessionNews::Refused { agent: a, .. }) if a == agent("big")
        ));
        assert!(
            was_granted(&mut next_events),
            "the refused head kept blocking the command behind it"
        );
    }

    fn abandoned_behind_a_blocker(
        ledger: &mut Ledger,
        behind: &Link,
    ) -> mpsc::Receiver<ServerEvent> {
        let (blocker, blocker_events) = link(8);
        request(ledger, 1, &blocker);
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        let (reply, gone) = oneshot::channel();
        drop(gone);
        ledger.apply(
            Change::Session {
                id: LeaseId(2),
                agent: agent("abandoned"),
                reserved_bytes: GIB,
                answer: Answer::Caller(reply),
            },
            &one_at_a_time(),
        );
        request(ledger, 3, behind);
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        ledger.answer_queued_sessions();
        blocker_events
    }

    #[test]
    fn a_caller_that_stopped_waiting_for_its_queued_answer_hands_its_launch_to_the_starter() {
        let Fixture {
            mut ledger,
            mut news,
            ..
        } = fixture(ONE_AT_A_TIME);
        let (behind, mut behind_events) = link(8);
        let _blocker = abandoned_behind_a_blocker(&mut ledger, &behind);
        assert!(
            matches!(
                news.try_recv(),
                Ok(SessionNews::Unheard {
                    agent: a,
                    launch: LeaseId(2),
                    admission: Admission::Queued(LeaseId(2)),
                }) if a == agent("abandoned")
            ),
            "the persisted row was never told it is queued"
        );
        ledger.apply(
            Change::Release(Consumer::Command(LeaseId(1))),
            &one_at_a_time(),
        );
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        let grants = granted(&mut news);
        assert_eq!(grants.len(), 1, "the handed-over launch lost its turn");
        assert_eq!(grants[0].0, agent("abandoned"));
        assert!(
            !was_granted(&mut behind_events),
            "the command jumped the queue"
        );
    }

    fn admitted_to_nobody(ledger: &mut Ledger) {
        let (reply, gone) = oneshot::channel();
        drop(gone);
        ledger.apply(
            Change::Session {
                id: LeaseId(7),
                agent: agent("left"),
                reserved_bytes: GIB,
                answer: Answer::Caller(reply),
            },
            &one_at_a_time(),
        );
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
    }

    #[test]
    fn a_caller_that_left_before_its_admission_hands_the_live_charge_to_the_starter() {
        let Fixture {
            mut ledger,
            mut news,
            mut released,
        } = fixture(ONE_AT_A_TIME);
        admitted_to_nobody(&mut ledger);
        let Ok(SessionNews::Unheard {
            agent: left,
            launch: LeaseId(7),
            admission: Admission::Admitted(charge),
        }) = news.try_recv()
        else {
            panic!("the admission was not handed to the starter");
        };
        assert_eq!(left, agent("left"));
        assert!(news.try_recv().is_err(), "one handoff, not two messages");
        assert!(
            matches!(
                ledger
                    .dispatcher
                    .standing(&Consumer::SessionLaunch(LeaseId(7))),
                Some(Standing::Running)
            ),
            "the handed-over charge stopped counting"
        );
        assert!(
            released.try_recv().is_err(),
            "the charge was given back early"
        );
        drop(charge);
        release_dropped(&mut ledger, &mut released);
        assert_eq!(
            ledger
                .dispatcher
                .standing(&Consumer::SessionLaunch(LeaseId(7))),
            None
        );
    }

    #[test]
    fn with_no_starter_left_an_unheard_admission_is_released() {
        let Fixture {
            mut ledger,
            news,
            mut released,
        } = fixture(ONE_AT_A_TIME);
        drop(news);
        admitted_to_nobody(&mut ledger);
        let releases: Vec<Change> = std::iter::from_fn(|| match released.try_recv() {
            Ok(Msg::Change(change)) => Some(change),
            _ => None,
        })
        .collect();
        assert!(
            matches!(
                releases.as_slice(),
                [Change::Release(Consumer::SessionLaunch(LeaseId(7)))]
            ),
            "released other than exactly once"
        );
        for change in releases {
            ledger.apply(change, &one_at_a_time());
        }
        assert_eq!(
            ledger
                .dispatcher
                .standing(&Consumer::SessionLaunch(LeaseId(7))),
            None
        );
    }

    #[test]
    fn with_no_starter_left_an_abandoned_launch_leaves_no_phantom_head() {
        let Fixture {
            mut ledger,
            news,
            mut released,
        } = fixture(ONE_AT_A_TIME);
        drop(news);
        let (behind, mut behind_events) = link(8);
        let _blocker = abandoned_behind_a_blocker(&mut ledger, &behind);
        release_dropped(&mut ledger, &mut released);
        assert_eq!(
            ledger
                .dispatcher
                .standing(&Consumer::SessionLaunch(LeaseId(2))),
            None,
            "nobody will ever record this launch, yet it still waits in the queue"
        );
        ledger.apply(
            Change::Release(Consumer::Command(LeaseId(1))),
            &one_at_a_time(),
        );
        ledger.admit(ONE_AT_A_TIME, &ROOMY);
        assert!(was_granted(&mut behind_events));
    }

    #[test]
    fn a_session_that_asks_is_answered_admitted_or_queued() {
        let Fixture { mut ledger, .. } = fixture(ONE_AT_A_TIME);
        let ask = |ledger: &mut Ledger, id: u64| {
            let (reply, answer) = oneshot::channel();
            ledger.apply(
                Change::Session {
                    id: LeaseId(id),
                    agent: agent("asker"),
                    reserved_bytes: GIB,
                    answer: Answer::Caller(reply),
                },
                &one_at_a_time(),
            );
            ledger.admit(ONE_AT_A_TIME, &ROOMY);
            ledger.answer_queued_sessions();
            answer
        };
        let first = ask(&mut ledger, 1).try_recv().map(AdmissionOffer::claim);
        assert!(matches!(first, Ok(Admission::Admitted(_))));
        assert!(matches!(
            ask(&mut ledger, 2).try_recv().map(AdmissionOffer::claim),
            Ok(Admission::Queued(LeaseId(2)))
        ));
    }
}
