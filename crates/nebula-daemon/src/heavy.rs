//! Heavy-command leases. One task owns the dispatcher ledger; each client
//! connection holds at most one `Lease`, and dropping it (the connection
//! ended, however the client died) frees the reservation and lets the
//! queue head in. A daemon starts by counting every wrapper that marked
//! itself for it (`HeavyMark`), since an earlier daemon may have granted
//! it a lease.

use crate::config::{Config, HeavyConfig};
use crate::dispatcher::{Consumer, Dispatcher, LeaseId, Limits, Standing, UNBOUNDED_BYTES};
use nebula_core::heavy::{daemon_id, processes, HeavyMark};
use nebula_core::mem::{machine_memory, MachineMemory, ProbeError};
use nebula_core::{
    paths, Blocked, HeavyLease, HeavySnapshot, RunningLease, ServerEvent, SessionRef, Waiting,
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
    id: LeaseId,
    tx: mpsc::UnboundedSender<Msg>,
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
    Release(LeaseId),
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
    pub fn spawn(&self, shutdown: CancellationToken) {
        if let Some(rx) = self.rx.lock().unwrap().take() {
            tokio::spawn(run(rx, self.ids.clone(), shutdown));
        }
    }

    /// Queue a command; the link gets its `HeavyQueued`/`HeavyGranted`.
    pub fn request(&self, reserve_bytes: Option<u64>, command: String, link: Link) -> Lease {
        let lease = self.lease();
        let _ = self.tx.send(Msg::Change(Change::Request {
            id: lease.id,
            reserve_bytes,
            command,
            link,
        }));
        lease
    }

    /// Re-attach a wrapper this daemon counted at startup.
    pub fn running(&self, reserved_bytes: u64, command: String, link: Link) -> Lease {
        let lease = self.lease();
        let _ = self.tx.send(Msg::Change(Change::Running {
            id: lease.id,
            reserved_bytes,
            command,
            link,
        }));
        lease
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

    fn lease(&self) -> Lease {
        Lease {
            id: LeaseId(self.ids.fetch_add(1, Ordering::Relaxed)),
            tx: self.tx.clone(),
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Change(Change::Release(self.id)));
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

struct Undelivered;

struct Ledger {
    dispatcher: Dispatcher,
    leases: HashMap<LeaseId, Entry>,
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Msg>,
    ids: Arc<AtomicU64>,
    shutdown: CancellationToken,
) {
    let heavy = Config::load().heavy;
    let mut ledger = Ledger::new(heavy.limits());
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
                if !roots.is_empty() {
                    let pids: Vec<u32> = roots.iter().map(|(_, pid)| *pid).collect();
                    let rss = tokio::task::spawn_blocking(move || crate::metrics::tree_rss(&pids))
                        .await
                        .unwrap_or_else(|e| Err(ProbeError::Unreadable(e.to_string())));
                    ledger.observe(roots, rss);
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
    }
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
    fn new(limits: Limits) -> Self {
        Self {
            dispatcher: Dispatcher::new(limits),
            leases: HashMap::new(),
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
            Change::Release(id) => self.release(id),
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
                    Consumer::Session(_) => None,
                })
                .collect(),
            queued: self
                .dispatcher
                .queued()
                .filter_map(|(consumer, reserved_bytes)| match consumer {
                    Consumer::Command(id) => lease(id).map(Waiting::Command),
                    Consumer::Session(agent) => Some(Waiting::Session {
                        agent: agent.clone(),
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
            for consumer in pump.admitted {
                if let Consumer::Command(id) = consumer {
                    freed |= self.try_grant(id).is_err();
                }
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
        let mut ledger = Ledger::new(limits);
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
}
