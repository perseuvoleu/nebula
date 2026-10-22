//! The daemon's world: persisted entity tree + live PTY sessions, and the
//! operations the IPC surface exposes over them.

use crate::git;
use crate::hooks::{self, HookEnv};
use crate::pty::{PtyEvent, PtySession, SpawnSpec};
use crate::status::{AgentStatusMachine, Effect, HookEvent};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use nebula_core::ClientRequest;
use nebula_core::{
    Agent, AgentId, AgentKind, AgentStatus, Entity, EntityId, Link, LinkId, Note, NoteId,
    NoteOwner, Project, ProjectId, ServerEvent, SessionRef, TerminalId, TerminalTab, Todo, TodoId,
    TodoOwner, Workspace, WorkspaceId, Worktree, WorktreeId, WriterClaim, BUSY_SESSIONS_REFUSAL,
    WRITER_HELD_REFUSAL,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// A warm agent CLI older than this is reaped — it holds memory and its
/// conversation context grows stale.
const PREWARM_MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// A live same-spec warm CLI older than this is recycled (killed and
/// re-booted fresh) when its slot is re-requested, instead of being kept.
/// Clients keep-warm the selected worktree on a cadence shorter than
/// `PREWARM_MAX_AGE - PREWARM_RECYCLE_AGE`, so a slot they still care about
/// is always refreshed before the reaper can empty it.
const PREWARM_RECYCLE_AGE: Duration = Duration::from_secs(10 * 60);
/// Hook events buffered on a warm session before its row exists (oldest
/// dropped beyond this).
const PREWARM_HOOK_BUFFER_CAP: usize = 64;
/// Lines (scrollback tail plus screen) kept when a stopped session's
/// screen is saved for `ReadSession`.
const SAVED_SCREEN_LINES: usize = 200;

/// A pre-spawned agent CLI waiting to be adopted by the next CreateAgent for
/// the same (worktree, kind). The PTY lives in the normal sessions map under
/// a pre-generated agent id, so its NEBULA_AGENT_ID env is already the id
/// the adopted row will use. Hook events that arrive before the row exists
/// (SessionStart carries the resume session id) are buffered here and
/// replayed at adoption.
struct PrewarmEntry {
    agent_id: AgentId,
    spawned_at: Instant,
    /// Model/effort the warm CLI booted with; a CreateAgent asking for a
    /// different spec can't adopt it (the CLI is already running the wrong
    /// model), so the entry is discarded instead.
    model: Option<String>,
    effort: Option<String>,
    buffered_hooks: Vec<(HookEvent, Option<String>)>,
}

/// Terminal tabs export `NEBULA_AGENT_ID` with this prefix on their
/// TerminalId, so an agent CLI run by hand inside the shell reports
/// through the globally-installed hooks and the daemon can route those
/// events onto the terminal's own status instead of an agent row.
pub const TERMINAL_HOOK_PREFIX: &str = "term:";

/// Wall-clock epoch ms, matching the store's `status_changed_at` stamps.
fn epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct Daemon {
    /// Runtime handoff reservations and accepted prompt notifications.
    pub(crate) collect_owners: Mutex<HashSet<AgentId>>,
    pub(crate) collect_events: broadcast::Sender<crate::collect::CollectEvent>,
    /// Orders source-side prompt/status delivery with guarded collect input.
    status_delivery: Mutex<()>,
    sessions: Mutex<HashMap<SessionRef, Arc<PtySession>>>,
    status_machines: Mutex<HashMap<AgentId, AgentStatusMachine>>,
    /// Status machines for agent CLIs run by hand inside shell tabs (hook
    /// events arriving under a `term:<id>` agent id). Dropped, and the
    /// persisted status cleared, when the tab's PTY dies.
    terminal_status_machines: Mutex<HashMap<TerminalId, AgentStatusMachine>>,
    pub hook_env: HookEnv,
    /// Shared with the hook HTTP server, which reads agent rows to decide
    /// auto-title injection.
    pub store: Arc<Store>,
    /// Entity/status deltas fanned out to every subscribed client.
    pub events: broadcast::Sender<ServerEvent>,
    pub shutdown: tokio_util::sync::CancellationToken,
    /// Serializes worktree create/delete with the background auto-sync so
    /// a checkout is never adopted twice while its row is mid-insert.
    worktree_ops: tokio::sync::Mutex<()>,
    /// Orders physical checkout changes against session creation/resume.
    checkout_execution: tokio::sync::RwLock<()>,
    /// Makes a writer spawn's holder check and its row insert one step.
    writer_claims: tokio::sync::Mutex<()>,
    stop_deadline: Duration,
    agent_cmd_override: Option<String>,
    /// Warm agent CLIs awaiting adoption, at most one per (worktree, kind).
    prewarmed: Mutex<HashMap<(WorktreeId, AgentKind), PrewarmEntry>>,
    /// Cached `command -v` results per CLI so a missing binary doesn't get
    /// re-probed (login shell spawn) on every prewarm request.
    cli_probes: Mutex<HashMap<AgentKind, (bool, Instant)>>,
    /// How many client connections are attached per session — a session
    /// with attachments is "in view" and exempt from idle reaping.
    attach_counts: Mutex<HashMap<SessionRef, usize>>,
    /// When each live session was last "looked at": spawned, prewarmed,
    /// attached, detached, or refreshed by the reaper while attached. The
    /// idle reaper kills sessions whose stamp ages past
    /// `session_idle_timeout`.
    session_interest: Mutex<HashMap<SessionRef, Instant>>,
    /// Last hook-reported cwd per agent, recorded only for payloads that
    /// passed the foreign-session gate. An agent that walks into a checkout
    /// nebula hasn't adopted yet leaves its cwd here, so the worktree sync
    /// can finish the re-home once the row exists.
    last_cwd: Mutex<HashMap<AgentId, PathBuf>>,
    /// One relay per remote host with projects here: mirrors that host
    /// daemon's tree for those projects and forwards our clients' requests
    /// to it (see `relay.rs`).
    relays: Mutex<HashMap<String, Arc<crate::relay::Relay>>>,
}

impl Daemon {
    pub fn new(store: Arc<Store>, hook_env: HookEnv) -> Arc<Self> {
        Self::with_overrides(
            store,
            hook_env,
            DaemonOverrides {
                agent_cmd: std::env::var("NEBULA_AGENT_CMD").ok(),
                ..DaemonOverrides::default()
            },
        )
    }

    /// A daemon built with `overrides` in place of `Daemon::new`'s defaults.
    pub fn with_overrides(
        store: Arc<Store>,
        hook_env: HookEnv,
        overrides: DaemonOverrides,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(1024);
        Arc::new(Self {
            collect_owners: Mutex::new(HashSet::new()),
            collect_events: broadcast::channel(1024).0,
            status_delivery: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            status_machines: Mutex::new(HashMap::new()),
            terminal_status_machines: Mutex::new(HashMap::new()),
            hook_env,
            store,
            events,
            shutdown: tokio_util::sync::CancellationToken::new(),
            worktree_ops: tokio::sync::Mutex::new(()),
            checkout_execution: tokio::sync::RwLock::new(()),
            writer_claims: tokio::sync::Mutex::new(()),
            stop_deadline: overrides.stop_deadline,
            agent_cmd_override: overrides.agent_cmd,
            prewarmed: Mutex::new(HashMap::new()),
            cli_probes: Mutex::new(HashMap::new()),
            attach_counts: Mutex::new(HashMap::new()),
            session_interest: Mutex::new(HashMap::new()),
            last_cwd: Mutex::new(HashMap::new()),
            relays: Mutex::new(HashMap::new()),
        })
    }

    // ---- relays ----

    /// Relays follow the anchors: one per host that has an anchor here,
    /// refreshed when anchors change, torn down (ssh and all) when a host's
    /// last anchor goes. Called at boot and after any anchor add/remove.
    pub fn ensure_relays(self: &Arc<Self>) {
        let Ok((projects, ..)) = self.store.load_tree() else {
            return;
        };
        let hosts: std::collections::HashSet<String> =
            projects.iter().filter_map(|p| p.host.clone()).collect();
        let mut relays = self.relays.lock().unwrap();
        for host in &hosts {
            match relays.get(host) {
                Some(relay) => relay.refresh_anchors(self),
                None => {
                    tracing::info!(host = %host, "starting relay");
                    let relay = crate::relay::Relay::spawn(self.clone(), host.clone());
                    relays.insert(host.clone(), relay);
                }
            }
        }
        let gone: Vec<String> = relays
            .keys()
            .filter(|h| !hosts.contains(*h))
            .cloned()
            .collect();
        for host in gone {
            tracing::info!(host = %host, "stopping relay: no anchors left");
            if let Some(relay) = relays.remove(&host) {
                relay.stop(self);
            }
        }
    }

    /// Removing a mirrored project row means "stop showing this host's
    /// checkout here", never "delete it on the host": the local anchors it
    /// covers go, and the relay takes the rows back.
    fn detach_mirrored_project(self: &Arc<Self>, id: &ProjectId) -> Option<Result<()>> {
        let relays = self.relays.lock().unwrap();
        let (host, root) = relays.values().find_map(|r| {
            let m = r.mirror.lock().unwrap();
            m.projects
                .iter()
                .find(|p| &p.id == id)
                .map(|p| (r.host.clone(), p.repo_path.clone()))
        })?;
        drop(relays);
        Some((|| {
            let (projects, ..) = self.store.load_tree()?;
            for anchor in projects
                .iter()
                .filter(|p| p.host.as_deref() == Some(&host) && p.repo_path.starts_with(&root))
            {
                self.store.delete_project(&anchor.id)?;
            }
            self.refresh_remote_hosts();
            self.ensure_relays();
            Ok(())
        })())
    }

    /// The relay whose mirror a request addresses, if any.
    pub fn relay_for_request(&self, req: &ClientRequest) -> Option<Arc<crate::relay::Relay>> {
        let relays = self.relays.lock().unwrap();
        relays.values().find(|r| r.owns_request(req)).cloned()
    }

    /// Whether a session is mirrored from some host (its PTY isn't here).
    pub fn is_mirrored_session(&self, sref: &SessionRef) -> bool {
        self.relays
            .lock()
            .unwrap()
            .values()
            .any(|r| r.owns_session(sref))
    }

    /// Mirrored entities, for the snapshot. The local `host:/path` rows
    /// stay hidden: the host's own project row (stamped with the host)
    /// stands in for them.
    fn mirrored(&self) -> Vec<crate::relay::Mirror> {
        self.relays
            .lock()
            .unwrap()
            .values()
            .map(|r| r.mirror.lock().unwrap().clone())
            .collect()
    }

    /// Rebuild the process-wide checkout→host map from the store: every
    /// remote project's root plus each of its worktrees. Called at boot and
    /// after anything that adds a project or worktree row, so the git
    /// runner and the spawners route by path without being told a host.
    pub fn refresh_remote_hosts(&self) {
        let Ok((projects, worktrees, _, _)) = self.store.load_tree() else {
            return;
        };
        let mut entries = Vec::new();
        for project in projects.iter().filter(|p| p.host.is_some()) {
            let host = project.host.clone().unwrap_or_default();
            entries.push((project.repo_path.clone(), host.clone()));
            for w in worktrees.iter().filter(|w| w.project_id == project.id) {
                entries.push((w.path.clone(), host.clone()));
            }
        }
        nebula_core::remote::replace_all(entries);
    }

    // ---- status machine plumbing ----

    /// Feed one hook (or synthetic) event through the agent's status machine
    /// and apply the resulting effects (persist + broadcast).
    pub fn apply_hook_event(
        &self,
        agent_id: &AgentId,
        event: HookEvent,
        session_id: Option<String>,
    ) {
        // A `term:`-prefixed id is a shell tab's hook env: the event drives
        // the terminal's own status, not an agent row.
        if let Some(tid) = agent_id.as_str().strip_prefix(TERMINAL_HOOK_PREFIX) {
            self.apply_terminal_hook_event(&TerminalId(tid.to_string()), event, session_id);
            return;
        }
        let (probe, turn) = match self.status_machines.lock().unwrap().get(agent_id) {
            Some(m) => (m.wants_command_probe(&event), m.turn()),
            None => (
                matches!(event, HookEvent::Stop | HookEvent::Progress { busy: false }),
                0,
            ),
        };
        let command_running = probe.then(|| self.agent_command_running(agent_id));
        let _delivery = self.status_delivery.lock().unwrap();
        let submitted = matches!(event, HookEvent::UserPromptSubmit);
        enum Outcome {
            Effects(Vec<Effect>),
            UnknownAgent(HookEvent, Option<String>),
        }
        let outcome = {
            let mut machines = self.status_machines.lock().unwrap();
            let machine = match machines.entry(agent_id.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => Some(e.into_mut()),
                std::collections::hash_map::Entry::Vacant(slot) => {
                    // Lazily seed from the persisted row.
                    match self.store.get_agent(agent_id) {
                        Ok(Some(agent)) => Some(slot.insert(AgentStatusMachine::for_kind(
                            agent.status,
                            agent.session_id,
                            agent.kind,
                        ))),
                        _ => None,
                    }
                }
            };
            match machine {
                Some(m) => {
                    let now = Instant::now();
                    let sid = session_id.as_deref();
                    Outcome::Effects(
                        match command_running.filter(|_| m.wants_command_probe(&event)) {
                            Some(running) => m.handle_probed(event, sid, running, turn, now),
                            None => m.handle(event, sid, now),
                        },
                    )
                }
                None => Outcome::UnknownAgent(event, session_id),
            }
        };
        match outcome {
            Outcome::Effects(effects) => {
                self.apply_status_effects(agent_id, effects);
                // UserPromptSubmit is a session-capturing event, always
                // accepted for a known row. PTY progress may already have
                // set Running, leaving no SetStatus effect for this hook.
                if submitted {
                    let _ = self
                        .collect_events
                        .send(crate::collect::CollectEvent::Prompt(agent_id.clone()));
                }
            }
            // Ids with no row are prewarmed sessions (buffer for replay at
            // adoption) or stale env / deleted agents (dropped, as before).
            Outcome::UnknownAgent(event, session_id) => {
                self.buffer_prewarm_hook(agent_id, event, session_id)
            }
        }
    }

    fn agent_command_running(&self, id: &AgentId) -> bool {
        self.agents_command_running(vec![id.clone()])
            .pop()
            .is_some_and(|(_, running)| running)
    }

    fn agents_command_running(&self, ids: Vec<AgentId>) -> Vec<(AgentId, bool)> {
        let pids: Vec<(AgentId, Option<Option<u32>>)> = {
            let sessions = self.sessions.lock().unwrap();
            ids.into_iter()
                .map(|id| {
                    let pid = sessions
                        .get(&SessionRef::Agent(id.clone()))
                        .map(|session| session.child_pid);
                    (id, pid)
                })
                .collect()
        };
        if pids.iter().all(|(_, pid)| pid.is_none()) {
            return pids.into_iter().map(|(id, _)| (id, false)).collect();
        }
        let parents = command_parents();
        pids.into_iter()
            .map(|(id, pid)| {
                let running = pid.is_some_and(|pid| has_command_child(pid, parents.as_ref()));
                (id, running)
            })
            .collect()
    }

    fn buffer_prewarm_hook(
        &self,
        agent_id: &AgentId,
        event: HookEvent,
        session_id: Option<String>,
    ) {
        let mut pool = self.prewarmed.lock().unwrap();
        if let Some(entry) = pool.values_mut().find(|e| &e.agent_id == agent_id) {
            if entry.buffered_hooks.len() >= PREWARM_HOOK_BUFFER_CAP {
                entry.buffered_hooks.remove(0);
            }
            entry.buffered_hooks.push((event, session_id));
        }
    }

    /// Deferred-finish recheck across all machines (runs on a timer).
    pub fn tick_status_machines(&self) {
        let held: Vec<AgentId> = {
            let machines = self.status_machines.lock().unwrap();
            machines
                .iter()
                .filter(|(_, m)| m.command_held())
                .map(|(id, _)| id.clone())
                .collect()
        };
        let probes = self.agents_command_running(held);
        let _delivery = self.status_delivery.lock().unwrap();
        let now = Instant::now();
        let ticked: Vec<(AgentId, Vec<Effect>)> = {
            let mut machines = self.status_machines.lock().unwrap();
            for (id, running) in probes {
                if let Some(m) = machines.get_mut(&id) {
                    m.handle(HookEvent::CommandProbe { running }, None, now);
                }
            }
            machines
                .iter_mut()
                .map(|(id, m)| (id.clone(), m.tick(now)))
                .collect()
        };
        for (id, effects) in ticked {
            self.apply_status_effects(&id, effects);
        }
        let ticked: Vec<(TerminalId, Vec<Effect>)> = {
            let mut machines = self.terminal_status_machines.lock().unwrap();
            machines
                .iter_mut()
                .map(|(id, m)| (id.clone(), m.tick(now)))
                .collect()
        };
        for (id, effects) in ticked {
            self.apply_terminal_status_effects(&id, effects);
        }
    }

    /// Feed a shell tab's hook event (its env reports as `term:<id>`)
    /// through the terminal's status machine. Ids with no terminal row
    /// (deleted tab, stale env copied elsewhere) are dropped.
    fn apply_terminal_hook_event(
        &self,
        id: &TerminalId,
        event: HookEvent,
        session_id: Option<String>,
    ) {
        let effects = {
            let mut machines = self.terminal_status_machines.lock().unwrap();
            match machines.entry(id.clone()) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    e.into_mut()
                        .handle(event, session_id.as_deref(), Instant::now())
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    match self.store.get_terminal(id) {
                        Ok(Some(term)) => slot
                            .insert(AgentStatusMachine::new(
                                term.status.unwrap_or(AgentStatus::Fresh),
                                None,
                            ))
                            .handle(event, session_id.as_deref(), Instant::now()),
                        _ => return,
                    }
                }
            }
        };
        self.apply_terminal_status_effects(id, effects);
    }

    fn apply_terminal_status_effects(&self, id: &TerminalId, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::SetStatus(status) => {
                    if let Err(e) = self.store.set_terminal_status(id, Some(status)) {
                        tracing::warn!(error = %e, "persist terminal status failed");
                    }
                    // Terminals have no StatusChanged lane — the upsert
                    // carries the new status like any other terminal edit.
                    if let Ok(term) = self.terminal_entity(id) {
                        self.broadcast(ServerEvent::EntityUpserted {
                            entity: Entity::Terminal(term),
                        });
                    }
                }
                // A hand-run CLI's resume id belongs to nobody: there is no
                // agent row to resume it from.
                Effect::SaveSessionId(_) => {}
            }
        }
    }

    /// The tab's shell died — whatever CLI reported status inside it is
    /// gone too: drop its machine and clear the persisted status.
    fn clear_terminal_status(&self, id: &TerminalId) {
        self.terminal_status_machines.lock().unwrap().remove(id);
        if let Err(e) = self.store.set_terminal_status(id, None) {
            tracing::warn!(error = %e, "clear terminal status failed");
        }
    }

    fn apply_status_effects(&self, agent_id: &AgentId, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::SetStatus(status) => {
                    let changed_at = match self.store.set_agent_status(agent_id, status) {
                        Ok(stamp) => stamp,
                        Err(e) => {
                            tracing::warn!(error = %e, "persist status failed");
                            epoch_ms()
                        }
                    };
                    self.broadcast(ServerEvent::StatusChanged {
                        agent: agent_id.clone(),
                        status,
                        changed_at,
                    });
                    let _ = self
                        .collect_events
                        .send(crate::collect::CollectEvent::Status(
                            agent_id.clone(),
                            status,
                        ));
                }
                Effect::SaveSessionId(sid) => {
                    if let Err(e) = self.store.set_agent_session_id(agent_id, Some(&sid)) {
                        tracing::warn!(error = %e, "persist session id failed");
                    }
                }
            }
        }
    }

    /// Read-only source validation for clean collection, including parked
    /// branch writers. Runs on the owning daemon, not the relay mirror.
    pub fn inspect_collect(&self, worktree: &WorktreeId, branch: &str) -> Result<()> {
        let _delivery = self.status_delivery.lock().unwrap();
        let row = self
            .store
            .get_worktree(worktree)?
            .context("source checkout disappeared")?;
        if row.branch != branch {
            bail!("source checkout changed branch; refresh and retry");
        }
        let ServerEvent::Snapshot {
            worktrees,
            agents,
            terminals,
            ..
        } = self.snapshot()?
        else {
            unreachable!()
        };
        let owns = |wid: &WorktreeId, remembered: Option<&str>| {
            wid == worktree
                || worktrees.iter().any(|w| {
                    &w.id == wid
                        && w.project_id == row.project_id
                        && (w.branch == branch || remembered == Some(branch))
                })
        };
        let owners: Vec<_> = agents
            .iter()
            .filter(|a| a.alive && !a.archived && owns(&a.worktree_id, a.branch.as_deref()))
            .collect();
        if owners.len() > 1
            || owners.iter().any(|a| {
                self.collect_owners.lock().unwrap().contains(&a.id)
                    || &a.worktree_id != worktree
                    || a.status != AgentStatus::Finished
                    || a.branch.as_deref().is_some_and(|b| b != branch)
            })
            || terminals
                .iter()
                .any(|t| owns(&t.worktree_id, t.branch.as_deref()))
        {
            bail!("source has busy, ambiguous, or parked writers; settle its sessions before collecting");
        }
        Ok(())
    }

    /// Validate source ownership and atomically check status before writing
    /// handoff input. Called on the source daemon, never its cached mirror.
    pub(crate) fn collect_input(
        &self,
        id: &AgentId,
        worktree: &WorktreeId,
        branch: &str,
        revision: i64,
        data: &[u8],
    ) -> Result<()> {
        let _delivery = self.status_delivery.lock().unwrap();
        let machines = self.status_machines.lock().unwrap();
        let agent = self.agent_entity(id)?;
        if agent.archived
            || !agent.alive
            || agent.status != AgentStatus::Finished
            || agent.status_changed_at != revision
            || &agent.worktree_id != worktree
            || machines
                .get(id)
                .is_some_and(|m| m.status() != AgentStatus::Finished)
        {
            bail!("source owner changed or is busy; inspect its session and retry");
        }
        let row = self
            .store
            .get_worktree(worktree)?
            .context("source checkout disappeared")?;
        if row.branch != branch || agent.branch.as_deref().is_some_and(|b| b != branch) {
            bail!("source checkout/owner changed branch; retry after inspecting ownership");
        }
        let ServerEvent::Snapshot {
            worktrees,
            agents,
            terminals,
            ..
        } = self.snapshot()?
        else {
            unreachable!()
        };
        let owns = |wid: &WorktreeId, remembered: Option<&str>| {
            wid == worktree
                || worktrees.iter().any(|w| {
                    &w.id == wid
                        && w.project_id == row.project_id
                        && (w.branch == branch || remembered == Some(branch))
                })
        };
        if agents.iter().any(|a| {
            &a.id != id && a.alive && !a.archived && owns(&a.worktree_id, a.branch.as_deref())
        }) || terminals
            .iter()
            .any(|t| owns(&t.worktree_id, t.branch.as_deref()))
        {
            bail!("multiple source writers; settle ownership before collecting");
        }
        self.session(&SessionRef::Agent(id.clone()))
            .context("source PTY disappeared")?
            .write_input(data)?;
        Ok(())
    }

    pub fn broadcast(&self, ev: ServerEvent) {
        let _ = self.events.send(ev);
    }

    pub fn session(&self, sref: &SessionRef) -> Option<Arc<PtySession>> {
        self.sessions.lock().unwrap().get(sref).cloned()
    }

    pub fn is_alive(&self, sref: &SessionRef) -> bool {
        self.sessions.lock().unwrap().contains_key(sref)
    }

    /// (session, child pid) for every live PTY — the metrics reading's input.
    pub fn session_pids(&self) -> Vec<(SessionRef, u32)> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(sref, s)| s.child_pid.map(|pid| (sref.clone(), pid)))
            .collect()
    }

    pub fn remove_session(&self, sref: &SessionRef) -> Option<Arc<PtySession>> {
        self.session_interest.lock().unwrap().remove(sref);
        self.sessions.lock().unwrap().remove(sref)
    }

    pub fn kill_session(&self, sref: &SessionRef) {
        if let Some(s) = self.remove_session(sref) {
            self.save_screen(&s);
            s.kill();
        }
    }

    pub fn kill_all(&self) {
        let sessions: Vec<Arc<PtySession>> = self
            .sessions
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s)
            .collect();
        for s in sessions {
            self.save_screen(&s);
            s.kill();
        }
    }

    /// Keep the stopping session's screen on disk for `ReadSession`. Rows
    /// without a store entry (the prewarm pool) are never read back.
    fn save_screen(&self, session: &PtySession) {
        if self.session_worktree(&session.sref).is_none() {
            return;
        }
        let (_, data) = session.snapshot(None);
        let (cols, rows) = session.size();
        let text = crate::pty::render::ring_to_text(&data, cols, rows, Some(SAVED_SCREEN_LINES));
        let path = nebula_core::paths::saved_screen_path(&session.sref);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, text));
        if let Err(e) = written {
            tracing::warn!(session = ?session.sref, error = %e, "could not save the last screen");
        }
    }

    fn forget_screen(sref: &SessionRef) {
        let _ = std::fs::remove_file(nebula_core::paths::saved_screen_path(sref));
    }

    // ---- attach tracking & idle reaping ----

    /// A client attached to `sref` (the server dedupes re-attaches per
    /// connection). While any attachment exists, the session counts as
    /// "in view".
    pub fn note_attached(&self, sref: &SessionRef) {
        *self
            .attach_counts
            .lock()
            .unwrap()
            .entry(sref.clone())
            .or_insert(0) += 1;
        self.touch_session(sref);
    }

    /// A client detached from `sref` (or its connection dropped). Restamps
    /// the session so the idle clock starts at "stopped looking", not at
    /// spawn time.
    pub fn note_detached(&self, sref: &SessionRef) {
        let mut counts = self.attach_counts.lock().unwrap();
        if let Some(n) = counts.get_mut(sref) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                counts.remove(sref);
            }
        }
        drop(counts);
        self.touch_session(sref);
    }

    /// Stamp `sref` as just-looked-at for the idle reaper.
    fn touch_session(&self, sref: &SessionRef) {
        self.session_interest
            .lock()
            .unwrap()
            .insert(sref.clone(), Instant::now());
    }

    /// Kill idle sessions no client is attached to, per
    /// `session_idle_timeout` — this bounds what prewarming and
    /// walked-away-from sessions cost. "In view" = attached (main pane or
    /// split), not the whole worktree: workers pile up on the primary, so a
    /// worktree-wide rule would keep hundreds of them alive. Attached
    /// sessions get their stamps refreshed instead, and detach stamps too,
    /// so the full timeout starts only when the user leaves the tab.
    /// Spared regardless of age: pinned agents (the user's "never kill
    /// this" mark — a schedule or loop is invisible to the status machine),
    /// agents that are running, waiting on feedback, or still have a command
    /// running (a background job outlives the turn that started it),
    /// terminals with a command running, and prewarm-pool sessions
    /// (`reap_prewarmed` owns those). A reaped session revives on the next
    /// attach or prewarm; agents resume their conversation.
    pub fn reap_idle_sessions(self: &Arc<Self>) {
        let Some(timeout) = crate::config::Config::load().session_idle_timeout() else {
            return;
        };
        let sessions: Vec<(SessionRef, Arc<PtySession>)> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let attached: std::collections::HashSet<SessionRef> =
            self.attach_counts.lock().unwrap().keys().cloned().collect();
        let now = Instant::now();
        for (sref, session) in sessions {
            // No store row = prewarm-pool session (or deleted mid-sweep).
            if self.session_worktree(&sref).is_none() {
                continue;
            }
            if attached.contains(&sref) {
                self.touch_session(&sref);
                continue;
            }
            let age = {
                let mut interest = self.session_interest.lock().unwrap();
                // A missing stamp (session predating the map) starts aging now.
                now.duration_since(*interest.entry(sref.clone()).or_insert(now))
            };
            if age < timeout {
                continue;
            }
            let spared = match &sref {
                SessionRef::Agent(id) => match self.store.get_agent(id).ok().flatten() {
                    // Pinned = the user marked it worth keeping (schedules,
                    // loops the status can't see) — never reap.
                    Some(agent) => {
                        agent.pinned
                            || matches!(
                                agent.status,
                                AgentStatus::Running | AgentStatus::NeedsFeedback
                            )
                            || agent_has_command_child(&session)
                    }
                    // Row vanished mid-sweep: its delete kills the PTY anyway.
                    None => true,
                },
                SessionRef::Terminal(_) => shell_has_children(&session),
            };
            if spared {
                continue;
            }
            tracing::info!(session = ?sref, idle_secs = age.as_secs(), "reaping idle session");
            self.kill_session(&sref);
            let upsert = match &sref {
                SessionRef::Agent(id) => self.agent_entity(id).map(Entity::Agent),
                SessionRef::Terminal(id) => self.terminal_entity(id).map(Entity::Terminal),
            };
            if let Ok(entity) = upsert {
                self.broadcast(ServerEvent::EntityUpserted { entity });
            }
        }
    }

    /// The worktree a session's row lives under; None when the row is gone
    /// or never existed (prewarm pool).
    fn session_worktree(&self, sref: &SessionRef) -> Option<WorktreeId> {
        match sref {
            SessionRef::Agent(id) => self
                .store
                .get_agent(id)
                .ok()
                .flatten()
                .map(|a| a.worktree_id),
            SessionRef::Terminal(id) => self
                .store
                .get_terminal(id)
                .ok()
                .flatten()
                .map(|t| t.worktree_id),
        }
    }

    // ---- snapshot ----

    pub fn snapshot(&self) -> Result<ServerEvent> {
        let (mut projects, mut worktrees, mut agents, mut terminals) = self.store.load_tree()?;
        let mut notes = self.store.load_notes()?;
        let mut todos = self.store.load_todos()?;
        let mut links = self.store.load_links()?;
        // Remote projects are shown through their host daemon's rows.
        projects.retain(|p| p.host.is_none());
        for m in self.mirrored() {
            projects.extend(m.projects);
            worktrees.extend(m.worktrees);
            agents.extend(m.agents);
            terminals.extend(m.terminals);
            notes.extend(m.notes);
            todos.extend(m.todos);
            links.extend(m.links);
        }
        {
            let sessions = self.sessions.lock().unwrap();
            // Mirrored rows keep the liveness their host reported.
            for a in &mut agents {
                if self.is_mirrored_session(&SessionRef::Agent(a.id.clone())) {
                    continue;
                }
                a.alive = sessions.contains_key(&SessionRef::Agent(a.id.clone()));
            }
            for t in &mut terminals {
                let sref = SessionRef::Terminal(t.id.clone());
                if self.is_mirrored_session(&sref) {
                    continue;
                }
                t.alive = sessions.contains_key(&sref);
                t.busy = sessions
                    .get(&sref)
                    .and_then(|s| s.progress_busy())
                    .unwrap_or(false);
            }
        }
        Ok(ServerEvent::Snapshot {
            workspaces: self.store.load_workspaces()?,
            active_workspace: self.store.active_workspace_id()?,
            projects,
            worktrees,
            agents,
            terminals,
            notes,
            todos,
            links,
            pr_seen: self.store.load_pr_seen()?,
            ui_state: self.store.load_ui_state()?,
        })
    }

    fn agent_entity(&self, id: &AgentId) -> Result<Agent> {
        let mut agent = self.store.get_agent(id)?.context("agent not found")?;
        agent.alive = self.is_alive(&SessionRef::Agent(id.clone()));
        Ok(agent)
    }

    fn terminal_entity(&self, id: &TerminalId) -> Result<TerminalTab> {
        let mut term = self.store.get_terminal(id)?.context("terminal not found")?;
        let sref = SessionRef::Terminal(id.clone());
        let sessions = self.sessions.lock().unwrap();
        term.alive = sessions.contains_key(&sref);
        term.busy = sessions
            .get(&sref)
            .and_then(|s| s.progress_busy())
            .unwrap_or(false);
        Ok(term)
    }

    // ---- workspaces ----

    /// Validated, trimmed workspace name, checked for collisions (excluding
    /// `except` on renames).
    fn checked_workspace_name(&self, name: &str, except: Option<&WorkspaceId>) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            bail!("workspace name is empty");
        }
        if let Some(existing) = self.store.workspace_by_name(name)? {
            if Some(&existing) != except {
                bail!("a workspace named '{name}' already exists");
            }
        }
        Ok(name.to_string())
    }

    /// Create a workspace. Does not open it — `workspace open` stays a
    /// separate, explicit step.
    pub fn add_workspace(self: &Arc<Self>, name: &str) -> Result<EntityId> {
        let name = self.checked_workspace_name(name, None)?;
        let workspace = Workspace {
            id: WorkspaceId::generate(),
            name,
        };
        self.store.insert_workspace(&workspace)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Workspace(workspace.clone()),
        });
        Ok(EntityId::Workspace(workspace.id))
    }

    pub fn rename_workspace(self: &Arc<Self>, id: &WorkspaceId, name: &str) -> Result<()> {
        let mut workspace = self
            .store
            .get_workspace(id)?
            .context("workspace not found")?;
        workspace.name = self.checked_workspace_name(name, Some(id))?;
        self.store.rename_workspace(id, &workspace.name)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Workspace(workspace),
        });
        Ok(())
    }

    /// Delete a workspace. Only empty ones go — its projects are the user's
    /// to move or remove first — and never the last one. Deleting the open
    /// workspace opens another one first so clients always have a live scope.
    pub fn remove_workspace(self: &Arc<Self>, id: &WorkspaceId) -> Result<()> {
        self.store
            .get_workspace(id)?
            .context("workspace not found")?;
        let projects = self.store.count_workspace_projects(id)?;
        if projects > 0 {
            bail!(
                "workspace still has {projects} project{} — remove them first",
                if projects == 1 { "" } else { "s" }
            );
        }
        if self.store.count_workspaces()? <= 1 {
            bail!("cannot delete the last workspace");
        }
        if self.store.active_workspace_id()? == *id {
            let fallback = self
                .store
                .load_workspaces()?
                .into_iter()
                .find(|w| &w.id != id)
                .context("no workspace left to open")?;
            self.store.set_active_workspace(&fallback.id)?;
            self.broadcast(ServerEvent::ActiveWorkspaceChanged { id: fallback.id });
        }
        self.store.delete_workspace(id)?;
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Workspace(id.clone()),
        });
        Ok(())
    }

    /// Make `id` the open workspace (daemon-global; every client follows).
    pub fn open_workspace(self: &Arc<Self>, id: &WorkspaceId) -> Result<()> {
        self.store
            .get_workspace(id)?
            .context("workspace not found")?;
        if self.store.active_workspace_id()? == *id {
            return Ok(()); // already open
        }
        self.store.set_active_workspace(id)?;
        self.broadcast(ServerEvent::ActiveWorkspaceChanged { id: id.clone() });
        Ok(())
    }

    // ---- projects ----

    pub async fn add_project(
        self: &Arc<Self>,
        path: &Path,
        name: Option<String>,
        create_missing: bool,
        host: Option<String>,
    ) -> Result<EntityId> {
        // A remote checkout is an *anchor*: the host's own daemon adopts
        // the repo (normalizing to its toplevel, rejecting non-repos) once
        // the relay hands it the path, and mirrors everything back. Nothing
        // to probe from here beyond a `~`, which only the remote shell can
        // expand.
        if let Some(host) = host {
            let path = if path.starts_with("~") {
                let rest = path.strip_prefix("~").unwrap_or(path);
                git::remote_home(&host)
                    .await
                    .with_context(|| format!("resolve ~ on {host}"))?
                    .join(rest)
            } else {
                path.to_path_buf()
            };
            let workspace_id = self.store.active_workspace_id()?;
            if self
                .store
                .project_in_workspace_on(&path, &workspace_id, Some(&host))?
                .is_some()
            {
                bail!(
                    "project already added to this workspace: {host}:{}",
                    path.display()
                );
            }
            let name = name.unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "project".into())
            });
            let project = Project {
                id: ProjectId::generate(),
                name,
                workspace_id,
                repo_path: path,
                sort_order: self.store.next_project_sort_order()?,
                divider_after: false,
                divider_label: None,
                divider_before: false,
                divider_before_label: None,
                host: Some(host),
            };
            self.store.insert_project(&project)?;
            self.refresh_remote_hosts();
            self.ensure_relays();
            // A local checkout of the same project lends the new anchor
            // its `.env`s — the host's clone has none — in the background,
            // and never over ones the host already has.
            let (projects, ..) = self.store.load_tree()?;
            if let Some(twin) = projects.iter().find(|p| {
                p.host.is_none() && p.name == project.name && p.workspace_id == project.workspace_id
            }) {
                let local = twin.repo_path.clone();
                let host = project.host.clone().unwrap_or_default();
                let remote = project.repo_path.clone();
                tokio::task::spawn_blocking(move || {
                    let result = nebula_core::envfiles::list_local(&local).and_then(|files| {
                        nebula_core::envfiles::push(&local, &host, &remote, &files, false)
                    });
                    match result {
                        Ok(p) => tracing::info!(
                            host,
                            sent = p.sent.len(),
                            kept = p.kept.len(),
                            "env files pushed to the new anchor"
                        ),
                        Err(error) => tracing::warn!(host, error = %error, "env files not pushed"),
                    }
                });
            }
            return Ok(EntityId::Project(project.id));
        }
        if create_missing && !path.exists() {
            tokio::fs::create_dir_all(path)
                .await
                .with_context(|| format!("create {}", path.display()))?;
            if crate::config::Config::load().git_init_on_create {
                git::init(path).await?;
            }
        }
        // "not a git repository" is the right explanation only when git ran and
        // said no — if git itself is missing, that message blames the wrong
        // thing, so let git.rs's own diagnosis through untouched.
        let toplevel = git::repo_toplevel(path).await.map_err(|e| {
            if git::is_missing(&e) {
                e
            } else {
                e.context(format!("{} is not a git repository", path.display()))
            }
        })?;
        // New projects land in whichever workspace is open; the same repo
        // may be added to any number of workspaces, just not twice to one.
        let workspace_id = self.store.active_workspace_id()?;
        if self
            .store
            .project_in_workspace(&toplevel, &workspace_id)?
            .is_some()
        {
            bail!(
                "project already added to this workspace: {}",
                toplevel.display()
            );
        }
        let name = name.unwrap_or_else(|| {
            toplevel
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".into())
        });
        let project = Project {
            id: ProjectId::generate(),
            name,
            workspace_id,
            repo_path: toplevel.clone(),
            sort_order: self.store.next_project_sort_order()?,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        self.store.insert_project(&project)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Project(project.clone()),
        });

        // Main checkout is modeled as a worktree row; adopt pre-existing
        // worktrees too so `nebula` matches reality on day one.
        let entries = git::list_worktrees(&toplevel).await.unwrap_or_default();
        let mut first = true;
        for entry in entries {
            let created_from = if first {
                None
            } else {
                git::branch_creation_base(&toplevel, &entry.branch)
                    .await
                    .filter(|b| b != &entry.branch)
            };
            let worktree = Worktree {
                id: WorktreeId::generate(),
                project_id: project.id.clone(),
                path: entry.path.clone(),
                branch: entry.branch,
                is_main: first,
                created_from,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            };
            first = false;
            self.store.insert_worktree(&worktree)?;
            self.refresh_remote_hosts();
            self.broadcast(ServerEvent::EntityUpserted {
                entity: Entity::Worktree(worktree),
            });
        }
        Ok(EntityId::Project(project.id))
    }

    pub fn remove_project(self: &Arc<Self>, id: &ProjectId) -> Result<()> {
        if let Some(result) = self.detach_mirrored_project(id) {
            return result;
        }
        // Kill any live sessions under this project first.
        let (all_projects, worktrees, agents, terminals) = self.store.load_tree()?;
        // Divider bookkeeping is per-workspace: the list clients see is the
        // removed project's workspace, so its neighbors are found there.
        let workspace = all_projects
            .iter()
            .find(|p| &p.id == id)
            .map(|p| p.workspace_id.clone());
        let projects: Vec<Project> = all_projects
            .iter()
            .filter(|p| Some(&p.workspace_id) == workspace.as_ref())
            .cloned()
            .collect();
        // The leading divider belongs to the list, not the top project:
        // removing that project hands it down to the next one.
        if let (Some(first), Some(second)) = (projects.first(), projects.get(1)) {
            if &first.id == id && first.divider_before {
                let mut heir = second.clone();
                heir.divider_before = true;
                heir.divider_before_label = first.divider_before_label.clone();
                self.store.set_project_position(&heir)?;
                self.broadcast(ServerEvent::EntityUpserted {
                    entity: Entity::Project(heir),
                });
            }
        }
        let wt_ids: Vec<WorktreeId> = worktrees
            .into_iter()
            .filter(|w| &w.project_id == id)
            .map(|w| w.id)
            .collect();
        for a in agents.iter().filter(|a| wt_ids.contains(&a.worktree_id)) {
            let sref = SessionRef::Agent(a.id.clone());
            self.kill_session(&sref);
            Self::forget_screen(&sref);
        }
        for t in terminals.iter().filter(|t| wt_ids.contains(&t.worktree_id)) {
            let sref = SessionRef::Terminal(t.id.clone());
            self.kill_session(&sref);
            Self::forget_screen(&sref);
        }
        self.kill_prewarmed_in(&wt_ids);
        // Removing a project only forgets it in nebula — never touches disk.
        self.store.delete_project(id)?;
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Project(id.clone()),
        });
        Ok(())
    }

    /// Move a project `delta` rows in the displayed list, where dividers
    /// occupy rows of their own (clamped at the edges). A project steps into
    /// the gap beside a divider before it swaps with the next project, so
    /// repeated single-row moves can park it directly above or below a
    /// divider — including below a divider that ends up above the whole
    /// list (the leading divider). Sort orders are rewritten to the display
    /// index for every project, which also normalizes legacy all-zero
    /// orders on first use.
    pub fn move_project(self: &Arc<Self>, id: &ProjectId, delta: i64) -> Result<()> {
        let (all_projects, _, _, _) = self.store.load_tree()?;
        // Reorders happen within the project's workspace — the list clients
        // actually see. Other workspaces' rows keep their sort orders; the
        // rewrite below only renumbers this workspace's slice, which stays
        // correctly interleaved because clients filter before ordering.
        let workspace = all_projects
            .iter()
            .find(|p| &p.id == id)
            .map(|p| p.workspace_id.clone());
        let projects: Vec<Project> = all_projects
            .into_iter()
            .filter(|p| Some(&p.workspace_id) == workspace.as_ref())
            .collect();
        #[derive(Clone, PartialEq)]
        enum Row {
            Project(ProjectId),
            Divider(Option<String>),
        }
        let mut rows: Vec<Row> = Vec::new();
        if let Some(first) = projects.first() {
            if first.divider_before {
                rows.push(Row::Divider(first.divider_before_label.clone()));
            }
        }
        for p in &projects {
            rows.push(Row::Project(p.id.clone()));
            if p.divider_after {
                rows.push(Row::Divider(p.divider_label.clone()));
            }
        }
        let Some(pos) = rows
            .iter()
            .position(|r| matches!(r, Row::Project(pid) if pid == id))
        else {
            bail!("project not found");
        };
        let mut target = (pos as i64 + delta).clamp(0, rows.len() as i64 - 1) as usize;
        let rows = loop {
            let mut moved = rows.clone();
            let row = moved.remove(pos);
            moved.insert(target, row);
            // Two dividers can't share a gap: a move that would leave them
            // stacked (no project left between) pushes one row further so
            // the keystroke still lands the project across, or no-ops at
            // the edge.
            let stacked = moved
                .windows(2)
                .any(|w| matches!(w, [Row::Divider(_), Row::Divider(_)]));
            if !stacked && moved != rows {
                break moved;
            }
            let next = (target as i64 + delta.signum()).clamp(0, rows.len() as i64 - 1) as usize;
            if next == target {
                return Ok(());
            }
            target = next;
        };
        type Position = (i64, bool, Option<String>, bool, Option<String>);
        fn position(p: &Project) -> Position {
            (
                p.sort_order,
                p.divider_after,
                p.divider_label.clone(),
                p.divider_before,
                p.divider_before_label.clone(),
            )
        }
        let before: HashMap<ProjectId, Position> = projects
            .iter()
            .map(|p| (p.id.clone(), position(p)))
            .collect();
        let mut by_id: HashMap<ProjectId, Project> =
            projects.into_iter().map(|p| (p.id.clone(), p)).collect();
        let mut ordered: Vec<Project> = Vec::new();
        let mut leading: Option<Option<String>> = None;
        for row in rows {
            match row {
                Row::Project(pid) => {
                    let mut project = by_id.remove(&pid).expect("row ids come from projects");
                    project.sort_order = ordered.len() as i64;
                    project.divider_after = false;
                    project.divider_label = None;
                    project.divider_before = false;
                    project.divider_before_label = None;
                    ordered.push(project);
                }
                Row::Divider(label) => match ordered.last_mut() {
                    Some(owner) => {
                        owner.divider_after = true;
                        owner.divider_label = label;
                    }
                    // Ahead of every project: the leading divider, re-owned
                    // by whichever project ends up on top.
                    None => leading = Some(label),
                },
            }
        }
        if let Some(label) = leading {
            let first = ordered.first_mut().expect("rows contain every project");
            first.divider_before = true;
            first.divider_before_label = label;
        }
        for project in &ordered {
            if before.get(&project.id) != Some(&position(project)) {
                self.store.set_project_position(project)?;
                self.broadcast(ServerEvent::EntityUpserted {
                    entity: Entity::Project(project.clone()),
                });
            }
        }
        Ok(())
    }

    /// Set or clear one of a project's dividers. `before` addresses the
    /// leading divider (drawn above the whole list) — only the first
    /// project can carry that one.
    pub fn set_project_divider(
        self: &Arc<Self>,
        id: &ProjectId,
        before: bool,
        present: bool,
        label: Option<String>,
    ) -> Result<()> {
        let mut project = self.store.get_project(id)?.context("project not found")?;
        if before && present {
            let (projects, _, _, _) = self.store.load_tree()?;
            // "First" within the project's workspace — the list clients see.
            let first = projects
                .iter()
                .find(|p| p.workspace_id == project.workspace_id)
                .map(|p| &p.id);
            if first != Some(id) {
                bail!("only the first project can hold the leading divider");
            }
        }
        // A removed divider keeps no label.
        let label = if present {
            label.filter(|l| !l.trim().is_empty())
        } else {
            None
        };
        let slot = if before {
            (
                &mut project.divider_before,
                &mut project.divider_before_label,
            )
        } else {
            (&mut project.divider_after, &mut project.divider_label)
        };
        if (*slot.0, &*slot.1) == (present, &label) {
            return Ok(());
        }
        *slot.0 = present;
        *slot.1 = label;
        self.store.set_project_position(&project)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Project(project),
        });
        Ok(())
    }

    /// Move project `id`'s divider (`before` picks which one) to the
    /// neighboring gap (sign of `delta`; one step per call). The divider
    /// under the first project can hop above it — the leading divider —
    /// and back down. No-op past the list's edges or when the destination
    /// gap already has a divider — two dividers can't share a gap.
    pub fn move_divider(self: &Arc<Self>, id: &ProjectId, before: bool, delta: i64) -> Result<()> {
        if delta == 0 {
            return Ok(());
        }
        let (all_projects, _, _, _) = self.store.load_tree()?;
        // Neighbors live in the project's workspace — the list clients see.
        let workspace = all_projects
            .iter()
            .find(|p| &p.id == id)
            .map(|p| p.workspace_id.clone());
        let projects: Vec<Project> = all_projects
            .into_iter()
            .filter(|p| Some(&p.workspace_id) == workspace.as_ref())
            .collect();
        let Some(index) = projects.iter().position(|p| &p.id == id) else {
            bail!("project not found");
        };
        let down = delta.signum() > 0;
        if before {
            if index != 0 || !projects[0].divider_before {
                bail!("project has no leading divider");
            }
            if !down {
                return Ok(()); // already above everything
            }
            // Hop from above the first project to below it.
            if projects[0].divider_after {
                return Ok(());
            }
            let label = projects[0].divider_before_label.clone();
            self.set_project_divider(id, true, false, None)?;
            self.set_project_divider(id, false, true, label)?;
            return Ok(());
        }
        if !projects[index].divider_after {
            bail!("project has no divider");
        }
        let label = projects[index].divider_label.clone();
        if index == 0 && !down {
            // Hop from below the first project to above it.
            if projects[0].divider_before {
                return Ok(());
            }
            self.set_project_divider(id, false, false, None)?;
            self.set_project_divider(id, true, true, label)?;
            return Ok(());
        }
        let neighbor = index as i64 + delta.signum();
        let Some(neighbor) = usize::try_from(neighbor).ok().and_then(|i| projects.get(i)) else {
            return Ok(()); // no project on that side
        };
        if neighbor.divider_after {
            return Ok(());
        }
        let neighbor_id = neighbor.id.clone();
        self.set_project_divider(id, false, false, None)?;
        self.set_project_divider(&neighbor_id, false, true, label)?;
        Ok(())
    }

    // ---- worktrees ----

    /// Resolve an existing branch onto this host's guarded primary checkout.
    /// The lifecycle lock serializes switches; branch intent never creates a WT.
    pub async fn resolve_branch_checkout(
        self: &Arc<Self>,
        project_id: &ProjectId,
        branch: &str,
    ) -> Result<EntityId> {
        let _ops = self.worktree_ops.lock().await;
        let project = self
            .store
            .get_project(project_id)?
            .context("project not found")?;
        // Remote ids must have been routed to their owning daemon by the relay.
        if project.host.is_some() {
            bail!("host checkout is unavailable; reconnect the host and retry");
        }
        if branch.trim().is_empty() || !git::branch_exists(&project.repo_path, branch).await {
            bail!("branch \"{branch}\" does not exist on this host; create or fetch it explicitly first");
        }
        self.checkout_primary_locked(project_id, branch, None, false)
            .await?;
        let (_, worktrees, _, _) = self.store.load_tree()?;
        let primary = worktrees
            .into_iter()
            .find(|w| w.project_id == *project_id && w.is_main)
            .context("primary checkout not found")?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Worktree(primary.clone()),
        });
        Ok(EntityId::Worktree(primary.id))
    }

    /// Create an explicitly requested task worktree, preserving existing branch behavior.
    pub async fn create_worktree(
        self: &Arc<Self>,
        project_id: &ProjectId,
        branch: &str,
        base: Option<&str>,
    ) -> Result<EntityId> {
        if branch.trim().is_empty() {
            bail!("branch name is empty");
        }
        let _ops = self.worktree_ops.lock().await;
        self.create_worktree_locked(project_id, branch, base).await
    }

    async fn create_worktree_locked(
        self: &Arc<Self>,
        project_id: &ProjectId,
        branch: &str,
        base: Option<&str>,
    ) -> Result<EntityId> {
        let project = self
            .store
            .get_project(project_id)?
            .context("project not found")?;
        let seed_primary = if crate::config::Config::load().seed_node_modules {
            match self.store.get_primary_worktree_path(project_id) {
                Ok(Some(primary)) => Some(primary),
                Ok(None) => {
                    tracing::warn!(
                        project = %project.name,
                        "node_modules seed skipped: primary checkout is not registered"
                    );
                    None
                }
                Err(error) => {
                    tracing::warn!(
                        project = %project.name,
                        error = %error,
                        "node_modules seed skipped: could not read primary checkout"
                    );
                    None
                }
            }
        } else {
            None
        };
        // Lineage for the panel's tree lines: an explicit base wins;
        // otherwise a pre-existing branch remembers its creation base in
        // the reflog, and a branch minted right here branches from the
        // root checkout's HEAD (which `git worktree add` reads too).
        let pre_existing = git::branch_exists(&project.repo_path, branch).await;
        // Capture display lineage before creating the task branch. A full
        // commit pin at the primary tip inherits the primary's branch name;
        // do not guess among other refs (siblings may share that same tip).
        // Git still receives the original base, so its commit stays pinned.
        let display_base = if let Some(b) = base {
            let is_oid = matches!(b.len(), 40 | 64) && b.bytes().all(|c| c.is_ascii_hexdigit());
            if !pre_existing && is_oid && !git::branch_exists(&project.repo_path, b).await {
                match git::current_branch(&project.repo_path).await {
                    Ok(primary) if !primary.starts_with("detached@") => {
                        match git::branch_tip(&project.repo_path, &primary).await {
                            Some(tip) if tip.eq_ignore_ascii_case(b) => Some(primary),
                            _ => Some(b.to_owned()),
                        }
                    }
                    _ => Some(b.to_owned()),
                }
            } else {
                Some(b.to_owned())
            }
        } else {
            None
        };
        let path = git::add_worktree(&project.repo_path, branch, base).await?;
        // The root checkout's `.env`s come along — synchronously, they are
        // tiny, and a session started right after must find them.
        match git::seed_env_files(&project.repo_path, &path).await {
            Ok(files) if !files.is_empty() => {
                tracing::info!(project = %project.name, count = files.len(), "env files seeded")
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(project = %project.name, error = %error, "env files not seeded")
            }
        }
        if let Some(primary) = seed_primary.filter(|_| project.host.is_none()) {
            git::seed_node_modules_in_background(&primary, &path);
        }
        let created_from = match display_base {
            Some(b) => Some(b),
            None if pre_existing => git::branch_creation_base(&project.repo_path, branch).await,
            None => git::current_branch(&project.repo_path)
                .await
                .ok()
                .filter(|b| !b.starts_with("detached@")),
        }
        .filter(|b| b != branch);
        let worktree = Worktree {
            id: WorktreeId::generate(),
            project_id: project_id.clone(),
            path,
            branch: branch.to_string(),
            is_main: false,
            created_from,
            pinned: false,
            // Preserve explicit worktree metadata; deletion policy is unchanged.
            for_branch: pre_existing,
            sort_order: 0,
        };
        self.store.insert_worktree(&worktree)?;
        self.refresh_remote_hosts();
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Worktree(worktree.clone()),
        });
        Ok(EntityId::Worktree(worktree.id))
    }

    /// The sessions criterion of a safe removal. Archived agents and exited
    /// terminals go with the checkout only under `with_archived`; live or
    /// unarchived sessions always refuse. Returns what removal would delete.
    fn removal_sessions(
        &self,
        id: &WorktreeId,
        agents: &[Agent],
        terminals: &[TerminalTab],
        with_archived: bool,
    ) -> (git::Verdict, Vec<SessionRef>) {
        let mut live = 0;
        let mut unarchived = 0;
        let mut archived = 0;
        let mut finished = 0;
        let mut leftovers = Vec::new();
        for a in agents.iter().filter(|a| a.worktree_id == *id) {
            let sref = SessionRef::Agent(a.id.clone());
            if self.is_alive(&sref) {
                live += 1;
            } else if !a.archived {
                unarchived += 1;
            } else {
                archived += 1;
                leftovers.push(sref);
            }
        }
        for t in terminals.iter().filter(|t| t.worktree_id == *id) {
            let sref = SessionRef::Terminal(t.id.clone());
            if self.is_alive(&sref) {
                live += 1;
            } else {
                finished += 1;
                leftovers.push(sref);
            }
        }
        let stored = format!("{archived} archived session(s) and {finished} finished terminal(s)");
        let verdict = if live + unarchived > 0 {
            git::Verdict::Refuse(format!("Sessions still exist: {live} live, {unarchived} stopped but not archived (and {stored}); stop and archive or delete them first"))
        } else if leftovers.is_empty() {
            git::Verdict::Pass("none".into())
        } else if !with_archived {
            git::Verdict::Refuse(format!("Sessions still exist: {stored}; pass --with-archived to delete them with the checkout"))
        } else {
            git::Verdict::Pass(format!("{stored} go with it (--with-archived)"))
        };
        (verdict, leftovers)
    }

    fn delete_sessions(self: &Arc<Self>, srefs: &[SessionRef]) -> Result<()> {
        for sref in srefs {
            match sref {
                SessionRef::Agent(id) => self.delete_agent(id)?,
                SessionRef::Terminal(id) => self.close_terminal(id)?,
            }
        }
        Ok(())
    }

    /// Unlike legacy delete, never stops user sessions or deletes a branch.
    /// Every criterion is evaluated and reported; removal needs all to pass.
    // Checkout lifecycle operations take worktree_ops before exclusive execution.
    // Create/move/reparent/prewarm hold shared execution so no session can arrive
    // between the safety check and removal; ordinary reconciliation can continue.
    pub async fn check_worktree_removal(
        self: &Arc<Self>,
        id: &WorktreeId,
        branch: &str,
        path: &Path,
        delete: bool,
        with_archived: bool,
    ) -> Result<RemovalReport> {
        let _ops = self.worktree_ops.lock().await;
        let _execution = self.checkout_execution.write().await;
        let w = self
            .store
            .get_worktree(id)?
            .context("Checkout no longer available")?;
        let project = self
            .store
            .get_project(&w.project_id)?
            .context("Project unavailable")?;
        if w.is_main
            || matches!(w.branch.as_str(), "main" | "master")
            || w.path == project.repo_path
        {
            bail!("Primary checkout and main/master are protected");
        }
        if w.branch != branch || w.path != path {
            bail!("Checkout changed; refresh before confirming");
        }
        if git::current_branch(path).await? != branch {
            bail!("Checkout branch changed; refresh before confirming");
        }
        git::safe_remove_worktree(&project.repo_path, &w.path, false).await?;
        let mut report = RemovalReport::default();
        report.push(
            "checkout",
            git::Verdict::Pass(format!(
                "{} on {branch}; the branch is kept",
                w.path.display()
            )),
        );
        report.push("files", git::removal_files(&project.repo_path, path).await?);
        match git::merge_baseline(&project.repo_path, w.created_from.as_deref()).await {
            Some(base) => {
                report.push("base", git::Verdict::Pass(format!("{} (from {})", base.short(), base.source)));
                report.push("merged", git::merged_into(path, &base.refname).await?);
            }
            None => report.push("base", git::Verdict::Refuse("Unmerged commits: no merge baseline (created_from, nebula.base, remote default branch or main/master)".into())),
        }
        let (_, _, agents, terminals) = self.store.load_tree()?;
        let (verdict, leftovers) = self.removal_sessions(id, &agents, &terminals, with_archived);
        report.push("sessions", verdict);
        if delete && report.refusals.is_empty() {
            git::safe_remove_worktree(&project.repo_path, &w.path, true).await?;
            self.delete_sessions(&leftovers)?;
            self.kill_prewarmed_in(std::slice::from_ref(id));
            self.store.delete_worktree(id)?;
            self.broadcast(ServerEvent::EntityRemoved {
                id: EntityId::Worktree(id.clone()),
            });
        }
        Ok(report)
    }

    /// Drop rows whose checkout directory and git worktree entry are both
    /// gone. The sync keeps such a row while sessions point at it; here
    /// archived ones go with it under `with_archived`, live ones never do.
    pub async fn prune_worktrees(
        self: &Arc<Self>,
        project_id: &ProjectId,
        delete: bool,
        with_archived: bool,
    ) -> Result<RemovalReport> {
        let _ops = self.worktree_ops.lock().await;
        let _execution = self.checkout_execution.write().await;
        let project = self
            .store
            .get_project(project_id)?
            .context("Project unavailable")?;
        if nebula_core::remote::is_remote(&project.repo_path) {
            bail!("prune runs on the host that owns the checkouts; use the remote project's own nebula");
        }
        let entries = git::list_worktrees(&project.repo_path).await?;
        let (_, worktrees, agents, terminals) = self.store.load_tree()?;
        let mut report = RemovalReport::default();
        for w in worktrees
            .iter()
            .filter(|w| w.project_id == *project_id && !w.is_main)
        {
            let registered = entries.iter().any(|e| e.path == w.path);
            if w.path.exists() {
                continue;
            }
            if registered {
                report.push(&format!("ghost {} {}", w.branch, w.path.display()),
                    git::Verdict::Refuse("checkout directory is gone but git still lists it; run `git worktree prune` first".into()));
                continue;
            }
            let (verdict, leftovers) =
                self.removal_sessions(&w.id, &agents, &terminals, with_archived);
            let removable = matches!(verdict, git::Verdict::Pass(_));
            report.push(&format!("ghost {} {}", w.branch, w.path.display()), verdict);
            if delete && removable {
                self.delete_sessions(&leftovers)?;
                self.store.delete_worktree(&w.id)?;
                self.broadcast(ServerEvent::EntityRemoved {
                    id: EntityId::Worktree(w.id.clone()),
                });
            }
        }
        if report.lines.is_empty() {
            report
                .lines
                .push("no ghost rows: every row has its checkout or git worktree entry".into());
        }
        Ok(report)
    }

    pub async fn delete_worktree(self: &Arc<Self>, id: &WorktreeId, force: bool) -> Result<()> {
        let _ops = self.worktree_ops.lock().await;
        let _execution = self.checkout_execution.write().await;
        let worktree = self.store.get_worktree(id)?.context("worktree not found")?;
        if worktree.is_main {
            bail!("cannot delete the main checkout — remove the project instead");
        }
        let project = self
            .store
            .get_project(&worktree.project_id)?
            .context("project not found")?;

        // A row presenting as a branch deletes as one. When the branch was
        // created in place inside a pre-existing checkout (the path isn't
        // nebula's dir for this branch), the checkout reverts to the
        // recorded base and the row survives — sessions keep running; only
        // the branch goes away.
        if worktree.for_branch
            && worktree.path != git::worktree_dir(&project.repo_path, &worktree.branch)
        {
            self.require_checkout_unoccupied(id, None)?;
            let base = worktree.created_from.clone();
            let target = base.as_deref().unwrap_or("-");
            // A dirty checkout can make the plain revert refuse (changes
            // that conflict with the base). The confirm dialog warned; a
            // forced delete retries discarding them.
            match git::checkout(&worktree.path, target).await {
                Err(_) if force => git::checkout_forced(&worktree.path, target).await?,
                r => r?,
            }
            if let Err(e) = git::delete_branch(&project.repo_path, &worktree.branch).await {
                tracing::warn!(
                    branch = %worktree.branch, error = %e,
                    "checkout reverted but the branch survived"
                );
            }
            let now = git::current_branch(&worktree.path)
                .await
                .unwrap_or_else(|_| worktree.branch.clone());
            self.store.update_worktree_branch(id, &now)?;
            // The stored base described the deleted branch, not the one the
            // checkout reverted to.
            self.store.set_worktree_created_from(id, None)?;
            self.store.set_worktree_for_branch(id, false)?;
            let updated = self.store.get_worktree(id)?.context("worktree not found")?;
            self.broadcast(ServerEvent::EntityUpserted {
                entity: Entity::Worktree(updated),
            });
            return Ok(());
        }

        // Kill sessions living in this worktree.
        let (_, _, agents, terminals) = self.store.load_tree()?;
        for a in agents.iter().filter(|a| &a.worktree_id == id) {
            let sref = SessionRef::Agent(a.id.clone());
            self.kill_session(&sref);
            Self::forget_screen(&sref);
        }
        for t in terminals.iter().filter(|t| &t.worktree_id == id) {
            let sref = SessionRef::Terminal(t.id.clone());
            self.kill_session(&sref);
            Self::forget_screen(&sref);
        }
        self.kill_prewarmed_in(std::slice::from_ref(id));

        git::remove_worktree(&project.repo_path, &worktree.path, force).await?;
        self.store.delete_worktree(id)?;
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Worktree(id.clone()),
        });
        // The checkout existed to host sessions on a branch the user made
        // as a branch: deleting the row means deleting the branch too,
        // unmerged commits included — the confirm dialog warned.
        if worktree.for_branch {
            if let Err(e) = git::delete_branch(&project.repo_path, &worktree.branch).await {
                tracing::warn!(
                    branch = %worktree.branch, error = %e,
                    "checkout removed but the branch survived"
                );
            }
        }
        Ok(())
    }

    pub fn set_worktree_pinned(self: &Arc<Self>, id: &WorktreeId, pinned: bool) -> Result<()> {
        self.store.set_worktree_pinned(id, pinned)?;
        let worktree = self.store.get_worktree(id)?.context("worktree not found")?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Worktree(worktree),
        });
        Ok(())
    }

    /// Move primary to `branch`. Refused, before any session is stopped, when
    /// `branch` exists neither here nor on origin, primary has uncommitted
    /// tracked changes, an untracked file would block the checkout, `branch`
    /// already has a linked checkout, or a live session other than `mover`
    /// sits on primary and `stop_busy` is false (the error then starts with
    /// `BUSY_SESSIONS_REFUSAL` and lists every one of them).
    /// Otherwise every other session on primary, warm PTYs included, has its
    /// PTY stopped and its child waited for before the checkout; stopped rows
    /// keep their conversation and the branch they were on. Only `mover`
    /// keeps running and follows `branch`.
    pub async fn checkout_primary(
        self: &Arc<Self>,
        project_id: &ProjectId,
        branch: &str,
        mover: Option<&SessionRef>,
        stop_busy: bool,
    ) -> Result<()> {
        let _ops = self.worktree_ops.lock().await;
        self.checkout_primary_locked(project_id, branch, mover, stop_busy)
            .await
    }

    /// Caller holds worktree_ops; never reacquire it through the public wrapper.
    async fn checkout_primary_locked(
        self: &Arc<Self>,
        project_id: &ProjectId,
        branch: &str,
        mover: Option<&SessionRef>,
        stop_busy: bool,
    ) -> Result<()> {
        let branch = branch.trim();
        if branch.is_empty() {
            bail!("branch name is empty");
        }
        let _execution = self.checkout_execution.write().await;
        let project = self
            .store
            .get_project(project_id)?
            .context("project not found")?;
        if project.host.is_some() {
            bail!("host checkout is unavailable; reconnect the host and retry");
        }
        self.reconcile_project_worktrees(&project).await?;
        let (_, checkouts, _, _) = self.store.load_tree()?;
        let primary = checkouts
            .iter()
            .find(|w| w.project_id == *project_id && w.is_main)
            .context("primary checkout not found")?;
        let current = git::current_branch(&primary.path).await?;
        if current == branch {
            return Ok(());
        }
        if let Some(session) = mover {
            let owner = match session {
                SessionRef::Agent(id) => self.store.get_agent(id)?.map(|a| a.worktree_id),
                SessionRef::Terminal(id) => self.store.get_terminal(id)?.map(|t| t.worktree_id),
            };
            if owner.as_ref() != Some(&primary.id) {
                bail!("only a session in the primary checkout may move itself with that checkout");
            }
        }
        if !git::branch_exists(&project.repo_path, branch).await {
            // Not here yet — a branch pushed from another machine can be
            // fetched into place; one that was never pushed cannot.
            if let Err(e) = git::fetch_branch(&project.repo_path, branch).await {
                tracing::debug!(branch, error = %e, "fetch from origin failed");
                bail!("no branch \"{branch}\" here or on origin — push it first, then retry");
            }
        }
        let (_, worktrees, _, _) = self.store.load_tree()?;
        let parked: Vec<&Worktree> = worktrees
            .iter()
            .filter(|w| w.project_id == *project_id && !w.is_main && w.branch == branch)
            .collect();
        if let Some(w) = parked.first() {
            bail!(
                "branch \"{branch}\" already has a checkout at {}; primary was not switched. Explicitly select it with --worktree, or free the parked branch before retrying",
                w.path.display()
            );
        }
        let dirty = git::uncommitted_count(&primary.path).await?;
        if dirty > 0 {
            let files = if dirty == 1 { "file" } else { "files" };
            bail!("primary has {dirty} uncommitted {files} on \"{current}\"; commit them before switching to \"{branch}\"");
        }
        if let Some(path) = git::untracked_blocking_checkout(&primary.path, branch).await? {
            bail!("untracked \"{path}\" in primary would be overwritten by \"{branch}\"; move or commit it before switching");
        }
        let occupants = self.primary_occupants(&primary.id, mover)?;
        if !occupants.is_empty() && !stop_busy {
            let stopping: Vec<String> = occupants.iter().map(Occupant::describe).collect();
            bail!(
                "{BUSY_SESSIONS_REFUSAL} \"{current}\"; switching to \"{branch}\" stops {}",
                stopping.join(", ")
            );
        }
        let mut stops: Vec<Stop> = self
            .prewarmed_in(&primary.id)
            .into_iter()
            .map(|(sref, session)| Stop {
                sref,
                label: "a warm agent PTY".to_owned(),
                session,
                kind: StopKind::Warm,
            })
            .collect();
        for occupant in &occupants {
            if let Some(session) = self.session(&occupant.sref) {
                stops.push(Stop {
                    sref: occupant.sref.clone(),
                    label: occupant.describe(),
                    session,
                    kind: StopKind::Park,
                });
            }
        }
        for stop in &stops {
            if let StopKind::Park = stop.kind {
                self.stamp_branch(&stop.sref, &current)?;
            }
        }
        for stop in &stops {
            stop.session.kill();
        }
        self.finish_stops(stops, &current).await?;
        git::checkout(&project.repo_path, branch).await?;
        // The main row's branch label follows the checkout via reconcile.
        self.reconcile_project_worktrees(&project).await?;
        // Stopped sessions retain expected affinity for safe resume validation.
        // A sole caller explicitly moving itself updates that expectation.
        if let Some(mover) = mover {
            self.follow_branch(mover, branch)?;
        }
        Ok(())
    }

    async fn finish_stops(&self, mut stops: Vec<Stop>, branch: &str) -> Result<()> {
        let started = Instant::now();
        loop {
            stops.retain(|stop| {
                let running = child_running(&stop.session);
                if !running {
                    self.finalize_stop(stop);
                }
                running
            });
            if stops.is_empty() {
                return Ok(());
            }
            if started.elapsed() >= self.stop_deadline {
                let labels: Vec<&str> = stops.iter().map(|s| s.label.as_str()).collect();
                bail!(
                    "{} still running; primary stays on \"{branch}\"",
                    labels.join(", ")
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn finalize_stop(&self, stop: &Stop) {
        self.unregister_if_current(&stop.sref, &stop.session);
        match stop.kind {
            StopKind::Park => self.publish_session_end(&stop.sref, SessionEnd::Parked),
            StopKind::Warm => self
                .prewarmed
                .lock()
                .unwrap()
                .retain(|_, e| SessionRef::Agent(e.agent_id.clone()) != stop.sref),
        }
    }

    fn unregister_if_current(&self, sref: &SessionRef, session: &Arc<PtySession>) -> bool {
        let removed = {
            let mut sessions = self.sessions.lock().unwrap();
            let current = sessions.get(sref).is_some_and(|s| Arc::ptr_eq(s, session));
            if current {
                sessions.remove(sref);
            }
            current
        };
        if removed {
            self.session_interest.lock().unwrap().remove(sref);
        }
        removed
    }

    async fn wait_for_checkout_switch(&self) {
        drop(self.checkout_execution.read().await);
    }

    fn primary_occupants(
        &self,
        primary: &WorktreeId,
        mover: Option<&SessionRef>,
    ) -> Result<Vec<Occupant>> {
        let (_, _, agents, terminals) = self.store.load_tree()?;
        let agents = agents
            .iter()
            .filter(|a| &a.worktree_id == primary)
            .filter_map(|a| {
                let sref = SessionRef::Agent(a.id.clone());
                self.is_alive(&sref).then(|| Occupant {
                    sref,
                    name: a.name.clone(),
                    status: Some(a.status),
                })
            });
        let terminals = terminals
            .iter()
            .filter(|t| &t.worktree_id == primary)
            .filter_map(|t| {
                let sref = SessionRef::Terminal(t.id.clone());
                self.is_alive(&sref).then(|| Occupant {
                    sref,
                    name: t.name.clone(),
                    status: None,
                })
            });
        Ok(agents
            .chain(terminals)
            .filter(|o| Some(&o.sref) != mover)
            .collect())
    }

    fn stamp_branch(&self, sref: &SessionRef, branch: &str) -> Result<()> {
        match sref {
            SessionRef::Agent(id) => {
                if self
                    .store
                    .get_agent(id)?
                    .is_some_and(|a| a.branch.is_none())
                {
                    self.store.set_agent_branch(id, Some(branch))?;
                }
            }
            SessionRef::Terminal(id) => {
                if self
                    .store
                    .get_terminal(id)?
                    .is_some_and(|t| t.branch.is_none())
                {
                    self.store.set_terminal_branch(id, Some(branch))?;
                }
            }
        }
        Ok(())
    }

    fn publish_session_end(&self, sref: &SessionRef, end: SessionEnd) {
        match (sref, end) {
            (SessionRef::Agent(id), SessionEnd::Exited(exit_code)) => {
                self.apply_hook_event(id, HookEvent::SessionEnded { exit_code }, None);
            }
            (SessionRef::Agent(id), SessionEnd::Parked) => {
                self.apply_hook_event(id, HookEvent::Parked, None);
            }
            (SessionRef::Terminal(id), _) => self.clear_terminal_status(id),
        }
        let upsert = match sref {
            SessionRef::Agent(id) => self.agent_entity(id).map(Entity::Agent),
            SessionRef::Terminal(id) => self.terminal_entity(id).map(Entity::Terminal),
        };
        if let Ok(entity) = upsert {
            self.broadcast(ServerEvent::EntityUpserted { entity });
        }
    }

    /// Refuse to invalidate any live PTY, including finished agents and warm sessions.
    fn require_checkout_unoccupied(
        &self,
        id: &WorktreeId,
        mover: Option<&SessionRef>,
    ) -> Result<()> {
        let (_, _, agents, terminals) = self.store.load_tree()?;
        let occupied = agents
            .iter()
            .filter(|a| &a.worktree_id == id)
            .map(|a| SessionRef::Agent(a.id.clone()))
            .chain(
                terminals
                    .iter()
                    .filter(|t| &t.worktree_id == id)
                    .map(|t| SessionRef::Terminal(t.id.clone())),
            )
            .any(|s| Some(&s) != mover && self.is_alive(&s));
        let warm = self
            .prewarmed
            .lock()
            .unwrap()
            .iter()
            .any(|((w, _), entry)| {
                w == id && self.is_alive(&SessionRef::Agent(entry.agent_id.clone()))
            });
        if occupied || warm {
            bail!("primary checkout has another live session; close the other sessions or let warm PTYs expire, then retry (finished agents and shells still count); no worktree was created");
        }
        Ok(())
    }

    /// Read Git at the execution boundary, not only the periodically reconciled row.
    /// A legacy mismatch requires explicit recovery; changing metadata cannot move a PTY.
    fn validate_execution_branch(&self, worktree: &Worktree, recorded: Option<&str>) -> Result<()> {
        let project = self
            .store
            .get_project(&worktree.project_id)?
            .context("project not found")?;
        if project.host.is_some() {
            bail!("execution belongs to another host; reconnect that host and retry");
        }
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&worktree.path)
            .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
            .output()
            .context("validate session checkout with git")?;
        if !output.status.success() {
            bail!("cannot resume in {}: checkout is missing or detached; restore its branch checkout explicitly first", worktree.path.display());
        }
        let actual = String::from_utf8(output.stdout)?.trim().to_owned();
        let expected = recorded.unwrap_or(&worktree.branch);
        if actual != expected {
            bail!("session expects branch \"{expected}\" but {} is on \"{actual}\"; live processes are unchanged. Restore that checkout to \"{expected}\" after explicitly closing other live sessions, or open a new session on \"{expected}\"", worktree.path.display());
        }
        Ok(())
    }

    /// Record the branch a sole caller explicitly moved its checkout onto.
    fn follow_branch(self: &Arc<Self>, session: &SessionRef, branch: &str) -> Result<()> {
        let entity = match session {
            SessionRef::Agent(id) => {
                if self.store.get_agent(id)?.is_none() {
                    return Ok(());
                }
                self.store.set_agent_branch(id, Some(branch))?;
                Entity::Agent(self.agent_entity(id)?)
            }
            SessionRef::Terminal(id) => {
                if self.store.get_terminal(id)?.is_none() {
                    return Ok(());
                }
                self.store.set_terminal_branch(id, Some(branch))?;
                Entity::Terminal(self.terminal_entity(id)?)
            }
        };
        self.broadcast(ServerEvent::EntityUpserted { entity });
        Ok(())
    }

    /// Reconcile a project's worktree rows with `git worktree list` so
    /// checkouts made outside nebula (an agent running `git worktree add`,
    /// manual CLI use) appear without a restart. Adopts unknown checkouts;
    /// refreshes the branch on known rows after an in-place checkout;
    /// drops rows whose checkout vanished — except the main row and rows
    /// that still have sessions, which the user must delete deliberately.
    pub async fn sync_project_worktrees(self: &Arc<Self>, project: &Project) -> Result<()> {
        let adopted = {
            let _ops = self.worktree_ops.lock().await;
            self.reconcile_project_worktrees(project).await?
        };
        // Outside the ops lock: the replay only touches agent rows, and a
        // just-adopted checkout is exactly where a session that ran
        // `git worktree add` itself already lives.
        if adopted {
            self.reparent_agents_by_last_cwd(project);
        }
        Ok(())
    }

    /// The reconcile half of `sync_project_worktrees`. Returns whether any
    /// checkout was newly adopted.
    async fn reconcile_project_worktrees(self: &Arc<Self>, project: &Project) -> Result<bool> {
        let mut adopted = false;
        let entries = git::list_worktrees(&project.repo_path).await?;
        let (_, worktrees, agents, terminals) = self.store.load_tree()?;
        let ours: Vec<&Worktree> = worktrees
            .iter()
            .filter(|w| w.project_id == project.id)
            .collect();
        for entry in &entries {
            if let Some(known) = ours.iter().find(|w| w.path == entry.path) {
                // Branch switched in place (checkout on the root or inside a
                // linked worktree): refresh the stored name so the row tracks
                // reality instead of the branch at adoption time.
                let mut updated = (*known).clone();
                let mut changed = false;
                if known.branch != entry.branch {
                    self.store
                        .update_worktree_branch(&known.id, &entry.branch)?;
                    updated.branch = entry.branch.clone();
                    changed = true;
                    // The checkout now hosts a branch the user switched to
                    // (or created) in its terminal — not the worktree they
                    // asked for by name. The row presents as a branch from
                    // here on, like a checkout created for one.
                    if !known.is_main && !known.for_branch {
                        self.store.set_worktree_for_branch(&known.id, true)?;
                        updated.for_branch = true;
                    }
                }
                // Rows from before lineage was recorded (or adopted without
                // it) backfill from the reflog, so the panel's tree lines
                // appear without recreating the worktree. An in-place branch
                // switch re-derives too — the stored base described the old
                // branch. The primary checkout gets the same treatment: the
                // branch it holds nests under the branch it was cut from,
                // so `nebula switch` on the primary keeps the tree intact.
                if known.created_from.is_none() || changed {
                    let mut base =
                        git::branch_creation_base(&project.repo_path, &entry.branch).await;
                    if base.is_none() && known.branch != entry.branch {
                        // `checkout -b` from an implicit HEAD logs only
                        // `Created from HEAD`. The sync watched the switch,
                        // so the branch this checkout sat on IS that HEAD —
                        // claimed only when the creation sha still matches
                        // its tip, so switching to some old branch never
                        // invents lineage.
                        let created_at =
                            git::branch_creation_sha_from_head(&project.repo_path, &entry.branch)
                                .await;
                        let old_tip = git::branch_tip(&project.repo_path, &known.branch).await;
                        if created_at.is_some() && created_at == old_tip {
                            base = Some(known.branch.clone());
                        }
                    }
                    let base = base.filter(|b| b != &entry.branch);
                    if base != known.created_from {
                        // A switch onto a branch with no recorded base
                        // clears the old one rather than keeping a
                        // lineage that described the previous branch.
                        self.store
                            .set_worktree_created_from(&known.id, base.as_deref())?;
                        updated.created_from = base;
                        changed = true;
                    }
                }
                if changed {
                    self.broadcast(ServerEvent::EntityUpserted {
                        entity: Entity::Worktree(updated),
                    });
                }
                continue;
            }
            let worktree = Worktree {
                id: WorktreeId::generate(),
                project_id: project.id.clone(),
                path: entry.path.clone(),
                branch: entry.branch.clone(),
                is_main: false,
                created_from: git::branch_creation_base(&project.repo_path, &entry.branch)
                    .await
                    .filter(|b| b != &entry.branch),
                pinned: false,
                for_branch: false,
                sort_order: 0,
            };
            self.store.insert_worktree(&worktree)?;
            self.refresh_remote_hosts();
            adopted = true;
            self.broadcast(ServerEvent::EntityUpserted {
                entity: Entity::Worktree(worktree),
            });
        }
        for w in ours {
            if w.is_main || entries.iter().any(|e| e.path == w.path) {
                continue;
            }
            let occupied = agents.iter().any(|a| a.worktree_id == w.id)
                || terminals.iter().any(|t| t.worktree_id == w.id);
            if occupied {
                continue;
            }
            self.store.delete_worktree(&w.id)?;
            self.broadcast(ServerEvent::EntityRemoved {
                id: EntityId::Worktree(w.id.clone()),
            });
        }
        Ok(adopted)
    }

    // ---- agents ----

    #[allow(clippy::too_many_arguments)]
    /// Spawn in a host-owned checkout, validating a branch intent when supplied.
    pub async fn create_agent(
        self: &Arc<Self>,
        worktree_id: &WorktreeId,
        expected_branch: Option<&str>,
        name: &str,
        kind: AgentKind,
        model: Option<String>,
        effort: Option<String>,
        auto_title: bool,
        prompt: Option<String>,
        writer: WriterClaim,
    ) -> Result<EntityId> {
        let _execution = self.checkout_execution.read().await;
        let worktree = self
            .store
            .get_worktree(worktree_id)?
            .context("worktree not found")?;
        self.validate_execution_branch(&worktree, expected_branch.or(Some(&worktree.branch)))?;
        let _claim = match writer {
            WriterClaim::Off => None,
            _ => Some(self.writer_claims.lock().await),
        };
        if writer == WriterClaim::Exclusive {
            if let Some(holder) = self.live_writer(worktree_id)? {
                bail!(
                    "\"{holder}\" {WRITER_HELD_REFUSAL} {}; wait for it, archive it, or pass --force-writer after the user approves two writers",
                    worktree.branch
                );
            }
        }
        // A warm session for this (worktree, kind) hands over its PTY and
        // its pre-generated id — the CLI booted while the user typed the
        // name, so the create feels instant. An initial prompt forces the
        // cold path: it rides the CLI's argv, which a booted CLI can't take.
        let adopted = if prompt.is_some() {
            None
        } else {
            self.take_prewarmed(worktree_id, kind, &model, &effort)
        };
        // Only the cold path needs asking: an adopted warm session is proof
        // the CLI runs. Without this, a missing CLI still "succeeds" — the
        // login shell prints `command not found` into a PTY that dies at
        // once, leaving a dead row that looks identical to a fresh one.
        if adopted.is_none() && !self.cli_available_for_create(kind).await {
            bail!("{}", cli_missing_message(kind));
        }
        let agent = Agent {
            id: adopted
                .as_ref()
                .map(|e| e.agent_id.clone())
                .unwrap_or_else(AgentId::generate),
            worktree_id: worktree_id.clone(),
            name: if name.trim().is_empty() {
                "agent".into()
            } else {
                name.trim().to_string()
            },
            status: AgentStatus::Fresh,
            archived: false,
            archived_at: 0,
            pinned: false,
            kind,
            model,
            effort,
            session_id: None,
            sort_order: 0,
            status_changed_at: epoch_ms(),
            alive: false,
            branch: Some(worktree.branch.clone()),
            writer: !writer.is_off(),
        };
        self.store
            .insert_agent_with_auto_title(&agent, auto_title)?;
        if adopted.is_none() {
            // Cold path: boot the CLI right away.
            self.spawn_agent_session_with_prompt(&agent, &worktree, 80, 24, prompt.as_deref())?;
        }
        let mut broadcast_agent = agent.clone();
        broadcast_agent.alive = true;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(broadcast_agent),
        });
        if let Some(entry) = adopted {
            // Now that the row exists, replay the hooks the warm CLI fired
            // before adoption (SessionStart stores the resume session id).
            for (event, sid) in entry.buffered_hooks {
                self.apply_hook_event(&agent.id, event, sid);
            }
        }
        Ok(EntityId::Agent(agent.id))
    }

    /// The unarchived writer on this checkout whose turn can still run:
    /// a live PTY, or a status a resume picks up again. A daemon restart
    /// sweeps a mid-turn holder to disconnected, which still holds.
    fn live_writer(&self, worktree_id: &WorktreeId) -> Result<Option<String>> {
        let (_, _, agents, _) = self.store.load_tree()?;
        Ok(agents
            .into_iter()
            .find(|a| {
                a.writer
                    && !a.archived
                    && &a.worktree_id == worktree_id
                    && (self.is_alive(&SessionRef::Agent(a.id.clone()))
                        || matches!(
                            a.status,
                            AgentStatus::Running
                                | AgentStatus::NeedsFeedback
                                | AgentStatus::Finished
                                | AgentStatus::Disconnected
                        ))
            })
            .map(|a| a.name))
    }

    // ---- prewarm pool ----

    /// Pre-spawn an agent CLI for (worktree, kind) so the next create adopts
    /// an already-booted session. Fail-soft by design: a disabled config,
    /// missing CLI, or spawn error just means the create stays cold.
    pub async fn prewarm_agent(
        self: &Arc<Self>,
        worktree_id: &WorktreeId,
        kind: AgentKind,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<()> {
        let _execution = self.checkout_execution.read().await;
        if !crate::config::Config::load().prewarm_agents {
            return Ok(());
        }
        let Some(worktree) = self.store.get_worktree(worktree_id)? else {
            return Ok(());
        };
        let stale = {
            // One warm slot per key; keep a live, young one with the same
            // spec, replace a dead, wrong-spec, or aging one (recycling
            // before the reaper hits keeps a re-requested slot gap-free).
            let mut pool = self.prewarmed.lock().unwrap();
            if let Some(entry) = pool.get(&(worktree_id.clone(), kind)) {
                if self.is_alive(&SessionRef::Agent(entry.agent_id.clone()))
                    && entry.model == model
                    && entry.effort == effort
                    && entry.spawned_at.elapsed() < PREWARM_RECYCLE_AGE
                {
                    return Ok(());
                }
                pool.remove(&(worktree_id.clone(), kind))
            } else {
                None
            }
        };
        if let Some(old) = stale {
            self.kill_session(&SessionRef::Agent(old.agent_id));
        }
        if !self.cli_available(kind).await {
            tracing::debug!(kind = kind.as_str(), "prewarm skipped: CLI not installed");
            return Ok(());
        }
        let agent = Agent {
            id: AgentId::generate(),
            worktree_id: worktree_id.clone(),
            name: "prewarm".into(),
            status: AgentStatus::Fresh,
            archived: false,
            archived_at: 0,
            pinned: false,
            kind,
            model: model.clone(),
            effort: effort.clone(),
            session_id: None,
            sort_order: 0,
            status_changed_at: 0,
            alive: false,
            branch: None,
            writer: false,
        };
        self.spawn_agent_session(&agent, &worktree, 80, 24)?;
        tracing::info!(agent = %agent.id, kind = kind.as_str(), worktree = %worktree.branch, "prewarmed agent session");
        let replaced = self.prewarmed.lock().unwrap().insert(
            (worktree_id.clone(), kind),
            PrewarmEntry {
                agent_id: agent.id,
                spawned_at: Instant::now(),
                model,
                effort,
                buffered_hooks: Vec::new(),
            },
        );
        // Two racing prewarms for the same key: the loser's session would
        // otherwise leak as an orphan CLI process.
        if let Some(old) = replaced {
            self.kill_session(&SessionRef::Agent(old.agent_id));
        }
        Ok(())
    }

    /// Pop the warm entry for (worktree, kind) if its PTY is still running
    /// and it booted with the requested model/effort. A dead entry (CLI
    /// missing/crashed while warm) is dropped, a wrong-spec one is killed;
    /// either way the caller falls back to a cold spawn.
    fn take_prewarmed(
        &self,
        worktree_id: &WorktreeId,
        kind: AgentKind,
        model: &Option<String>,
        effort: &Option<String>,
    ) -> Option<PrewarmEntry> {
        let key = (worktree_id.clone(), kind);
        let entry = {
            let mut pool = self.prewarmed.lock().unwrap();
            let sref = SessionRef::Agent(pool.get(&key)?.agent_id.clone());
            if self.session(&sref).is_some_and(|s| s.was_killed()) {
                return None;
            }
            pool.remove(&key)?
        };
        if !self.is_alive(&SessionRef::Agent(entry.agent_id.clone())) {
            return None;
        }
        if entry.model != *model || entry.effort != *effort {
            self.kill_session(&SessionRef::Agent(entry.agent_id));
            return None;
        }
        Some(entry)
    }

    /// Drop warm sessions that died or sat unclaimed past the max age, or
    /// all of them once `prewarm_agents` is off (runs on the daemon's
    /// periodic tick).
    pub fn reap_prewarmed(&self) {
        self.sweep_prewarmed(crate::config::Config::load().prewarm_agents);
    }

    fn sweep_prewarmed(&self, prewarm_enabled: bool) {
        let doomed: Vec<AgentId> = {
            let mut pool = self.prewarmed.lock().unwrap();
            let expired: Vec<_> = pool
                .iter()
                .filter(|(_, e)| {
                    !prewarm_enabled
                        || e.spawned_at.elapsed() > PREWARM_MAX_AGE
                        || !self.is_alive(&SessionRef::Agent(e.agent_id.clone()))
                })
                .map(|(k, _)| k.clone())
                .collect();
            expired
                .into_iter()
                .filter_map(|k| pool.remove(&k))
                .map(|e| e.agent_id)
                .collect()
        };
        for id in doomed {
            tracing::debug!(agent = %id, "reaping prewarmed session");
            self.kill_session(&SessionRef::Agent(id));
        }
    }

    /// Kill warm sessions homed in any of these worktrees (worktree delete,
    /// project remove — their store rows are gone or going).
    fn kill_prewarmed_in(&self, worktree_ids: &[WorktreeId]) {
        let doomed: Vec<AgentId> = {
            let mut pool = self.prewarmed.lock().unwrap();
            let keys: Vec<_> = pool
                .keys()
                .filter(|(w, _)| worktree_ids.contains(w))
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| pool.remove(&k))
                .map(|e| e.agent_id)
                .collect()
        };
        for id in doomed {
            self.kill_session(&SessionRef::Agent(id));
        }
    }

    fn prewarmed_in(&self, worktree: &WorktreeId) -> Vec<(SessionRef, Arc<PtySession>)> {
        let pool = self.prewarmed.lock().unwrap();
        pool.iter()
            .filter(|((w, _), _)| w == worktree)
            .filter_map(|(_, e)| {
                let sref = SessionRef::Agent(e.agent_id.clone());
                let session = self.session(&sref)?;
                Some((sref, session))
            })
            .collect()
    }

    /// Is the kind's CLI on the user's PATH (as their login shell sees it)?
    /// Cached: hits for an hour, misses for a minute so a just-installed CLI
    /// gets picked up quickly. Probe trouble (timeout, spawn error) fails
    /// open — a doomed warm spawn is still graceful.
    async fn cli_available(&self, kind: AgentKind) -> bool {
        if self.agent_cmd_override.is_some() {
            return true; // test override is spawned verbatim
        }
        const OK_TTL: Duration = Duration::from_secs(3600);
        const FAIL_TTL: Duration = Duration::from_secs(60);
        {
            let probes = self.cli_probes.lock().unwrap();
            if let Some((ok, at)) = probes.get(&kind) {
                if at.elapsed() < if *ok { OK_TTL } else { FAIL_TTL } {
                    return *ok;
                }
            }
        }
        self.probe_cli(kind).await
    }

    /// Fill the availability cache for every kind at boot, off the request
    /// loop. Without it the first CreateAgent of a session pays a full
    /// login-shell probe (~1s with a heavy ~/.zshrc) before it can answer.
    pub async fn warm_cli_probes(self: &Arc<Self>) {
        for kind in AgentKind::ALL {
            self.cli_available(kind).await;
        }
    }

    /// Same question, asked on behalf of a create the user just triggered.
    /// A cached *hit* is trusted; a cached *miss* is re-probed, so someone who
    /// installs the CLI and immediately retries isn't told for another minute
    /// that it's missing. Misses are rare, so this costs nothing in practice.
    async fn cli_available_for_create(&self, kind: AgentKind) -> bool {
        self.cli_available(kind).await || self.probe_cli(kind).await
    }

    /// Uncached `command -v` through the user's login shell; caches the answer.
    async fn probe_cli(&self, kind: AgentKind) -> bool {
        let check = format!("command -v '{}' >/dev/null 2>&1", kind.cli_program());
        let mut probe = tokio::process::Command::new(user_shell());
        probe
            .args(["-l", "-i", "-c", &check])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // A timed-out probe must die with the dropped future, not linger.
            .kill_on_drop(true);
        // Own session: the interactive shell must not reach the daemon's
        // controlling terminal (--foreground runs have one). zsh's job-control
        // init opens /dev/tty and makes itself the foreground process group,
        // SIGTTIN-stopping whatever TUI owns that terminal.
        unsafe {
            probe.pre_exec(|| match nix::unistd::setsid() {
                Ok(_) => Ok(()),
                Err(errno) => Err(std::io::Error::from_raw_os_error(errno as i32)),
            });
        }
        let status = tokio::time::timeout(Duration::from_secs(5), probe.status()).await;
        match status {
            Ok(Ok(status)) => {
                let ok = status.success();
                self.cli_probes
                    .lock()
                    .unwrap()
                    .insert(kind, (ok, Instant::now()));
                ok
            }
            _ => true,
        }
    }

    pub fn rename_agent(self: &Arc<Self>, id: &AgentId, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            bail!("name is empty");
        }
        self.store.rename_agent(id, name.trim())?;
        let agent = self.agent_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(agent),
        });
        Ok(())
    }

    /// Agent-initiated one-shot title (`nebula rename` inside the session's
    /// CLI). Applies only while the auto-title is still pending; afterwards
    /// it reports the standing title as an error so the CLI (and the model
    /// reading its output) knows nothing changed.
    pub fn auto_rename_agent(self: &Arc<Self>, id: &AgentId, name: &str) -> Result<()> {
        let title = sanitize_title(name);
        if title.is_empty() {
            bail!("title is empty");
        }
        let agent = self.store.get_agent(id)?.context("agent not found")?;
        if !self.store.rename_agent_if_auto_pending(id, &title)? {
            bail!(
                "session already has a title ({:?}); leaving it unchanged — a user-set \
                 title is only replaced with `nebula rename --force`",
                agent.name
            );
        }
        let agent = self.agent_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(agent),
        });
        Ok(())
    }

    /// Re-home an agent row under another worktree of the same project. A
    /// live PTY still runs — and its hooks still report a cwd — inside the
    /// old checkout, so left alone `reparent_agent_by_cwd` would snap the
    /// row straight back on the next hook event: kill it and respawn resumed
    /// in the target so the process and the row agree. A respawn failure
    /// degrades to a dead session the next attach/prewarm revives via
    /// `ensure_session`.
    pub fn move_agent(self: &Arc<Self>, id: &AgentId, worktree_id: &WorktreeId) -> Result<()> {
        let _execution = self
            .checkout_execution
            .try_read()
            .map_err(|_| anyhow::anyhow!("checkout operation in progress; retry session action"))?;
        let agent = self.store.get_agent(id)?.context("agent not found")?;
        if &agent.worktree_id == worktree_id {
            return Ok(());
        }
        let target = self
            .store
            .get_worktree(worktree_id)?
            .context("worktree not found")?;
        let current = self
            .store
            .get_worktree(&agent.worktree_id)?
            .context("worktree not found")?;
        if target.project_id != current.project_id {
            bail!("target worktree belongs to a different project");
        }
        self.validate_execution_branch(&target, Some(&target.branch))?;
        let sref = SessionRef::Agent(id.clone());
        let was_alive = self.session(&sref).is_some();
        if was_alive {
            self.kill_session(&sref);
        }
        // A deliberate move invalidates the remembered hook cwd: it still
        // points at the old checkout, and the next worktree sync would
        // replay it straight back over the user's choice.
        self.last_cwd.lock().unwrap().remove(id);
        self.store.set_agent_worktree(id, worktree_id)?;
        self.store.set_agent_branch(id, Some(&target.branch))?;
        if was_alive {
            let agent = self.store.get_agent(id)?.context("agent not found")?;
            if let Err(e) = self.spawn_agent_session(&agent, &target, 80, 24) {
                tracing::warn!(agent = %id, error = %e, "respawn after move failed");
            }
        }
        let agent = self.agent_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(agent),
        });
        Ok(())
    }

    /// Row-only re-home: store update plus broadcast, never the PTY. The
    /// hook-cwd reparent uses this — there the process already runs in the
    /// target checkout and only the row is stale, so killing it would
    /// interrupt a live conversation for nothing.
    fn move_agent_row(self: &Arc<Self>, id: &AgentId, worktree_id: &WorktreeId) -> Result<()> {
        let _execution = self
            .checkout_execution
            .try_read()
            .map_err(|_| anyhow::anyhow!("checkout operation in progress; retry session action"))?;
        self.store.set_agent_worktree(id, worktree_id)?;
        // The process moved into another checkout: its chat now belongs
        // to that checkout's branch.
        if let Some(target) = self.store.get_worktree(worktree_id)? {
            self.store.set_agent_branch(id, Some(&target.branch))?;
        }
        let agent = self.agent_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(agent),
        });
        Ok(())
    }

    /// Re-home every live shell tab under the worktree its shell process
    /// currently sits in. Terminals have no hooks to report a cwd, so the
    /// daemon reads the shell's working directory straight from the kernel
    /// each sync tick: a `cd` into another checkout (by hand, or via
    /// `nebula switch`) moves the tab under that row in the panel, and a
    /// `cd` back moves it home. Directories outside every worktree of the
    /// project leave the row where it is. Fail-soft per terminal.
    pub fn sync_terminal_cwds(self: &Arc<Self>) {
        let live: Vec<(TerminalId, u32)> = {
            let sessions = self.sessions.lock().unwrap();
            sessions
                .iter()
                .filter_map(|(sref, s)| match sref {
                    SessionRef::Terminal(id) => s.child_pid.map(|pid| (id.clone(), pid)),
                    SessionRef::Agent(_) => None,
                })
                .collect()
        };
        for (id, pid) in live {
            let Some(cwd) = process_cwd(pid) else {
                continue;
            };
            if let Err(e) = self.reparent_terminal_to_cwd(&id, &canonical_or_raw(&cwd)) {
                tracing::warn!(terminal = %id, error = %e, "terminal cwd reparent failed");
            }
        }
    }

    /// Move a shell tab's row under the worktree owning `cwd` when that is
    /// a different worktree of the same project. `cwd` must already be
    /// canonicalized. The PTY is untouched — only the row moves.
    fn reparent_terminal_to_cwd(self: &Arc<Self>, id: &TerminalId, cwd: &Path) -> Result<()> {
        let _execution = self
            .checkout_execution
            .try_read()
            .map_err(|_| anyhow::anyhow!("checkout operation in progress; retry session action"))?;
        let Some(terminal) = self.store.get_terminal(id)? else {
            return Ok(());
        };
        let Some(current) = self.store.get_worktree(&terminal.worktree_id)? else {
            return Ok(());
        };
        let Some(target) = self.worktree_owning(&current.project_id, cwd)? else {
            return Ok(());
        };
        if target.id == terminal.worktree_id {
            return Ok(());
        }
        tracing::info!(
            terminal = %id,
            from = %current.branch,
            to = %target.branch,
            "terminal re-homed by shell cwd"
        );
        self.store.set_terminal_worktree(id, &target.id)?;
        self.store.set_terminal_branch(id, Some(&target.branch))?;
        let entity = self.terminal_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Terminal(entity),
        });
        Ok(())
    }

    /// Deepest worktree of `project` whose path contains `cwd` — nested
    /// layouts (checkouts under the repo root) must not resolve to the
    /// root row just because the root path is also a prefix.
    fn worktree_owning(&self, project_id: &ProjectId, cwd: &Path) -> Result<Option<Worktree>> {
        let (_, worktrees, _, _) = self.store.load_tree()?;
        Ok(worktrees
            .into_iter()
            .filter(|w| w.project_id == *project_id)
            .map(|w| {
                let canonical = canonical_or_raw(&w.path);
                (w, canonical)
            })
            .filter(|(_, canonical)| cwd.starts_with(canonical))
            .max_by_key(|(_, canonical)| canonical.components().count())
            .map(|(w, _)| w))
    }

    /// A hook payload reported the agent CLI's working directory. When that
    /// directory sits inside a *different* worktree of the same project (the
    /// session entered a worktree it created mid-conversation), re-home the
    /// agent row so the tree reflects where the work actually happens.
    /// Fail-soft: any error leaves the row where it is.
    pub fn reparent_agent_by_cwd(
        self: &Arc<Self>,
        agent_id: &AgentId,
        cwd: &str,
        payload_session_id: Option<&str>,
        captures_session: bool,
    ) {
        if let Err(e) =
            self.try_reparent_agent_by_cwd(agent_id, cwd, payload_session_id, captures_session)
        {
            tracing::warn!(agent = %agent_id, error = %e, "cwd reparent failed");
        }
    }

    fn try_reparent_agent_by_cwd(
        self: &Arc<Self>,
        agent_id: &AgentId,
        cwd: &str,
        payload_session_id: Option<&str>,
        captures_session: bool,
    ) -> Result<()> {
        let Some(agent) = self.store.get_agent(agent_id)? else {
            self.last_cwd.lock().unwrap().remove(agent_id);
            return Ok(());
        };
        if agent.archived {
            self.last_cwd.lock().unwrap().remove(agent_id);
            return Ok(());
        }
        // Same foreign-session rule as the status machine: a payload from a
        // different CLI session only counts when the event (re)establishes
        // session ownership (UserPromptSubmit / SessionStart).
        if !captures_session {
            if let (Some(mine), Some(theirs)) = (agent.session_id.as_deref(), payload_session_id) {
                if mine != theirs {
                    return Ok(());
                }
            }
        }
        let cwd = canonical_or_raw(Path::new(cwd));
        // Remembered even when it resolves to nothing: an agent that just ran
        // `git worktree add` and stepped into the result reports a cwd nebula
        // has no row for yet, and the worktree sync replays this to finish the
        // re-home the moment that row is adopted.
        self.last_cwd
            .lock()
            .unwrap()
            .insert(agent_id.clone(), cwd.clone());
        self.reparent_agent_to_cwd(&agent, &cwd)
    }

    /// Move `agent`'s row under the worktree owning `cwd` when that is a
    /// different worktree of the same project. `cwd` must already be
    /// canonicalized.
    fn reparent_agent_to_cwd(self: &Arc<Self>, agent: &Agent, cwd: &Path) -> Result<()> {
        let Some(current) = self.store.get_worktree(&agent.worktree_id)? else {
            return Ok(());
        };
        let target = self.worktree_owning(&current.project_id, cwd)?;
        if let Some(worktree) = target {
            if worktree.id != agent.worktree_id {
                tracing::info!(
                    agent = %agent.id,
                    from = %current.branch,
                    to = %worktree.branch,
                    "agent re-homed by hook cwd"
                );
                self.move_agent_row(&agent.id, &worktree.id)?;
            }
        }
        Ok(())
    }

    /// Replay remembered hook cwds for `project`'s agents. Runs after the
    /// worktree sync adopts checkouts: a session that creates a worktree and
    /// enters it reports the new cwd (often on the very next `Stop`) before
    /// the row exists, and without this replay its row would sit under the
    /// old checkout until the user's next prompt.
    fn reparent_agents_by_last_cwd(self: &Arc<Self>, project: &Project) {
        let known: Vec<(AgentId, PathBuf)> = {
            let map = self.last_cwd.lock().unwrap();
            map.iter().map(|(id, p)| (id.clone(), p.clone())).collect()
        };
        for (agent_id, cwd) in known {
            let agent = match self.store.get_agent(&agent_id) {
                Ok(Some(agent)) => agent,
                Ok(None) => {
                    self.last_cwd.lock().unwrap().remove(&agent_id);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(agent = %agent_id, error = %e, "cwd replay lookup failed");
                    continue;
                }
            };
            if agent.archived {
                continue;
            }
            let in_project = matches!(
                self.store.get_worktree(&agent.worktree_id),
                Ok(Some(w)) if w.project_id == project.id
            );
            if !in_project {
                continue;
            }
            if let Err(e) = self.reparent_agent_to_cwd(&agent, &cwd) {
                tracing::warn!(agent = %agent_id, error = %e, "cwd replay reparent failed");
            }
        }
    }

    pub fn archive_agent(self: &Arc<Self>, id: &AgentId) -> Result<()> {
        self.kill_session(&SessionRef::Agent(id.clone()));
        self.store.set_agent_archived(id, true)?;
        let agent = self.agent_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(agent),
        });
        Ok(())
    }

    /// Archive agents left finished, fresh or disconnected longer than
    /// `auto_archive_after` (runs on the daemon's periodic tick).
    pub fn auto_archive_stale(self: &Arc<Self>) {
        self.sweep_stale_agents(crate::config::Config::load().auto_archive_after());
    }

    fn sweep_stale_agents(self: &Arc<Self>, after: Option<Duration>) {
        let Some(after) = after else {
            return;
        };
        let cutoff = epoch_ms() - after.as_millis() as i64;
        let stale = match self.store.stale_idle_agent_ids(cutoff) {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "auto-archive query failed");
                return;
            }
        };
        let attached: HashSet<SessionRef> =
            self.attach_counts.lock().unwrap().keys().cloned().collect();
        for id in stale {
            if attached.contains(&SessionRef::Agent(id.clone())) {
                continue;
            }
            tracing::info!(agent = %id, "auto-archiving stale agent");
            if let Err(e) = self.archive_agent(&id) {
                tracing::warn!(agent = %id, error = %e, "auto-archive failed");
            }
        }
    }

    pub fn unarchive_agent(self: &Arc<Self>, id: &AgentId) -> Result<()> {
        self.store.set_agent_archived(id, false)?;
        let agent = self.agent_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(agent),
        });
        Ok(())
    }

    pub fn set_agent_pinned(self: &Arc<Self>, id: &AgentId, pinned: bool) -> Result<()> {
        self.store.set_agent_pinned(id, pinned)?;
        let agent = self.agent_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(agent),
        });
        Ok(())
    }

    /// Require a non-archived agent belonging to the explicitly selected project.
    pub fn validate_agent_project(&self, id: &AgentId, project: &ProjectId) -> Result<()> {
        let agent = self
            .store
            .get_agent(id)?
            .context("Session no longer available")?;
        let w = self
            .store
            .get_worktree(&agent.worktree_id)?
            .context("Checkout unavailable")?;
        if agent.archived || w.project_id != *project {
            bail!("Session no longer available in this project");
        }
        Ok(())
    }

    pub fn delete_agent(self: &Arc<Self>, id: &AgentId) -> Result<()> {
        let sref = SessionRef::Agent(id.clone());
        self.kill_session(&sref);
        self.last_cwd.lock().unwrap().remove(id);
        self.store.delete_agent(id)?;
        Self::forget_screen(&sref);
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Agent(id.clone()),
        });
        Ok(())
    }

    pub fn restart_agent(self: &Arc<Self>, id: &AgentId) -> Result<()> {
        let _execution = self
            .checkout_execution
            .try_read()
            .map_err(|_| anyhow::anyhow!("checkout operation in progress; retry session action"))?;
        let agent = self.store.get_agent(id)?.context("agent not found")?;
        if agent.archived {
            bail!("agent is archived — unarchive it first");
        }
        let worktree = self
            .store
            .get_worktree(&agent.worktree_id)?
            .context("worktree not found")?;
        self.validate_execution_branch(&worktree, agent.branch.as_deref())?;
        self.kill_session(&SessionRef::Agent(id.clone()));
        self.spawn_agent_session(&agent, &worktree, 80, 24)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Agent(self.agent_entity(id)?),
        });
        Ok(())
    }

    // ---- terminals ----

    /// Spawn a shell in this checkout; branch intents are checked before row creation.
    pub fn create_terminal(
        self: &Arc<Self>,
        worktree_id: &WorktreeId,
        expected_branch: Option<&str>,
        name: Option<String>,
    ) -> Result<EntityId> {
        let _execution = self
            .checkout_execution
            .try_read()
            .map_err(|_| anyhow::anyhow!("checkout operation in progress; retry session action"))?;
        let worktree = self
            .store
            .get_worktree(worktree_id)?
            .context("worktree not found")?;
        self.validate_execution_branch(&worktree, expected_branch.or(Some(&worktree.branch)))?;
        let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            let n = self.store.count_terminals(worktree_id).unwrap_or(0);
            format!("term-{}", n + 1)
        });
        let terminal = TerminalTab {
            id: TerminalId::generate(),
            worktree_id: worktree_id.clone(),
            name,
            sort_order: 0,
            alive: false,
            busy: false,
            status: None,
            status_changed_at: 0,
            branch: Some(worktree.branch.clone()),
        };
        self.store.insert_terminal(&terminal)?;
        self.spawn_terminal_session(&terminal, &worktree, 80, 24)?;
        let mut broadcast_term = terminal.clone();
        broadcast_term.alive = true;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Terminal(broadcast_term),
        });
        Ok(EntityId::Terminal(terminal.id))
    }

    pub fn rename_terminal(self: &Arc<Self>, id: &TerminalId, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            bail!("name is empty");
        }
        self.store.rename_terminal(id, name.trim())?;
        let term = self.terminal_entity(id)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Terminal(term),
        });
        Ok(())
    }

    pub fn close_terminal(self: &Arc<Self>, id: &TerminalId) -> Result<()> {
        let sref = SessionRef::Terminal(id.clone());
        self.kill_session(&sref);
        self.store.delete_terminal(id)?;
        Self::forget_screen(&sref);
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Terminal(id.clone()),
        });
        Ok(())
    }

    // ---- notes ----

    pub fn create_note(self: &Arc<Self>, owner: &NoteOwner, text: &str) -> Result<EntityId> {
        let text = text.trim();
        if text.is_empty() {
            bail!("note text is empty");
        }
        match owner {
            NoteOwner::Project(id) => {
                self.store.get_project(id)?.context("project not found")?;
            }
            NoteOwner::Worktree(id) => {
                self.store.get_worktree(id)?.context("worktree not found")?;
            }
            NoteOwner::Todo(id) => {
                self.store.get_todo(id)?.context("todo not found")?;
            }
        }
        let note = Note {
            id: NoteId::generate(),
            owner: owner.clone(),
            text: text.to_string(),
            done: false,
            sort_order: self.store.next_note_sort_order(owner)?,
        };
        self.store.insert_note(&note)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Note(note.clone()),
        });
        Ok(EntityId::Note(note.id))
    }

    pub fn update_note(self: &Arc<Self>, id: &NoteId, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            bail!("note text is empty");
        }
        self.store.set_note_text(id, text)?;
        let note = self.store.get_note(id)?.context("note not found")?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Note(note),
        });
        Ok(())
    }

    pub fn set_note_done(self: &Arc<Self>, id: &NoteId, done: bool) -> Result<()> {
        self.store.set_note_done(id, done)?;
        let note = self.store.get_note(id)?.context("note not found")?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Note(note),
        });
        Ok(())
    }

    pub fn delete_note(self: &Arc<Self>, id: &NoteId) -> Result<()> {
        self.store.delete_note(id)?;
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Note(id.clone()),
        });
        Ok(())
    }

    // ---- todos ----

    pub fn create_todo(self: &Arc<Self>, owner: &TodoOwner, text: &str) -> Result<EntityId> {
        let text = text.trim();
        if text.is_empty() {
            bail!("todo text is empty");
        }
        match owner {
            TodoOwner::Project(id) => {
                self.store.get_project(id)?.context("project not found")?;
            }
            TodoOwner::Worktree(id) => {
                self.store.get_worktree(id)?.context("worktree not found")?;
            }
        }
        let todo = Todo {
            id: TodoId::generate(),
            owner: owner.clone(),
            text: text.to_string(),
            done: false,
            sort_order: self.store.next_todo_sort_order(owner)?,
        };
        self.store.insert_todo(&todo)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Todo(todo.clone()),
        });
        Ok(EntityId::Todo(todo.id))
    }

    pub fn update_todo(self: &Arc<Self>, id: &TodoId, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            bail!("todo text is empty");
        }
        self.store.set_todo_text(id, text)?;
        let todo = self.store.get_todo(id)?.context("todo not found")?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Todo(todo),
        });
        Ok(())
    }

    pub fn set_todo_done(self: &Arc<Self>, id: &TodoId, done: bool) -> Result<()> {
        self.store.set_todo_done(id, done)?;
        let todo = self.store.get_todo(id)?.context("todo not found")?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Todo(todo),
        });
        Ok(())
    }

    /// Child notes cascade away in the store; clients mirror that pruning
    /// off this one removal event, so none is broadcast per note.
    pub fn delete_todo(self: &Arc<Self>, id: &TodoId) -> Result<()> {
        self.store.delete_todo(id)?;
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Todo(id.clone()),
        });
        Ok(())
    }

    // ---- links ----

    pub fn create_link(self: &Arc<Self>, worktree_id: &WorktreeId, url: &str) -> Result<EntityId> {
        let url = normalize_url(url)?;
        self.store
            .get_worktree(worktree_id)?
            .context("worktree not found")?;
        let link = Link {
            id: LinkId::generate(),
            worktree_id: worktree_id.clone(),
            url,
            sort_order: self.store.next_link_sort_order(worktree_id)?,
        };
        self.store.insert_link(&link)?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Link(link.clone()),
        });
        Ok(EntityId::Link(link.id))
    }

    pub fn update_link(self: &Arc<Self>, id: &LinkId, url: &str) -> Result<()> {
        let url = normalize_url(url)?;
        self.store.set_link_url(id, &url)?;
        let link = self.store.get_link(id)?.context("link not found")?;
        self.broadcast(ServerEvent::EntityUpserted {
            entity: Entity::Link(link),
        });
        Ok(())
    }

    pub fn delete_link(self: &Arc<Self>, id: &LinkId) -> Result<()> {
        self.store.delete_link(id)?;
        self.broadcast(ServerEvent::EntityRemoved {
            id: EntityId::Link(id.clone()),
        });
        Ok(())
    }

    // ---- attach / spawn ----

    /// Get the live session for an entity, lazily (re)spawning its PTY when
    /// none is running (restored agents, closed shells).
    pub fn ensure_session(
        self: &Arc<Self>,
        sref: &SessionRef,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<PtySession>> {
        if let Some(s) = self.session(sref) {
            return Ok(s);
        }
        let _execution = self
            .checkout_execution
            .try_read()
            .map_err(|_| anyhow::anyhow!("checkout operation in progress; retry resume"))?;
        match sref {
            SessionRef::Agent(id) => {
                let agent = self.store.get_agent(id)?.context("agent not found")?;
                if agent.archived {
                    bail!("agent is archived — unarchive it first");
                }
                let worktree = self
                    .store
                    .get_worktree(&agent.worktree_id)?
                    .context("worktree not found")?;
                let session = self.spawn_agent_session(&agent, &worktree, cols, rows)?;
                self.broadcast(ServerEvent::EntityUpserted {
                    entity: Entity::Agent(self.agent_entity(id)?),
                });
                Ok(session)
            }
            SessionRef::Terminal(id) => {
                let term = self.store.get_terminal(id)?.context("terminal not found")?;
                let worktree = self
                    .store
                    .get_worktree(&term.worktree_id)?
                    .context("worktree not found")?;
                let session = self.spawn_terminal_session(&term, &worktree, cols, rows)?;
                let mut broadcast_term = term;
                broadcast_term.alive = true;
                self.broadcast(ServerEvent::EntityUpserted {
                    entity: Entity::Terminal(broadcast_term),
                });
                Ok(session)
            }
        }
    }

    fn spawn_agent_session(
        self: &Arc<Self>,
        agent: &Agent,
        worktree: &Worktree,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<PtySession>> {
        self.spawn_agent_session_with_prompt(agent, worktree, cols, rows, None)
    }

    /// `initial_prompt` rides the CLI's positional prompt argument, so it
    /// only applies to a fresh spawn — respawns resume by session id and
    /// never repeat it (`agent_spawn_command` drops it when resuming).
    fn spawn_agent_session_with_prompt(
        self: &Arc<Self>,
        agent: &Agent,
        worktree: &Worktree,
        cols: u16,
        rows: u16,
        initial_prompt: Option<&str>,
    ) -> Result<Arc<PtySession>> {
        self.validate_execution_branch(worktree, agent.branch.as_deref())?;
        // Managed status hooks; a failure here degrades to "no status
        // updates", never blocks the spawn.
        if let Err(e) = hooks::installer::install_for_kind(agent.kind, &worktree.path) {
            tracing::warn!(error = %e, cwd = %worktree.path.display(), "hook install failed");
        }

        let cmd_override = self.agent_cmd_override.as_deref();
        if cmd_override.is_none() {
            if let Err(e) = hooks::installer::trust_for_kind(agent.kind, &worktree.path) {
                tracing::warn!(error = %e, cwd = %worktree.path.display(), "trust pre-accept failed");
            }
        }
        let (program, args, resumed) = agent_spawn_command(
            agent.kind,
            agent.session_id.as_deref(),
            agent.model.as_deref(),
            agent.effort.as_deref(),
            cmd_override,
            initial_prompt,
        );
        let hook_env = self.hook_env_pairs(&agent.id.to_string());
        // Run the agent through the user's login+interactive shell so it sees
        // the same env as a Terminal.app tab (~/.zprofile, ~/.zshrc,
        // path_helper) instead of the daemon's inherited-at-boot env.
        // Overrides (tests) stay verbatim.
        let (program, args) = if cmd_override.is_some() {
            (program, args)
        } else {
            login_shell_wrap(&user_shell(), &program, &args)
        };

        let spec = SpawnSpec {
            program,
            args,
            cwd: worktree.path.clone(),
            env: hook_env,
            scrub_env: scrubbed_env_names(),
            cols,
            rows,
        };
        let sref = SessionRef::Agent(agent.id.clone());
        let session = PtySession::spawn(sref, spec)?;
        self.install_session(session.clone());
        if resumed {
            self.arm_resume_fallback(agent.clone(), worktree.clone(), session.clone(), cols, rows);
        }
        Ok(session)
    }

    /// A resumed session (`claude --resume` / `codex resume` /
    /// `cursor-agent --resume`) dies fast when
    /// it is stale/deleted — fall back to a fresh session instead of leaving
    /// a dead pane.
    fn arm_resume_fallback(
        self: &Arc<Self>,
        agent: Agent,
        worktree: Worktree,
        session: Arc<PtySession>,
        cols: u16,
        rows: u16,
    ) {
        let daemon = self.clone();
        let mut rx = session.events.subscribe();
        tokio::spawn(async move {
            let early_exit = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    match rx.recv().await {
                        Ok(PtyEvent::Exited { exit_code }) => return exit_code.unwrap_or(1) != 0,
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return false,
                    }
                }
            })
            .await;
            if early_exit != Ok(true) || session.was_killed() {
                return;
            }
            match daemon.store.get_agent(&agent.id) {
                Ok(Some(current)) if !current.archived => {}
                _ => return,
            }
            let Ok(_execution) = daemon.checkout_execution.try_read() else {
                return;
            };
            if daemon
                .validate_execution_branch(&worktree, agent.branch.as_deref())
                .is_err()
            {
                return;
            }
            tracing::info!(agent = %agent.id, "resume failed fast — respawning fresh");
            let _ = daemon.store.set_agent_session_id(&agent.id, None);
            let mut fresh = agent.clone();
            fresh.session_id = None;
            if daemon
                .spawn_agent_session(&fresh, &worktree, cols, rows)
                .is_ok()
            {
                if let Ok(agent) = daemon.agent_entity(&fresh.id) {
                    daemon.broadcast(ServerEvent::EntityUpserted {
                        entity: Entity::Agent(agent),
                    });
                }
            }
        });
    }

    fn spawn_terminal_session(
        self: &Arc<Self>,
        terminal: &TerminalTab,
        worktree: &Worktree,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<PtySession>> {
        self.validate_execution_branch(worktree, terminal.branch.as_deref())?;
        // `-l` makes it a login shell, matching Terminal.app: zsh then sources
        // /etc/zprofile (path_helper), ~/.zprofile, and ~/.zshrc.
        // The hook env carries a `term:`-prefixed id: an agent CLI run by
        // hand inside this shell reports through the same globally-installed
        // hooks, and the daemon routes those onto the terminal's status.
        let hook_env =
            self.hook_env_pairs(&format!("{TERMINAL_HOOK_PREFIX}{}", terminal.id.as_str()));
        let spec = SpawnSpec {
            program: user_shell(),
            args: vec!["-l".into()],
            cwd: worktree.path.clone(),
            env: hook_env,
            scrub_env: scrubbed_env_names(),
            cols,
            rows,
        };
        let sref = SessionRef::Terminal(terminal.id.clone());
        let session = PtySession::spawn(sref, spec)?;
        self.install_session(session.clone());
        Ok(session)
    }

    /// The hook env every nebula-spawned PTY gets: who it is and where to
    /// report. A remote spawn ships the same three across ssh.
    fn hook_env_pairs(&self, agent_id: &str) -> Vec<(String, String)> {
        vec![
            ("NEBULA_AGENT_ID".into(), agent_id.to_string()),
            (
                "NEBULA_API_URL".into(),
                format!("http://127.0.0.1:{}", self.hook_env.port),
            ),
            ("NEBULA_API_TOKEN".into(), self.hook_env.token.clone()),
        ]
    }

    fn install_session(self: &Arc<Self>, session: Arc<PtySession>) {
        if let SessionRef::Agent(id) = &session.sref {
            if matches!(self.store.get_agent(id), Ok(Some(_))) {
                self.apply_hook_event(id, HookEvent::PtySpawned, None);
            }
        }
        self.touch_session(&session.sref);
        self.sessions
            .lock()
            .unwrap()
            .insert(session.sref.clone(), session.clone());
        self.watch_for_exit(session);
    }

    /// Once the child dies: drop it from the registry, feed the status
    /// machine (agents), and tell subscribers the entity is no longer alive.
    fn watch_for_exit(self: &Arc<Self>, session: Arc<PtySession>) {
        let daemon = self.clone();
        let mut rx = session.events.subscribe();
        let sref = session.sref.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(PtyEvent::Exited { exit_code }) => {
                        if session.was_killed() {
                            daemon.wait_for_checkout_switch().await;
                        }
                        if !daemon.unregister_if_current(&sref, &session) {
                            break;
                        }
                        tracing::info!(session = ?sref, exit_code, "session exited");
                        daemon.save_screen(&session);
                        daemon.publish_session_end(&sref, SessionEnd::Exited(exit_code));
                        break;
                    }
                    // The CLI's own busy/idle bit, read off its output. It is
                    // the only end-of-turn news after a user cancel: Claude
                    // Code fires no Stop for an interrupted turn, and
                    // suppresses the idle notification because the user just
                    // pressed a key. See `pty::progress`.
                    Ok(PtyEvent::Progress { busy }) => match &sref {
                        SessionRef::Agent(id) => {
                            daemon.apply_hook_event(id, HookEvent::Progress { busy }, None);
                        }
                        // A shell tab running an agent CLI by hand: surface
                        // its busy/idle bit as a terminal upsert so the tab
                        // lights up like an agent's status dot.
                        SessionRef::Terminal(id) => {
                            if let Ok(term) = daemon.terminal_entity(id) {
                                daemon.broadcast(ServerEvent::EntityUpserted {
                                    entity: Entity::Terminal(term),
                                });
                            }
                        }
                    },
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // A fire-hosing child can push progress edges off the
                        // broadcast queue. The scanner itself never lags, so
                        // reconcile from its current reading rather than
                        // leaving the status stuck on a dropped edge.
                        match (&sref, session.progress_busy()) {
                            (SessionRef::Agent(id), Some(busy)) => {
                                daemon.apply_hook_event(id, HookEvent::Progress { busy }, None);
                            }
                            (SessionRef::Terminal(id), Some(_)) => {
                                if let Ok(term) = daemon.terminal_entity(id) {
                                    daemon.broadcast(ServerEvent::EntityUpserted {
                                        entity: Entity::Terminal(term),
                                    });
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

/// Per-criterion verdict lines of a safe removal or prune; `refusals` holds
/// the reasons of the ones that refused.
#[derive(Debug, Default)]
pub struct RemovalReport {
    pub lines: Vec<String>,
    pub refusals: Vec<String>,
}

impl RemovalReport {
    fn push(&mut self, criterion: &str, verdict: git::Verdict) {
        match verdict {
            git::Verdict::Pass(detail) => self.lines.push(format!("{criterion}: ok — {detail}")),
            git::Verdict::Refuse(reason) => {
                self.lines.push(format!("{criterion}: REFUSE — {reason}"));
                self.refusals.push(reason);
            }
        }
    }
}

/// Program + args for an agent PTY. An override (tests) is used verbatim —
/// no resume args. Otherwise the kind picks the CLI and its resume shape:
/// `claude --resume <sid>` and `cursor-agent --resume <sid>` (flag),
/// `pi --session <sid>` (flag, takes a full or partial session id) vs
/// `codex resume <sid>` (subcommand, so resume args must lead). Claude,
/// codex and cursor always get their skip-permissions flag
/// (`--dangerously-skip-permissions` / `--yolo` / `--force`), appended
/// after the resume args — yolo mode for everything, same as Herdr.
/// Pi needs none: it has no permission prompts. Model/effort choices trail
/// everything: `claude --model m --effort e`, `codex -m m -c
/// model_reasoning_effort=e`, `pi --model m --thinking e` (pi's "effort" is
/// its thinking level); cursor has neither knob.
fn agent_spawn_command(
    kind: AgentKind,
    session_id: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
    cmd_override: Option<&str>,
    initial_prompt: Option<&str>,
) -> (String, Vec<String>, bool) {
    if let Some(cmd) = cmd_override {
        let mut parts = cmd.split_whitespace().map(String::from).collect::<Vec<_>>();
        if parts.is_empty() {
            parts.push(kind.cli_program().into());
        }
        let program = parts.remove(0);
        return (program, parts, false);
    }
    let program = kind.cli_program().to_string();
    let (mut args, resumed) = match (kind, session_id) {
        (AgentKind::Claude, Some(sid)) => (vec!["--resume".to_string(), sid.to_string()], true),
        (AgentKind::Codex, Some(sid)) => (vec!["resume".to_string(), sid.to_string()], true),
        (AgentKind::Cursor, Some(sid)) => (vec!["--resume".to_string(), sid.to_string()], true),
        (AgentKind::Pi, Some(sid)) => (vec!["--session".to_string(), sid.to_string()], true),
        (_, None) => (Vec::new(), false),
    };
    match kind {
        AgentKind::Claude => args.push("--dangerously-skip-permissions".to_string()),
        AgentKind::Codex => args.push("--yolo".to_string()),
        AgentKind::Cursor => args.push("--force".to_string()),
        AgentKind::Pi => {}
    }
    match kind {
        AgentKind::Claude => {
            if let Some(m) = model {
                args.extend(["--model".to_string(), m.to_string()]);
            }
            if let Some(e) = effort {
                args.extend(["--effort".to_string(), e.to_string()]);
            }
        }
        AgentKind::Codex => {
            if let Some(m) = model {
                args.extend(["--model".to_string(), m.to_string()]);
            }
            if let Some(e) = effort {
                args.extend(["-c".to_string(), format!("model_reasoning_effort={e}")]);
            }
        }
        AgentKind::Cursor => {}
        AgentKind::Pi => {
            if let Some(m) = model {
                args.extend(["--model".to_string(), m.to_string()]);
            }
            if let Some(e) = effort {
                args.extend(["--thinking".to_string(), e.to_string()]);
            }
        }
    }
    // All the CLIs take the initial task as a positional argument. Only
    // on a fresh spawn — a resume continues the old conversation and must
    // not re-submit the prompt.
    if let Some(p) = initial_prompt.filter(|_| !resumed) {
        args.push(p.to_string());
    }
    (program, args, resumed)
}

/// Normalize an agent-supplied title: control characters become spaces,
/// whitespace collapses, and over-long titles are cut — models occasionally
/// hand over a whole sentence no matter what the instruction says.
fn sanitize_title(raw: &str) -> String {
    const MAX_CHARS: usize = 60;
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut title = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() > MAX_CHARS {
        title = title.chars().take(MAX_CHARS).collect();
        title.truncate(title.trim_end().len());
    }
    title
}

/// Canonicalize for path containment tests, falling back to the raw path
/// when it doesn't resolve (deleted checkout, not-yet-created dir). macOS
/// symlinks (`/tmp` → `/private/tmp`) otherwise break `starts_with`.
/// Working directory of a live process, read from the kernel: the shell
/// tab re-homing signal. `None` when the process is gone or unreadable.
pub fn process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
    #[cfg(target_os = "macos")]
    {
        use nix::libc;
        // SAFETY: proc_vnodepathinfo is plain data; proc_pidinfo fills at
        // most `size` bytes of it and reports how many it wrote.
        unsafe {
            let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
            let n = libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            );
            if n <= 0 {
                return None;
            }
            let raw = std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast());
            let path = raw.to_str().ok()?;
            (!path.is_empty()).then(|| std::path::PathBuf::from(path))
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

fn canonical_or_raw(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// What `Daemon::with_overrides` changes from `Daemon::new`.
pub struct DaemonOverrides {
    /// How long a primary switch waits, in total, for the PTY children it stopped to exit.
    pub stop_deadline: Duration,
    /// Spawned, split on whitespace, in place of every agent CLI.
    pub agent_cmd: Option<String>,
}

impl Default for DaemonOverrides {
    fn default() -> Self {
        Self {
            stop_deadline: crate::pty::STOP_DEADLINE,
            agent_cmd: None,
        }
    }
}

struct Stop {
    sref: SessionRef,
    label: String,
    session: Arc<PtySession>,
    kind: StopKind,
}

enum StopKind {
    Park,
    Warm,
}

fn child_running(session: &PtySession) -> bool {
    session.child_pid.is_some_and(|pid| {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None)
            != Err(nix::errno::Errno::ESRCH)
    })
}

enum SessionEnd {
    Exited(Option<i32>),
    Parked,
}

struct Occupant {
    sref: SessionRef,
    name: String,
    status: Option<AgentStatus>,
}

impl Occupant {
    fn describe(&self) -> String {
        match self.status {
            Some(status) => format!(
                "{} (agent, {})",
                self.name,
                status.as_str().replace('_', " ")
            ),
            None => format!("{} (terminal)", self.name),
        }
    }
}

/// Does this terminal's shell have any child processes (a command or job
/// still running)? An unknown child pid or a failed probe counts as busy —
/// never kill what can't be inspected.
fn shell_has_children(session: &PtySession) -> bool {
    let Some(pid) = session.child_pid else {
        return true;
    };
    !matches!(
        std::process::Command::new("pgrep")
            .arg("-P")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
        Ok(status) if !status.success()
    )
}

/// Is this agent CLI running a command, a background one included? Agent
/// CLIs run every command as a `<shell> -c` child (Claude's Bash and Monitor
/// tools, Codex's exec), while their permanent helpers — stdio MCP servers,
/// caffeinate — are never shells, so only a shell child counts. An unknown
/// child pid or a failed probe counts as busy, as in `shell_has_children`.
fn agent_has_command_child(session: &PtySession) -> bool {
    has_command_child(session.child_pid, command_parents().as_ref())
}

fn has_command_child(pid: Option<u32>, command_parents: Option<&HashSet<u32>>) -> bool {
    match (pid, command_parents) {
        (Some(pid), Some(parents)) => parents.contains(&pid),
        _ => true,
    }
}

/// One `ps` scan: the pids that have a background command child, or `None`
/// when the probe failed.
fn command_parents() -> Option<HashSet<u32>> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "ppid=", "-o", "args="])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let (ppid, args) = line.trim_start().split_once(char::is_whitespace)?;
                is_background_command(args)
                    .then(|| ppid.parse().ok())
                    .flatten()
            })
            .collect(),
    )
}

/// Claude runs nebula's own hooks as `<shell> -c` children too.
fn is_background_command(args: &str) -> bool {
    is_shell_command(args.split_whitespace())
        && !crate::hooks::installer::is_nebula_hook_command(args)
}

/// `argv` is a POSIX-ish shell running a command string: `zsh -c …`,
/// `bash -lc …`.
fn is_shell_command<'a>(mut argv: impl Iterator<Item = &'a str>) -> bool {
    let shell = argv
        .next()
        .and_then(|arg0| arg0.rsplit('/').next())
        .unwrap_or_default()
        .trim_start_matches('-');
    matches!(shell, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish")
        && argv.next().is_some_and(|flag| {
            flag.starts_with('-') && !flag.starts_with("--") && flag.contains('c')
        })
}

/// Canonical form of a user-typed link. Pasting a URL out of a browser is
/// the common case, but people also type `github.com/o/r/pull/7`, so a
/// scheme-less value gets https://. Anything else — another scheme, or no
/// host at all — is refused rather than stored: the TUI hands these to
/// `open(1)`, and only http(s) may ever reach it.
fn normalize_url(url: &str) -> Result<String> {
    let url = url.trim();
    if url.is_empty() {
        bail!("link URL is empty");
    }
    if url.contains(char::is_whitespace) {
        bail!("link URL contains whitespace");
    }
    let normalized = match url.split_once("://") {
        Some(("http" | "https", _)) => url.to_string(),
        Some((scheme, _)) => bail!("only http(s) links are supported (got {scheme}://)"),
        // Scheme-less: a bare host is a URL people type; a bare word is not.
        None => {
            let host = url.split(['/', '?', '#']).next().unwrap_or_default();
            if !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
                bail!("not a URL: {url}");
            }
            format!("https://{url}")
        }
    };
    // Reject "https://" and friends: a scheme with nothing behind it.
    if normalized
        .split_once("://")
        .is_none_or(|(_, rest)| rest.is_empty())
    {
        bail!("not a URL: {url}");
    }
    Ok(normalized)
}

fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
}

/// Why a create was refused when the agent CLI isn't installed. One line —
/// the TUI shows it in the footer flash, which truncates. Unlike git (which
/// the daemon runs with its own inherited PATH), agent CLIs are spawned
/// through the user's login shell, so a fresh install is picked up on the
/// next try with no daemon restart.
fn cli_missing_message(kind: AgentKind) -> String {
    format!(
        "{} was not found on your PATH — install it, then try again.",
        kind.cli_program()
    )
}

/// Wrap `program args…` in a login + interactive shell (`$SHELL -l -i -c
/// 'exec …'`) so the child gets the user's real environment — ~/.zprofile
/// and ~/.zshrc on zsh — rather than the daemon's. `exec` keeps the child
/// as the PTY's direct process (exit codes and signals pass through).
fn login_shell_wrap(shell: &str, program: &str, args: &[String]) -> (String, Vec<String>) {
    let mut cmdline = String::from("exec");
    for part in std::iter::once(program).chain(args.iter().map(String::as_str)) {
        cmdline.push_str(" '");
        cmdline.push_str(&part.replace('\'', "'\\''"));
        cmdline.push('\'');
    }
    (
        shell.to_string(),
        vec!["-l".into(), "-i".into(), "-c".into(), cmdline],
    )
}

/// Env vars that must never leak into plain terminals (and are re-set
/// explicitly for agent PTYs).
pub fn scrubbed_env_names() -> Vec<String> {
    vec![
        "NEBULA_AGENT_ID".into(),
        "NEBULA_API_URL".into(),
        "NEBULA_API_TOKEN".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_command_per_kind_resume_shapes() {
        // Fresh sessions: CLI plus its skip-permissions flag.
        assert_eq!(
            agent_spawn_command(AgentKind::Claude, None, None, None, None, None),
            (
                "claude".into(),
                vec!["--dangerously-skip-permissions".to_string()],
                false
            )
        );
        // An initial prompt rides as the positional arg on a fresh spawn —
        // and is dropped on a resume, which continues the old conversation.
        assert_eq!(
            agent_spawn_command(
                AgentKind::Claude,
                None,
                None,
                None,
                None,
                Some("do the task")
            ),
            (
                "claude".into(),
                vec![
                    "--dangerously-skip-permissions".to_string(),
                    "do the task".to_string()
                ],
                false
            )
        );
        let (_, resumed_args, resumed) = agent_spawn_command(
            AgentKind::Claude,
            Some("sid"),
            None,
            None,
            None,
            Some("do it"),
        );
        assert!(resumed);
        assert!(
            !resumed_args.contains(&"do it".to_string()),
            "resume must not re-submit the prompt: {resumed_args:?}"
        );
        // Codex/cursor run in skip-permissions mode too.
        assert_eq!(
            agent_spawn_command(AgentKind::Codex, None, None, None, None, None),
            ("codex".into(), vec!["--yolo".to_string()], false)
        );
        // Cursor's agent CLI is `cursor-agent`, not `cursor` (the editor).
        assert_eq!(
            agent_spawn_command(AgentKind::Cursor, None, None, None, None, None),
            ("cursor-agent".into(), vec!["--force".to_string()], false)
        );
        // Pi has no permission prompts, so no skip flag either.
        assert_eq!(
            agent_spawn_command(AgentKind::Pi, None, None, None, None, None),
            ("pi".into(), vec![], false)
        );
        // Claude resumes with a flag; codex with a subcommand (order matters).
        assert_eq!(
            agent_spawn_command(AgentKind::Claude, Some("sid-1"), None, None, None, None),
            (
                "claude".into(),
                vec![
                    "--resume".to_string(),
                    "sid-1".to_string(),
                    "--dangerously-skip-permissions".to_string()
                ],
                true
            )
        );
        // Skip-permissions flags trail the resume args.
        assert_eq!(
            agent_spawn_command(AgentKind::Codex, Some("sid-2"), None, None, None, None),
            (
                "codex".into(),
                vec![
                    "resume".to_string(),
                    "sid-2".to_string(),
                    "--yolo".to_string()
                ],
                true
            )
        );
        assert_eq!(
            agent_spawn_command(AgentKind::Cursor, Some("sid-3"), None, None, None, None),
            (
                "cursor-agent".into(),
                vec![
                    "--resume".to_string(),
                    "sid-3".to_string(),
                    "--force".to_string()
                ],
                true
            )
        );
        // Pi resumes with `--session <id>` (its `--resume` is an
        // interactive picker, not an id flag).
        assert_eq!(
            agent_spawn_command(AgentKind::Pi, Some("sid-4"), None, None, None, None),
            (
                "pi".into(),
                vec!["--session".to_string(), "sid-4".to_string()],
                true
            )
        );
        // Override wins for both kinds and never gets resume args.
        assert_eq!(
            agent_spawn_command(
                AgentKind::Claude,
                Some("sid"),
                None,
                None,
                Some("/bin/sh -i"),
                None
            ),
            ("/bin/sh".into(), vec!["-i".to_string()], false)
        );
        assert_eq!(
            agent_spawn_command(
                AgentKind::Codex,
                Some("sid"),
                None,
                None,
                Some("/bin/sh"),
                None
            ),
            ("/bin/sh".into(), vec![], false)
        );
    }

    #[test]
    fn spawn_command_model_and_effort_flags() {
        // Claude gets --model/--effort; either alone works.
        assert_eq!(
            agent_spawn_command(
                AgentKind::Claude,
                None,
                Some("opus"),
                Some("high"),
                None,
                None
            ),
            (
                "claude".into(),
                vec![
                    "--dangerously-skip-permissions".to_string(),
                    "--model".to_string(),
                    "opus".to_string(),
                    "--effort".to_string(),
                    "high".to_string()
                ],
                false
            )
        );
        assert_eq!(
            agent_spawn_command(AgentKind::Claude, None, None, Some("max"), None, None),
            (
                "claude".into(),
                vec![
                    "--dangerously-skip-permissions".to_string(),
                    "--effort".to_string(),
                    "max".to_string()
                ],
                false
            )
        );
        // Codex takes --model plus a config override for effort, after --yolo.
        assert_eq!(
            agent_spawn_command(
                AgentKind::Codex,
                None,
                Some("gpt-5.5"),
                Some("high"),
                None,
                None
            ),
            (
                "codex".into(),
                vec![
                    "--yolo".to_string(),
                    "--model".to_string(),
                    "gpt-5.5".to_string(),
                    "-c".to_string(),
                    "model_reasoning_effort=high".to_string()
                ],
                false
            )
        );
        // Resume keeps the model/effort flags (a fallback fresh spawn needs
        // them, and the CLIs accept them alongside resume).
        assert_eq!(
            agent_spawn_command(
                AgentKind::Claude,
                Some("sid"),
                Some("sonnet"),
                None,
                None,
                None
            ),
            (
                "claude".into(),
                vec![
                    "--resume".to_string(),
                    "sid".to_string(),
                    "--dangerously-skip-permissions".to_string(),
                    "--model".to_string(),
                    "sonnet".to_string()
                ],
                true
            )
        );
        // Cursor has no model/effort knobs — choices are ignored.
        assert_eq!(
            agent_spawn_command(AgentKind::Cursor, None, Some("m"), Some("e"), None, None),
            ("cursor-agent".into(), vec!["--force".to_string()], false)
        );
        // Pi's effort rides its --thinking flag.
        assert_eq!(
            agent_spawn_command(
                AgentKind::Pi,
                None,
                Some("gpt-5.5"),
                Some("high"),
                None,
                None
            ),
            (
                "pi".into(),
                vec![
                    "--model".to_string(),
                    "gpt-5.5".to_string(),
                    "--thinking".to_string(),
                    "high".to_string()
                ],
                false
            )
        );
        // Override still wins over everything.
        assert_eq!(
            agent_spawn_command(
                AgentKind::Claude,
                None,
                Some("opus"),
                None,
                Some("/bin/sh"),
                None
            ),
            ("/bin/sh".into(), vec![], false)
        );
    }

    #[test]
    fn login_shell_wrap_quotes_and_execs() {
        let (program, args) = login_shell_wrap(
            "/bin/zsh",
            "claude",
            &["--resume".to_string(), "sid-1".to_string()],
        );
        assert_eq!(program, "/bin/zsh");
        assert_eq!(
            args,
            vec!["-l", "-i", "-c", "exec 'claude' '--resume' 'sid-1'"]
        );
        // Single quotes in an arg survive the wrapping.
        let (_, args) = login_shell_wrap("/bin/zsh", "echo", &["it's".to_string()]);
        assert_eq!(args[3], r"exec 'echo' 'it'\''s'");
    }

    fn test_daemon() -> Arc<Daemon> {
        test_daemon_with(DaemonOverrides::default())
    }

    fn test_daemon_stopping(stop_deadline: Duration) -> Arc<Daemon> {
        test_daemon_with(DaemonOverrides {
            stop_deadline,
            ..DaemonOverrides::default()
        })
    }

    fn test_daemon_with(overrides: DaemonOverrides) -> Arc<Daemon> {
        Daemon::with_overrides(
            Arc::new(Store::open_in_memory().unwrap()),
            HookEnv {
                port: 0,
                token: String::new(),
            },
            overrides,
        )
    }

    fn seed_projects(daemon: &Daemon, names: &[&str]) {
        for (i, name) in names.iter().enumerate() {
            daemon
                .store
                .insert_project(&Project {
                    workspace_id: Default::default(),
                    id: ProjectId((*name).into()),
                    name: (*name).into(),
                    repo_path: format!("/tmp/{name}").into(),
                    sort_order: i as i64,
                    divider_after: false,
                    divider_label: None,
                    divider_before: false,
                    divider_before_label: None,
                    host: None,
                })
                .unwrap();
        }
    }

    /// (name, divider_after, divider_label) in display order.
    fn layout(daemon: &Daemon) -> Vec<(String, bool, Option<String>)> {
        let (projects, _, _, _) = daemon.store.load_tree().unwrap();
        projects
            .into_iter()
            .map(|p| (p.name, p.divider_after, p.divider_label))
            .collect()
    }

    /// The leading divider: `Some(label)` when the list has one. Also
    /// asserts the invariant that only the first project ever carries it.
    fn leading(daemon: &Daemon) -> Option<Option<String>> {
        let (projects, _, _, _) = daemon.store.load_tree().unwrap();
        for p in projects.iter().skip(1) {
            assert!(
                !p.divider_before,
                "leading divider drifted off the first project"
            );
        }
        let first = projects.first()?;
        first
            .divider_before
            .then(|| first.divider_before_label.clone())
    }

    fn names(daemon: &Daemon) -> Vec<String> {
        layout(daemon).into_iter().map(|(n, _, _)| n).collect()
    }

    #[test]
    fn move_project_reorders_and_normalizes_sort_orders() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b", "c", "d"]);

        daemon.move_project(&ProjectId("d".into()), -2).unwrap();
        assert_eq!(names(&daemon), ["a", "d", "b", "c"]);
        let (projects, _, _, _) = daemon.store.load_tree().unwrap();
        assert_eq!(
            projects.iter().map(|p| p.sort_order).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );

        // Edge moves clamp to no-ops.
        daemon.move_project(&ProjectId("a".into()), -1).unwrap();
        daemon.move_project(&ProjectId("c".into()), 5).unwrap();
        assert_eq!(names(&daemon), ["a", "d", "b", "c"]);
    }

    #[test]
    fn moves_step_one_visual_row_so_projects_park_beside_dividers() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b", "c", "d"]);
        // Groups: [a b] [c d], labeled "work".
        daemon
            .set_project_divider(&ProjectId("b".into()), false, true, Some("work".into()))
            .unwrap();

        // First press: c crosses the divider into the first group without
        // swapping past b — the divider (and its label) keeps marking the
        // same gap, now below c.
        daemon.move_project(&ProjectId("c".into()), -1).unwrap();
        assert_eq!(
            layout(&daemon),
            [
                ("a".to_string(), false, None),
                ("b".to_string(), false, None),
                ("c".to_string(), true, Some("work".to_string())),
                ("d".to_string(), false, None),
            ]
        );

        // Second press: c swaps with b like any project-to-project move.
        daemon.move_project(&ProjectId("c".into()), -1).unwrap();
        assert_eq!(
            layout(&daemon),
            [
                ("a".to_string(), false, None),
                ("c".to_string(), false, None),
                ("b".to_string(), true, Some("work".to_string())),
                ("d".to_string(), false, None),
            ]
        );

        // Moving down retraces the same two steps back to the start.
        daemon.move_project(&ProjectId("c".into()), 1).unwrap();
        daemon.move_project(&ProjectId("c".into()), 1).unwrap();
        assert_eq!(
            layout(&daemon),
            [
                ("a".to_string(), false, None),
                ("b".to_string(), true, Some("work".to_string())),
                ("c".to_string(), false, None),
                ("d".to_string(), false, None),
            ]
        );
    }

    #[test]
    fn top_project_crossing_its_divider_leaves_it_leading() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b"]);
        daemon
            .set_project_divider(&ProjectId("a".into()), false, true, Some("work".into()))
            .unwrap();

        // a steps below its own divider, which stays put above the whole
        // list — the leading divider, label intact.
        daemon.move_project(&ProjectId("a".into()), 1).unwrap();
        assert_eq!(names(&daemon), ["a", "b"]);
        assert_eq!(leading(&daemon), Some(Some("work".to_string())));
        assert!(layout(&daemon).iter().all(|(_, divider, _)| !divider));

        // The next press swaps a past b like any project move; the leading
        // divider stays on top, now owned by b.
        daemon.move_project(&ProjectId("a".into()), 1).unwrap();
        assert_eq!(names(&daemon), ["b", "a"]);
        assert_eq!(leading(&daemon), Some(Some("work".to_string())));

        // Moving a back up retraces both steps.
        daemon.move_project(&ProjectId("a".into()), -1).unwrap();
        assert_eq!(names(&daemon), ["a", "b"]);
        assert_eq!(leading(&daemon), Some(Some("work".to_string())));
        daemon.move_project(&ProjectId("a".into()), -1).unwrap();
        assert_eq!(leading(&daemon), None);
        assert_eq!(
            layout(&daemon)[0],
            ("a".to_string(), true, Some("work".to_string()))
        );
    }

    #[test]
    fn move_divider_hops_above_the_first_project_and_back() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b"]);
        daemon
            .set_project_divider(&ProjectId("a".into()), false, true, Some("work".into()))
            .unwrap();

        // Up from under a: the divider crosses onto the top slot.
        daemon
            .move_divider(&ProjectId("a".into()), false, -1)
            .unwrap();
        assert_eq!(leading(&daemon), Some(Some("work".to_string())));
        assert!(layout(&daemon).iter().all(|(_, divider, _)| !divider));

        // Up again clamps — it is already above everything.
        daemon
            .move_divider(&ProjectId("a".into()), true, -1)
            .unwrap();
        assert_eq!(leading(&daemon), Some(Some("work".to_string())));

        // Down: back under a.
        daemon
            .move_divider(&ProjectId("a".into()), true, 1)
            .unwrap();
        assert_eq!(leading(&daemon), None);
        assert_eq!(
            layout(&daemon)[0],
            ("a".to_string(), true, Some("work".to_string()))
        );
    }

    #[test]
    fn leading_divider_blocks_on_a_stacked_gap() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b"]);
        daemon
            .set_project_divider(&ProjectId("a".into()), true, true, Some("top".into()))
            .unwrap();
        daemon
            .set_project_divider(&ProjectId("a".into()), false, true, Some("mid".into()))
            .unwrap();

        // Neither divider can move onto the other's gap.
        daemon
            .move_divider(&ProjectId("a".into()), true, 1)
            .unwrap();
        daemon
            .move_divider(&ProjectId("a".into()), false, -1)
            .unwrap();
        assert_eq!(leading(&daemon), Some(Some("top".to_string())));
        assert_eq!(
            layout(&daemon)[0],
            ("a".to_string(), true, Some("mid".to_string()))
        );

        // And the project pinched between them can't move either — that
        // would stack the dividers into one gap.
        daemon.move_project(&ProjectId("a".into()), 1).unwrap();
        daemon.move_project(&ProjectId("a".into()), -1).unwrap();
        assert_eq!(names(&daemon), ["a", "b"]);
        assert_eq!(leading(&daemon), Some(Some("top".to_string())));
    }

    #[test]
    fn removing_the_top_project_hands_the_leading_divider_down() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b"]);
        daemon
            .set_project_divider(&ProjectId("a".into()), true, true, Some("work".into()))
            .unwrap();

        daemon.remove_project(&ProjectId("a".into())).unwrap();
        assert_eq!(names(&daemon), ["b"]);
        assert_eq!(leading(&daemon), Some(Some("work".to_string())));
    }

    #[test]
    fn move_divider_steps_between_projects_and_keeps_its_label() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b", "c"]);
        daemon
            .set_project_divider(&ProjectId("a".into()), false, true, Some("work".into()))
            .unwrap();

        // Down: the divider hops from under a to under b, label intact.
        daemon
            .move_divider(&ProjectId("a".into()), false, 1)
            .unwrap();
        assert_eq!(
            layout(&daemon),
            [
                ("a".to_string(), false, None),
                ("b".to_string(), true, Some("work".to_string())),
                ("c".to_string(), false, None),
            ]
        );

        // Back up under a (the top hop has its own test).
        daemon
            .move_divider(&ProjectId("b".into()), false, -1)
            .unwrap();
        assert_eq!(
            layout(&daemon)[0],
            ("a".to_string(), true, Some("work".to_string()))
        );

        // Down past the last project clamps.
        daemon
            .move_divider(&ProjectId("a".into()), false, 1)
            .unwrap();
        daemon
            .move_divider(&ProjectId("b".into()), false, 1)
            .unwrap();
        daemon
            .move_divider(&ProjectId("c".into()), false, 1)
            .unwrap();
        assert_eq!(
            layout(&daemon)[2],
            ("c".to_string(), true, Some("work".to_string()))
        );
    }

    #[test]
    fn move_divider_blocks_on_a_neighboring_divider() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b"]);
        daemon
            .set_project_divider(&ProjectId("a".into()), false, true, Some("one".into()))
            .unwrap();
        daemon
            .set_project_divider(&ProjectId("b".into()), false, true, Some("two".into()))
            .unwrap();

        // Neither divider can move onto the other's gap.
        daemon
            .move_divider(&ProjectId("a".into()), false, 1)
            .unwrap();
        daemon
            .move_divider(&ProjectId("b".into()), false, -1)
            .unwrap();
        assert_eq!(
            layout(&daemon),
            [
                ("a".to_string(), true, Some("one".to_string())),
                ("b".to_string(), true, Some("two".to_string())),
            ]
        );
    }

    #[test]
    fn relabeling_keeps_divider_and_removal_drops_label() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b"]);
        daemon
            .set_project_divider(&ProjectId("a".into()), false, true, Some("work".into()))
            .unwrap();

        daemon
            .set_project_divider(&ProjectId("a".into()), false, true, Some("play".into()))
            .unwrap();
        assert_eq!(layout(&daemon)[0].2.as_deref(), Some("play"));
        daemon
            .set_project_divider(&ProjectId("a".into()), false, false, Some("ignored".into()))
            .unwrap();
        assert!(layout(&daemon)
            .iter()
            .all(|(_, divider, label)| !divider && label.is_none()));
    }

    fn seed_worktree(daemon: &Daemon, project: &str, id: &str, path: &str, is_main: bool) {
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId(id.into()),
                project_id: ProjectId(project.into()),
                path: path.into(),
                branch: id.into(),
                is_main,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();
    }

    fn seed_agent(daemon: &Daemon, id: &str, worktree: &str, session_id: Option<&str>) {
        daemon
            .store
            .insert_agent(&Agent {
                id: AgentId(id.into()),
                worktree_id: WorktreeId(worktree.into()),
                name: id.into(),
                status: AgentStatus::Running,
                archived: false,
                archived_at: 0,
                pinned: false,
                kind: AgentKind::Claude,
                model: None,
                effort: None,
                session_id: session_id.map(str::to_string),
                sort_order: 0,
                status_changed_at: 0,
                alive: false,
                branch: None,
                writer: false,
            })
            .unwrap();
    }

    fn seed_terminal(daemon: &Daemon, id: &str, worktree: &str) {
        daemon
            .store
            .insert_terminal(&TerminalTab {
                id: TerminalId(id.into()),
                worktree_id: WorktreeId(worktree.into()),
                name: id.into(),
                sort_order: 0,
                alive: false,
                branch: None,
                busy: false,
                status: None,
                status_changed_at: 0,
            })
            .unwrap();
    }

    fn terminal_worktree(daemon: &Daemon, id: &str) -> String {
        daemon
            .store
            .get_terminal(&TerminalId(id.into()))
            .unwrap()
            .unwrap()
            .worktree_id
            .to_string()
    }

    fn agent_worktree(daemon: &Daemon, id: &str) -> String {
        daemon
            .store
            .get_agent(&AgentId(id.into()))
            .unwrap()
            .unwrap()
            .worktree_id
            .to_string()
    }

    #[test]
    fn move_agent_rehomes_row_and_broadcasts() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p"]);
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let feat = tmp.path().join("feat");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "root"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(
            &repo,
            &["worktree", "add", "-b", "feat", feat.to_str().unwrap()],
        );
        seed_worktree(&daemon, "p", "root", repo.to_str().unwrap(), true);
        seed_worktree(&daemon, "p", "feat", feat.to_str().unwrap(), false);
        seed_agent(&daemon, "a1", "root", None);
        let mut rx = daemon.events.subscribe();

        daemon
            .move_agent(&AgentId("a1".into()), &WorktreeId("feat".into()))
            .unwrap();
        assert_eq!(agent_worktree(&daemon, "a1"), "feat");
        match rx.try_recv().unwrap() {
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(a),
            } => assert_eq!(a.worktree_id.to_string(), "feat"),
            other => panic!("expected agent upsert, got {other:?}"),
        }

        // Moving to the worktree it already lives in is a silent no-op.
        daemon
            .move_agent(&AgentId("a1".into()), &WorktreeId("feat".into()))
            .unwrap();
        assert!(rx.try_recv().is_err(), "no broadcast for a no-op move");
    }

    fn seed_pending_agent(daemon: &Daemon, id: &str, worktree: &str) {
        daemon
            .store
            .insert_agent_with_auto_title(
                &Agent {
                    id: AgentId(id.into()),
                    worktree_id: WorktreeId(worktree.into()),
                    name: format!("{id}-default"),
                    status: AgentStatus::Fresh,
                    archived: false,
                    archived_at: 0,
                    pinned: false,
                    kind: AgentKind::Claude,
                    model: None,
                    effort: None,
                    session_id: None,
                    sort_order: 0,
                    status_changed_at: 0,
                    alive: false,
                    branch: None,
                    writer: false,
                },
                true,
            )
            .unwrap();
    }

    #[test]
    fn auto_rename_applies_once_and_defers_to_user_titles() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p"]);
        seed_worktree(&daemon, "p", "root", "/nebula-test/p", true);
        seed_pending_agent(&daemon, "a1", "root");
        let mut rx = daemon.events.subscribe();

        // First agent attempt lands, sanitized, and is broadcast.
        daemon
            .auto_rename_agent(&AgentId("a1".into()), "  Fix   Login\tRedirect  ")
            .unwrap();
        match rx.try_recv().unwrap() {
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(a),
            } => assert_eq!(a.name, "Fix Login Redirect"),
            other => panic!("expected agent upsert, got {other:?}"),
        }

        // A second attempt is declined with a settled, informative error.
        let err = daemon
            .auto_rename_agent(&AgentId("a1".into()), "Another Title")
            .unwrap_err();
        assert!(err.to_string().contains("already has a title"), "{err}");
        assert_eq!(
            daemon
                .store
                .get_agent(&AgentId("a1".into()))
                .unwrap()
                .unwrap()
                .name,
            "Fix Login Redirect"
        );

        // A user rename beats a pending auto-title: the CLI's later attempt
        // must not clobber it.
        seed_pending_agent(&daemon, "a2", "root");
        daemon
            .rename_agent(&AgentId("a2".into()), "my session")
            .unwrap();
        let err = daemon
            .auto_rename_agent(&AgentId("a2".into()), "Model Title")
            .unwrap_err();
        assert!(err.to_string().contains("already has a title"), "{err}");

        // Garbage titles are rejected outright.
        assert!(daemon
            .auto_rename_agent(&AgentId("a1".into()), " \u{7}\n ")
            .is_err());
        // Unknown agents report cleanly.
        let err = daemon
            .auto_rename_agent(&AgentId("ghost".into()), "Some Title")
            .unwrap_err();
        assert!(err.to_string().contains("agent not found"), "{err}");
    }

    #[test]
    fn sanitize_title_collapses_and_caps() {
        assert_eq!(
            sanitize_title(" Fix   Login\u{7}Redirect \n"),
            "Fix Login Redirect"
        );
        assert_eq!(sanitize_title("\u{1b}[31m"), "[31m");
        assert_eq!(sanitize_title("   "), "");
        let long = "word ".repeat(30);
        assert!(sanitize_title(&long).chars().count() <= 60);
        assert!(!sanitize_title(&long).ends_with(' '));
    }

    #[test]
    fn move_agent_rejects_cross_project_targets() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p", "q"]);
        seed_worktree(&daemon, "p", "p-root", "/nebula-test/p", true);
        seed_worktree(&daemon, "q", "q-root", "/nebula-test/q", true);
        seed_agent(&daemon, "a1", "p-root", None);

        let err = daemon
            .move_agent(&AgentId("a1".into()), &WorktreeId("q-root".into()))
            .unwrap_err();
        assert!(err.to_string().contains("different project"));
        assert_eq!(agent_worktree(&daemon, "a1"), "p-root");
    }

    #[test]
    fn terminal_follows_its_shell_cwd_between_checkouts() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p", "q"]);
        seed_worktree(&daemon, "p", "root", "/nebula-test/p", true);
        seed_worktree(&daemon, "p", "feat", "/nebula-test/p/.wt/feat", false);
        seed_worktree(&daemon, "q", "q-root", "/nebula-test/q", true);
        seed_terminal(&daemon, "t1", "root");
        let t1 = TerminalId("t1".into());

        // A subdirectory of the root checkout is home already.
        daemon
            .reparent_terminal_to_cwd(&t1, Path::new("/nebula-test/p/src"))
            .unwrap();
        assert_eq!(terminal_worktree(&daemon, "t1"), "root");
        // `cd` into the nested worktree moves the tab under it (deepest
        // match wins over the root prefix).
        daemon
            .reparent_terminal_to_cwd(&t1, Path::new("/nebula-test/p/.wt/feat/src"))
            .unwrap();
        assert_eq!(terminal_worktree(&daemon, "t1"), "feat");
        // …and `cd` back moves it home again.
        daemon
            .reparent_terminal_to_cwd(&t1, Path::new("/nebula-test/p"))
            .unwrap();
        assert_eq!(terminal_worktree(&daemon, "t1"), "root");
        // Outside every checkout, and inside another project, the row
        // stays put.
        daemon
            .reparent_terminal_to_cwd(&t1, Path::new("/elsewhere"))
            .unwrap();
        daemon
            .reparent_terminal_to_cwd(&t1, Path::new("/nebula-test/q/src"))
            .unwrap();
        assert_eq!(terminal_worktree(&daemon, "t1"), "root");
    }

    #[test]
    fn process_cwd_reads_our_own_working_directory() {
        let me = std::process::id();
        let cwd = process_cwd(me).expect("own cwd is readable");
        assert_eq!(
            canonical_or_raw(&cwd),
            canonical_or_raw(&std::env::current_dir().unwrap())
        );
        // A pid nobody owns reads as nothing rather than garbage.
        assert!(process_cwd(u32::MAX - 1).is_none());
    }

    #[test]
    fn reparent_by_cwd_picks_deepest_matching_worktree() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p"]);
        // Nested layout: the linked checkout lives under the repo root, so
        // both paths are prefixes of a cwd inside it — deepest must win.
        seed_worktree(&daemon, "p", "root", "/nebula-test/p", true);
        seed_worktree(&daemon, "p", "feat", "/nebula-test/p/.wt/feat", false);
        seed_agent(&daemon, "a1", "root", None);

        // cwd inside the root checkout (but outside the nested worktree)
        // keeps the agent where it is.
        daemon.reparent_agent_by_cwd(&AgentId("a1".into()), "/nebula-test/p/src", None, false);
        assert_eq!(agent_worktree(&daemon, "a1"), "root");

        // cwd inside the nested worktree re-homes it there.
        daemon.reparent_agent_by_cwd(
            &AgentId("a1".into()),
            "/nebula-test/p/.wt/feat/src",
            None,
            false,
        );
        assert_eq!(agent_worktree(&daemon, "a1"), "feat");

        // cwd outside every worktree is ignored.
        daemon.reparent_agent_by_cwd(&AgentId("a1".into()), "/elsewhere", None, false);
        assert_eq!(agent_worktree(&daemon, "a1"), "feat");
    }

    /// Regression: a session that creates a worktree and steps into it
    /// reports the new cwd *before* the sync has adopted a row for it (the
    /// `Stop` hook fires long before the next 2s sync tick). The cwd must be
    /// remembered and replayed on adoption, or the row sits under the old
    /// checkout until the user's next prompt.
    #[tokio::test]
    async fn worktree_sync_replays_a_cwd_reported_before_adoption() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);

        let daemon = test_daemon();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId("p".into()),
            name: "p".into(),
            repo_path: repo.clone(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        daemon.store.insert_project(&project).unwrap();
        seed_worktree(&daemon, "p", "root", &repo.to_string_lossy(), true);
        seed_agent(&daemon, "a1", "root", Some("s1"));

        // The agent creates a sibling worktree and walks into it. The hook
        // lands first: no row exists yet, so nothing moves.
        let feat = root.join("repo-worktrees").join("feat");
        git_in(
            &repo,
            &["worktree", "add", &feat.to_string_lossy(), "-b", "feat"],
        );
        daemon.reparent_agent_by_cwd(
            &AgentId("a1".into()),
            &feat.to_string_lossy(),
            Some("s1"),
            false,
        );
        assert_eq!(agent_worktree(&daemon, "a1"), "root");

        // The sync adopts the checkout and replays the remembered cwd.
        daemon.sync_project_worktrees(&project).await.unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let adopted = worktrees
            .iter()
            .find(|w| w.branch == "feat")
            .expect("feat worktree adopted");
        assert_eq!(agent_worktree(&daemon, "a1"), adopted.id.to_string());

        // A deliberate move back must survive the next adoption: the move
        // drops the remembered cwd, so replaying it can't overrule the user.
        daemon
            .move_agent(&AgentId("a1".into()), &WorktreeId("root".into()))
            .unwrap();
        let other = root.join("repo-worktrees").join("other");
        git_in(
            &repo,
            &["worktree", "add", &other.to_string_lossy(), "-b", "other"],
        );
        daemon.sync_project_worktrees(&project).await.unwrap();
        assert_eq!(agent_worktree(&daemon, "a1"), "root");
    }

    /// A worktree row that never recorded its lineage (created before
    /// nebula stored bases, or via a base-less create for an existing
    /// branch) backfills `created_from` from the branch's reflog on the
    /// next sync — the panel's tree line appears without recreating the
    /// checkout. Adoption of an unknown checkout derives it the same way.
    #[tokio::test]
    async fn worktree_sync_backfills_lineage_from_the_reflog() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "dad", "main"]);
        let dad = root.join("repo-worktrees").join("dad");
        git_in(&repo, &["worktree", "add", &dad.to_string_lossy(), "dad"]);

        let daemon = test_daemon();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId("p".into()),
            name: "p".into(),
            repo_path: repo.clone(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        daemon.store.insert_project(&project).unwrap();
        seed_worktree(&daemon, "p", "root", &repo.to_string_lossy(), true);
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("wt-dad".into()),
                project_id: ProjectId("p".into()),
                path: dad.clone(),
                branch: "dad".into(),
                is_main: false,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();

        daemon.sync_project_worktrees(&project).await.unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let row = worktrees.iter().find(|w| w.branch == "dad").unwrap();
        assert_eq!(row.created_from.as_deref(), Some("main"));

        // An unknown checkout adopted by the sync gets its lineage at
        // adoption time, not just on the backfill pass.
        git_in(&repo, &["branch", "kid", "dad"]);
        let kid = root.join("repo-worktrees").join("kid");
        git_in(&repo, &["worktree", "add", &kid.to_string_lossy(), "kid"]);
        daemon.sync_project_worktrees(&project).await.unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let row = worktrees.iter().find(|w| w.branch == "kid").unwrap();
        assert_eq!(row.created_from.as_deref(), Some("dad"));
    }

    /// `create_worktree` records lineage even without an explicit base: an
    /// existing branch keeps its reflog base, a fresh branch records the
    /// root checkout's HEAD it was cut from.
    #[tokio::test]
    async fn checkout_primary_preserves_a_parked_checkout_and_its_stopped_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "feat", "main"]);

        let daemon = test_daemon();
        let pid = ProjectId("p".into());
        let project = Project {
            workspace_id: Default::default(),
            id: pid.clone(),
            name: "p".into(),
            repo_path: repo.clone(),
            sort_order: 0,
            divider_after: false,
            divider_before: false,
            divider_label: None,
            divider_before_label: None,
            host: None,
        };
        daemon.store.insert_project(&project).unwrap();
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("root".into()),
                project_id: pid.clone(),
                path: repo.clone(),
                branch: "main".into(),
                is_main: true,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();
        daemon.create_worktree(&pid, "feat", None).await.unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let parked = worktrees
            .iter()
            .find(|w| w.branch == "feat")
            .unwrap()
            .clone();
        assert!(parked.path.exists());

        // git itself refuses the plain checkout while the branch is parked.
        assert!(git::checkout(&repo, "feat").await.is_err());

        seed_agent(&daemon, "stopped", parked.id.as_str(), Some("conversation"));
        seed_terminal(&daemon, "idle", "root");
        let idle = SessionRef::Terminal(TerminalId("idle".into()));
        spawn_live(&daemon, idle.clone(), &repo, "exec sleep 60");
        let err = daemon
            .checkout_primary(&pid, "feat", None, false)
            .await
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("Explicitly select it with --worktree"));
        assert!(daemon.is_alive(&idle), "the parked refusal stops nothing");
        daemon.kill_session(&idle);
        let ServerEvent::Snapshot {
            worktrees, agents, ..
        } = daemon.snapshot().unwrap()
        else {
            panic!()
        };
        assert!(parked.path.exists(), "existing checkout survives");
        assert!(worktrees.iter().any(|w| w.id == parked.id));
        assert!(
            agents
                .iter()
                .any(|a| a.id.as_str() == "stopped"
                    && a.session_id.as_deref() == Some("conversation"))
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "main");

        // Unknown branch: refused, nothing touched.
        assert!(daemon
            .checkout_primary(&pid, "nope", None, false)
            .await
            .is_err());

        // A branch that only origin has (pushed from another machine) is
        // fetched into place and checked out — the "run on findl from a
        // branch findl never saw" case.
        let origin = root.join("origin.git");
        git_in(
            &root,
            &[
                "clone",
                "--bare",
                "-q",
                repo.to_str().unwrap(),
                "origin.git",
            ],
        );
        git_in(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_in(&origin, &["branch", "pushed", "main"]);
        assert!(!git::branch_exists(&repo, "pushed").await);
        daemon
            .checkout_primary(&pid, "pushed", None, false)
            .await
            .unwrap();
        assert_eq!(git::current_branch(&repo).await.unwrap(), "pushed");
        // Still unknown everywhere: the error says what to do.
        let err = daemon
            .checkout_primary(&pid, "nope", None, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("push it first"), "{err}");
    }

    /// Explicit primary movement retains stopped sessions' resume expectations;
    /// only the caller changes affinity, and
    /// the main row records the branch it was cut from (so the panel keeps
    /// it under its parent). Switching back onto a base-less branch clears
    /// the stale lineage.
    #[tokio::test]
    async fn checkout_primary_preserves_stopped_resume_affinity_but_updates_the_mover() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "feat", "main"]);

        let daemon = test_daemon();
        let pid = ProjectId("p".into());
        daemon
            .store
            .insert_project(&Project {
                workspace_id: Default::default(),
                id: pid.clone(),
                name: "p".into(),
                repo_path: repo.clone(),
                sort_order: 0,
                divider_after: false,
                divider_before: false,
                divider_label: None,
                divider_before_label: None,
                host: None,
            })
            .unwrap();
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("root".into()),
                project_id: pid.clone(),
                path: repo.clone(),
                branch: "main".into(),
                is_main: true,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();
        for id in ["mover", "stays"] {
            seed_agent(&daemon, id, "root", None);
            daemon
                .store
                .set_agent_branch(&AgentId(id.into()), Some("main"))
                .unwrap();
        }
        seed_terminal(&daemon, "shell", "root");
        daemon
            .store
            .set_terminal_branch(&TerminalId("shell".into()), Some("main"))
            .unwrap();

        let mover = SessionRef::Agent(AgentId("mover".into()));
        daemon
            .checkout_primary(&pid, "feat", Some(&mover), false)
            .await
            .unwrap();

        let branch_of = |id: &str| {
            daemon
                .store
                .get_agent(&AgentId(id.into()))
                .unwrap()
                .unwrap()
                .branch
        };
        assert_eq!(branch_of("mover").as_deref(), Some("feat"), "follows");
        assert_eq!(branch_of("stays").as_deref(), Some("main"), "left behind");
        assert_eq!(
            daemon
                .store
                .get_terminal(&TerminalId("shell".into()))
                .unwrap()
                .unwrap()
                .branch
                .as_deref(),
            Some("main")
        );
        let main = daemon
            .store
            .get_worktree(&WorktreeId("root".into()))
            .unwrap()
            .unwrap();
        assert_eq!(main.branch, "feat");
        assert_eq!(
            main.created_from.as_deref(),
            Some("main"),
            "the primary records the branch it was cut from"
        );

        daemon
            .checkout_primary(&pid, "main", None, false)
            .await
            .unwrap();
        let main = daemon
            .store
            .get_worktree(&WorktreeId("root".into()))
            .unwrap()
            .unwrap();
        assert_eq!(main.branch, "main");
        assert_eq!(
            main.created_from, None,
            "no base: the old lineage is cleared"
        );
        assert_eq!(
            branch_of("mover").as_deref(),
            Some("feat"),
            "no mover this time"
        );
    }

    async fn primary_with_feat() -> (tempfile::TempDir, PathBuf, Arc<Daemon>, ProjectId, Worktree) {
        primary_with_feat_on(test_daemon()).await
    }

    async fn primary_with_feat_on(
        daemon: Arc<Daemon>,
    ) -> (tempfile::TempDir, PathBuf, Arc<Daemon>, ProjectId, Worktree) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "main\n").unwrap();
        std::fs::write(repo.join("lib.rs"), "main\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-m", "init"]);
        git_in(&repo, &["checkout", "-b", "feat"]);
        std::fs::write(repo.join("notes.txt"), "feat\n").unwrap();
        git_in(&repo, &["add", "notes.txt"]);
        git_in(&repo, &["commit", "-m", "notes"]);
        git_in(&repo, &["checkout", "main"]);
        let EntityId::Project(pid) = daemon.add_project(&repo, None, false, None).await.unwrap()
        else {
            panic!()
        };
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let primary = worktrees.into_iter().find(|w| w.is_main).unwrap();
        (tmp, repo, daemon, pid, primary)
    }

    fn spawn_live(daemon: &Arc<Daemon>, sref: SessionRef, cwd: &Path, script: &str) -> u32 {
        let session = PtySession::spawn(
            sref,
            SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: cwd.to_path_buf(),
                env: Vec::new(),
                scrub_env: Vec::new(),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
        let pid = session.child_pid.unwrap();
        daemon.install_session(session);
        pid
    }

    async fn process_exits(pid: u32) -> bool {
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_dirty_primary_refuses_the_switch_before_stopping_anything() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_terminal(&daemon, "shell", primary.id.as_str());
        let shell = SessionRef::Terminal(TerminalId("shell".into()));
        spawn_live(&daemon, shell.clone(), &repo, "exec sleep 60");
        std::fs::write(repo.join("README.md"), "edited\n").unwrap();
        std::fs::write(repo.join("lib.rs"), "staged\n").unwrap();
        git_in(&repo, &["add", "lib.rs"]);
        std::fs::write(repo.join("scratch.txt"), "untracked\n").unwrap();

        let err = daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "primary has 2 uncommitted files on \"main\"; commit them before switching to \"feat\""
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "main");
        assert!(daemon.is_alive(&shell));
        daemon.kill_session(&shell);
    }

    #[tokio::test]
    async fn an_untracked_file_the_target_tracks_refuses_the_switch_before_stopping_anything() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_terminal(&daemon, "shell", primary.id.as_str());
        let shell = SessionRef::Terminal(TerminalId("shell".into()));
        spawn_live(&daemon, shell.clone(), &repo, "exec sleep 60");
        std::fs::write(repo.join("notes.txt"), "mine\n").unwrap();

        let err = daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "untracked \"notes.txt\" in primary would be overwritten by \"feat\"; move or commit it before switching"
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "main");
        assert_eq!(
            std::fs::read_to_string(repo.join("notes.txt")).unwrap(),
            "mine\n"
        );
        assert!(daemon.is_alive(&shell));
        daemon.kill_session(&shell);
    }

    #[tokio::test]
    async fn an_untracked_path_colliding_with_a_target_directory_or_file_refuses_before_stopping_anything(
    ) {
        for (target, tracked, untracked) in [
            ("dir-target", "assets/icon.png", "assets"),
            ("file-target", "assets", "assets/local.txt"),
        ] {
            let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
            git_in(&repo, &["checkout", "-b", target]);
            let tracked_path = repo.join(tracked);
            std::fs::create_dir_all(tracked_path.parent().unwrap()).unwrap();
            std::fs::write(&tracked_path, "tracked\n").unwrap();
            git_in(&repo, &["add", tracked]);
            git_in(&repo, &["commit", "-m", target]);
            git_in(&repo, &["checkout", "main"]);
            let untracked_path = repo.join(untracked);
            std::fs::create_dir_all(untracked_path.parent().unwrap()).unwrap();
            std::fs::write(&untracked_path, "mine\n").unwrap();
            seed_terminal(&daemon, "shell", primary.id.as_str());
            let shell = SessionRef::Terminal(TerminalId("shell".into()));
            spawn_live(&daemon, shell.clone(), &repo, "exec sleep 60");

            let err = daemon
                .checkout_primary(&pid, target, None, true)
                .await
                .unwrap_err();

            assert_eq!(
                err.to_string(),
                format!("untracked \"assets\" in primary would be overwritten by \"{target}\"; move or commit it before switching")
            );
            assert_eq!(git::current_branch(&repo).await.unwrap(), "main");
            assert_eq!(std::fs::read_to_string(&untracked_path).unwrap(), "mine\n");
            assert!(
                daemon.is_alive(&shell),
                "{target}: the shell survives the refusal"
            );
            daemon.kill_session(&shell);
        }
    }

    async fn until_running_sleep(pid: u32) {
        for _ in 0..50 {
            let out = std::process::Command::new("ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&out.stdout)
                .trim()
                .ends_with("sleep")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{pid} never reached its sleep");
    }

    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[tokio::test]
    async fn the_checkout_waits_until_a_session_ignoring_hangup_is_gone() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_terminal(&daemon, "stubborn", primary.id.as_str());
        let shell = SessionRef::Terminal(TerminalId("stubborn".into()));
        let shell_pid = spawn_live(&daemon, shell, &repo, "trap '' HUP; exec sleep 60");

        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();

        assert!(
            !process_alive(shell_pid),
            "checked out while the session still ran"
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "feat");
    }

    #[tokio::test]
    async fn parking_a_just_resumed_agent_keeps_its_conversation() {
        let (_tmp, repo, daemon, _pid, primary) = primary_with_feat().await;
        seed_agent(
            &daemon,
            "resumed",
            primary.id.as_str(),
            Some("conversation"),
        );
        let id = AgentId("resumed".into());
        daemon.store.set_agent_branch(&id, Some("main")).unwrap();
        let agent = SessionRef::Agent(id.clone());
        spawn_live(&daemon, agent.clone(), &repo, "exec sleep 60");
        let row = daemon.store.get_agent(&id).unwrap().unwrap();
        daemon.arm_resume_fallback(row, primary, daemon.session(&agent).unwrap(), 80, 24);

        daemon.kill_session(&agent);
        daemon.publish_session_end(&agent, SessionEnd::Parked);
        tokio::time::sleep(Duration::from_millis(2500)).await;

        let ServerEvent::Snapshot { agents, .. } = daemon.snapshot().unwrap() else {
            panic!()
        };
        daemon.kill_all();
        let resumed = agents.iter().find(|a| a.id == id).unwrap();
        assert_eq!(resumed.session_id.as_deref(), Some("conversation"));
    }

    fn agent_status(daemon: &Daemon, id: &str) -> AgentStatus {
        let ServerEvent::Snapshot { agents, .. } = daemon.snapshot().unwrap() else {
            panic!()
        };
        agents.iter().find(|a| a.id.as_str() == id).unwrap().status
    }

    #[tokio::test]
    async fn a_parked_busy_agent_reads_stopped_and_counts_as_busy_again_once_resumed() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_agent(&daemon, "runner", primary.id.as_str(), Some("conversation"));
        let agent = SessionRef::Agent(AgentId("runner".into()));
        spawn_live(&daemon, agent.clone(), &repo, "exec sleep 60");

        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();
        assert_eq!(agent_status(&daemon, "runner"), AgentStatus::Disconnected);

        daemon
            .checkout_primary(&pid, "main", None, false)
            .await
            .unwrap();
        spawn_live(&daemon, agent.clone(), &repo, "exec sleep 60");
        daemon.apply_hook_event(
            &AgentId("runner".into()),
            HookEvent::Progress { busy: true },
            None,
        );
        assert_eq!(agent_status(&daemon, "runner"), AgentStatus::Running);
        daemon.kill_all();
    }

    #[tokio::test]
    async fn a_survivor_that_exits_after_the_timeout_ends_as_a_natural_exit_and_is_resumable() {
        let (_tmp, repo, daemon, pid, primary) =
            primary_with_feat_on(test_daemon_stopping(Duration::from_millis(500))).await;
        seed_agent(&daemon, "runner", primary.id.as_str(), Some("conversation"));
        seed_terminal(&daemon, "shell", primary.id.as_str());
        let agent = SessionRef::Agent(AgentId("runner".into()));
        let shell = SessionRef::Terminal(TerminalId("shell".into()));
        spawn_live(&daemon, agent.clone(), &repo, "trap '' HUP; exec sleep 1.5");
        spawn_live(&daemon, shell.clone(), &repo, "trap '' HUP; exec sleep 1.5");
        let dead = daemon.session(&shell).unwrap();

        let err = daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .ends_with("still running; primary stays on \"main\""),
            "{err}"
        );

        for _ in 0..50 {
            if !daemon.is_alive(&agent) && !daemon.is_alive(&shell) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let ServerEvent::Snapshot {
            agents, terminals, ..
        } = daemon.snapshot().unwrap()
        else {
            panic!()
        };
        let runner = agents.iter().find(|a| a.id.as_str() == "runner").unwrap();
        assert!(!runner.alive, "the exited survivor no longer reads alive");
        assert_eq!(runner.status, AgentStatus::Finished);
        assert_eq!(runner.branch.as_deref(), Some("main"));
        assert_eq!(runner.session_id.as_deref(), Some("conversation"));
        let tab = terminals.iter().find(|t| t.id.as_str() == "shell").unwrap();
        assert!(!tab.alive);
        assert_eq!(tab.branch.as_deref(), Some("main"));
        let resumed = daemon.ensure_session(&shell, 80, 24).unwrap();
        assert!(!Arc::ptr_eq(&dead, &resumed), "resume spawns a new PTY");
        assert!(daemon.is_alive(&shell));
        daemon.kill_all();
    }

    fn stand_in_agent(stubborn_spawns: &[usize]) -> (tempfile::TempDir, Option<String>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sh");
        let stubborn: Vec<String> = stubborn_spawns.iter().map(|n| n.to_string()).collect();
        let script = format!(
            r#"d=$(dirname "$0"); i=1
while ! mkdir "$d/spawn-$i" 2>/dev/null; do i=$((i+1)); done
case " {} " in *" $i "*) trap '' HUP; exec sleep 2;; esac
exec sleep 60
"#,
            stubborn.join(" ")
        );
        std::fs::write(&path, script).unwrap();
        let cmd = format!("/bin/sh {}", path.display());
        (dir, Some(cmd))
    }

    fn pid_of(daemon: &Daemon, sref: &SessionRef) -> Option<u32> {
        daemon
            .session_pids()
            .into_iter()
            .find_map(|(s, pid)| (&s == sref).then_some(pid))
    }

    async fn warm_pid(daemon: &Arc<Daemon>, primary: &WorktreeId) -> u32 {
        daemon
            .prewarm_agent(primary, AgentKind::Claude, None, None)
            .await
            .unwrap();
        let pids = daemon.session_pids();
        assert_eq!(pids.len(), 1, "one warm PTY: {pids:?}");
        until_running_sleep(pids[0].1).await;
        pids[0].1
    }

    async fn create_on(daemon: &Arc<Daemon>, primary: &WorktreeId, name: &str) -> SessionRef {
        let EntityId::Agent(id) = daemon
            .create_agent(
                primary,
                None,
                name,
                AgentKind::Claude,
                None,
                None,
                false,
                None,
                WriterClaim::Off,
            )
            .await
            .unwrap()
        else {
            panic!()
        };
        SessionRef::Agent(id)
    }

    async fn spawn_writer(
        daemon: &Arc<Daemon>,
        checkout: &WorktreeId,
        name: &str,
        writer: WriterClaim,
    ) -> Result<AgentId> {
        let id = daemon
            .create_agent(
                checkout,
                None,
                name,
                AgentKind::Claude,
                None,
                None,
                false,
                None,
                writer,
            )
            .await?;
        let EntityId::Agent(id) = id else { panic!() };
        Ok(id)
    }

    fn agent_row(daemon: &Daemon, id: &AgentId) -> Agent {
        let ServerEvent::Snapshot { agents, .. } = daemon.snapshot().unwrap() else {
            unreachable!()
        };
        agents.into_iter().find(|a| &a.id == id).unwrap()
    }

    async fn checkout_with_live_writer() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        Arc<Daemon>,
        WorktreeId,
        AgentId,
    ) {
        let (script, agent) = stand_in_agent(&[]);
        let (tmp, _repo, daemon, _pid, primary) =
            primary_with_feat_on(test_daemon_with(DaemonOverrides {
                agent_cmd: agent,
                ..DaemonOverrides::default()
            }))
            .await;
        let holder = spawn_writer(&daemon, &primary.id, "holder", WriterClaim::Exclusive)
            .await
            .unwrap();
        (tmp, script, daemon, primary.id, holder)
    }

    #[tokio::test]
    async fn a_second_writer_is_refused_while_the_holder_lives() {
        let (_tmp, _script, daemon, checkout, _holder) = checkout_with_live_writer().await;
        let err = spawn_writer(&daemon, &checkout, "second", WriterClaim::Exclusive)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "\"holder\" is the writer on main; wait for it, archive it, or pass --force-writer after the user approves two writers"
        );
        let ServerEvent::Snapshot { agents, .. } = daemon.snapshot().unwrap() else {
            unreachable!()
        };
        assert!(
            agents.iter().all(|a| a.name != "second"),
            "a refused spawn leaves no row"
        );
        daemon.kill_all();
    }

    #[tokio::test]
    async fn a_forced_writer_spawns_beside_the_live_holder() {
        let (_tmp, _script, daemon, checkout, _holder) = checkout_with_live_writer().await;
        let second = spawn_writer(&daemon, &checkout, "second", WriterClaim::Forced)
            .await
            .unwrap();
        assert!(agent_row(&daemon, &second).writer);
        daemon.kill_all();
    }

    #[tokio::test]
    async fn archiving_the_holder_frees_the_writer_slot() {
        let (_tmp, _script, daemon, checkout, holder) = checkout_with_live_writer().await;
        daemon.archive_agent(&holder).unwrap();
        spawn_writer(&daemon, &checkout, "next", WriterClaim::Exclusive)
            .await
            .unwrap();
        daemon.kill_all();
    }

    #[tokio::test]
    async fn an_unarchived_holder_comes_back_without_the_writer_mark() {
        let (_tmp, _script, daemon, checkout, holder) = checkout_with_live_writer().await;
        daemon.archive_agent(&holder).unwrap();
        let next = spawn_writer(&daemon, &checkout, "next", WriterClaim::Exclusive)
            .await
            .unwrap();
        daemon.unarchive_agent(&holder).unwrap();
        assert!(!agent_row(&daemon, &holder).writer);
        assert!(agent_row(&daemon, &next).writer);
        daemon.kill_all();
    }

    #[tokio::test]
    async fn a_spawn_without_writer_ignores_the_holder() {
        let (_tmp, _script, daemon, checkout, _holder) = checkout_with_live_writer().await;
        let reviewer = spawn_writer(&daemon, &checkout, "reviewer", WriterClaim::Off)
            .await
            .unwrap();
        assert!(!agent_row(&daemon, &reviewer).writer);
        daemon.kill_all();
    }

    #[tokio::test]
    async fn a_holder_mid_turn_keeps_the_slot_across_a_daemon_restart() {
        for (turn, status) in [
            (HookEvent::UserPromptSubmit, AgentStatus::Running),
            (HookEvent::PermissionRequest, AgentStatus::NeedsFeedback),
        ] {
            let (_tmp, _script, daemon, checkout, holder) = checkout_with_live_writer().await;
            daemon.apply_hook_event(&holder, turn, None);
            assert_eq!(agent_row(&daemon, &holder).status, status);
            daemon.kill_all();
            daemon.store.sweep_disconnected().unwrap();
            let restarted = Daemon::with_overrides(
                daemon.store.clone(),
                HookEnv {
                    port: 0,
                    token: String::new(),
                },
                DaemonOverrides::default(),
            );
            assert_eq!(
                agent_row(&restarted, &holder).status,
                AgentStatus::Disconnected
            );
            let err = spawn_writer(&restarted, &checkout, "second", WriterClaim::Exclusive)
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("\"holder\" is the writer on main"),
                "{status:?}: {err}"
            );
            restarted.kill_all();
        }
    }

    #[tokio::test]
    async fn a_healthy_warm_pty_is_adopted_and_a_timed_out_one_is_waited_for_by_the_retry() {
        let (_script, agent) = stand_in_agent(&[2]);
        let (_tmp, repo, daemon, pid, primary) =
            primary_with_feat_on(test_daemon_with(DaemonOverrides {
                stop_deadline: Duration::from_millis(1500),
                agent_cmd: agent,
                ..DaemonOverrides::default()
            }))
            .await;

        let healthy = warm_pid(&daemon, &primary.id).await;
        let adopted = create_on(&daemon, &primary.id, "adopted").await;
        assert_eq!(
            pid_of(&daemon, &adopted),
            Some(healthy),
            "the warm PTY was adopted"
        );
        daemon.kill_session(&adopted);

        let stopping = warm_pid(&daemon, &primary.id).await;
        let err = daemon
            .checkout_primary(&pid, "feat", None, false)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "a warm agent PTY still running; primary stays on \"main\""
        );
        let cold = create_on(&daemon, &primary.id, "cold").await;
        assert_ne!(
            pid_of(&daemon, &cold),
            Some(stopping),
            "adopted a PTY being stopped"
        );
        daemon.kill_session(&cold);

        daemon
            .checkout_primary(&pid, "feat", None, false)
            .await
            .unwrap();
        assert!(
            !process_alive(stopping),
            "the retry waited for the warm PTY"
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "feat");
    }

    #[tokio::test]
    async fn a_resume_that_falls_back_publishes_the_respawned_agent() {
        let (_script, agent) = stand_in_agent(&[]);
        let (_tmp, repo, daemon, _pid, primary) =
            primary_with_feat_on(test_daemon_with(DaemonOverrides {
                agent_cmd: agent,
                ..DaemonOverrides::default()
            }))
            .await;
        seed_agent(&daemon, "stale", primary.id.as_str(), Some("gone"));
        let id = AgentId("stale".into());
        daemon
            .store
            .set_agent_status(&id, AgentStatus::Disconnected)
            .unwrap();
        let row = daemon.store.get_agent(&id).unwrap().unwrap();
        let agent = SessionRef::Agent(id.clone());
        spawn_live(&daemon, agent.clone(), &repo, "sleep 0.3; exit 1");
        let mut events = daemon.events.subscribe();
        daemon.arm_resume_fallback(row, primary, daemon.session(&agent).unwrap(), 80, 24);

        let upsert = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(ServerEvent::EntityUpserted {
                    entity: Entity::Agent(a),
                }) = events.recv().await
                {
                    if a.id == id && a.alive {
                        return (a.status, a.alive);
                    }
                }
            }
        })
        .await
        .expect("the fallback respawned the agent");
        daemon.kill_all();
        assert_eq!(upsert, (AgentStatus::Fresh, true));
    }

    #[tokio::test]
    async fn a_stopped_session_reaches_subscribers_as_parked_and_not_alive() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_agent(&daemon, "runner", primary.id.as_str(), Some("conversation"));
        seed_terminal(&daemon, "shell", primary.id.as_str());
        spawn_live(
            &daemon,
            SessionRef::Agent(AgentId("runner".into())),
            &repo,
            "exec sleep 60",
        );
        spawn_live(
            &daemon,
            SessionRef::Terminal(TerminalId("shell".into())),
            &repo,
            "exec sleep 60",
        );
        let mut events = daemon.events.subscribe();

        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let (mut agent, mut tab, mut statuses) = (None, None, Vec::new());
        while let Ok(event) = events.try_recv() {
            match event {
                ServerEvent::EntityUpserted {
                    entity: Entity::Agent(a),
                } if a.id.as_str() == "runner" => agent = Some((a.alive, a.status)),
                ServerEvent::EntityUpserted {
                    entity: Entity::Terminal(t),
                } if t.id.as_str() == "shell" => tab = Some(t.alive),
                ServerEvent::StatusChanged { agent, status, .. } if agent.as_str() == "runner" => {
                    statuses.push(status)
                }
                _ => {}
            }
        }
        assert_eq!(agent, Some((false, AgentStatus::Disconnected)));
        assert_eq!(tab, Some(false));
        assert_eq!(statuses, [AgentStatus::Disconnected]);
    }

    #[tokio::test]
    async fn a_session_outlasting_the_deadline_refuses_the_switch_and_the_others_are_still_parked()
    {
        let (_tmp, repo, daemon, pid, primary) =
            primary_with_feat_on(test_daemon_stopping(Duration::from_secs(1))).await;
        seed_agent(&daemon, "done", primary.id.as_str(), Some("conversation"));
        daemon
            .store
            .set_agent_status(&AgentId("done".into()), AgentStatus::Finished)
            .unwrap();
        seed_terminal(&daemon, "stubborn", primary.id.as_str());
        let done = SessionRef::Agent(AgentId("done".into()));
        let stubborn = SessionRef::Terminal(TerminalId("stubborn".into()));
        spawn_live(&daemon, done.clone(), &repo, "exec sleep 60");
        let stubborn_pid = spawn_live(
            &daemon,
            stubborn.clone(),
            &repo,
            "trap '' HUP; exec sleep 1.5",
        );

        let err = daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "stubborn (terminal) still running; primary stays on \"main\""
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "main");
        let ServerEvent::Snapshot {
            agents, terminals, ..
        } = daemon.snapshot().unwrap()
        else {
            panic!()
        };
        let parked = agents.iter().find(|a| a.id.as_str() == "done").unwrap();
        assert!(!parked.alive);
        assert_eq!(parked.branch.as_deref(), Some("main"));
        let survivor = terminals
            .iter()
            .find(|t| t.id.as_str() == "stubborn")
            .unwrap();
        assert!(survivor.alive, "a survivor stays visible as alive");

        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();
        assert!(
            !process_alive(stubborn_pid),
            "the retry waited for the survivor"
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "feat");
    }

    #[tokio::test]
    async fn every_parked_agent_reads_stopped_and_ignores_late_hooks_until_respawned() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        for (id, status) in [
            ("fresh", AgentStatus::Fresh),
            ("done", AgentStatus::Finished),
        ] {
            seed_agent(&daemon, id, primary.id.as_str(), Some(id));
            daemon
                .store
                .set_agent_status(&AgentId(id.into()), status)
                .unwrap();
            spawn_live(
                &daemon,
                SessionRef::Agent(AgentId(id.into())),
                &repo,
                "exec sleep 60",
            );
        }

        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();

        for id in ["fresh", "done"] {
            assert_eq!(agent_status(&daemon, id), AgentStatus::Disconnected, "{id}");
            for late in [
                HookEvent::UserPromptSubmit,
                HookEvent::PermissionRequest,
                HookEvent::Stop,
            ] {
                daemon.apply_hook_event(&AgentId(id.into()), late.clone(), Some(id.into()));
                assert_eq!(
                    agent_status(&daemon, id),
                    AgentStatus::Disconnected,
                    "{id} after a late {late:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_finished_agent_and_an_idle_shell_need_confirmation_and_stay_on_their_branch() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_agent(&daemon, "done", primary.id.as_str(), Some("conversation"));
        daemon
            .store
            .set_agent_status(&AgentId("done".into()), AgentStatus::Finished)
            .unwrap();
        seed_terminal(&daemon, "shell", primary.id.as_str());
        let agent = SessionRef::Agent(AgentId("done".into()));
        let shell = SessionRef::Terminal(TerminalId("shell".into()));
        let agent_pid = spawn_live(&daemon, agent.clone(), &repo, "exec sleep 60");
        let shell_pid = spawn_live(&daemon, shell.clone(), &repo, "exec sleep 60");

        let err = daemon
            .checkout_primary(&pid, "feat", None, false)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "primary has sessions on \"main\"; switching to \"feat\" stops done (agent, finished), shell (terminal)"
        );
        assert!(daemon.is_alive(&agent) && daemon.is_alive(&shell));
        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();

        assert_eq!(git::current_branch(&repo).await.unwrap(), "feat");
        assert!(!daemon.is_alive(&agent) && !daemon.is_alive(&shell));
        assert!(
            process_exits(agent_pid).await,
            "agent process still running"
        );
        assert!(
            process_exits(shell_pid).await,
            "shell process still running"
        );
        let ServerEvent::Snapshot {
            agents, terminals, ..
        } = daemon.snapshot().unwrap()
        else {
            panic!()
        };
        let done = agents.iter().find(|a| a.id.as_str() == "done").unwrap();
        assert_eq!(done.branch.as_deref(), Some("main"));
        assert_eq!(done.session_id.as_deref(), Some("conversation"));
        assert!(!done.archived && !done.alive);
        let tab = terminals.iter().find(|t| t.id.as_str() == "shell").unwrap();
        assert_eq!(tab.branch.as_deref(), Some("main"));
        assert!(!tab.alive);
    }

    #[tokio::test]
    async fn a_shell_running_a_command_needs_confirmation_before_the_switch() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_terminal(&daemon, "build", primary.id.as_str());
        let shell = SessionRef::Terminal(TerminalId("build".into()));
        let shell_pid = spawn_live(&daemon, shell.clone(), &repo, "sleep 60; true");
        tokio::time::sleep(Duration::from_millis(200)).await;

        let err = daemon
            .checkout_primary(&pid, "feat", None, false)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "primary has sessions on \"main\"; switching to \"feat\" stops build (terminal)"
        );
        assert!(daemon.is_alive(&shell));

        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();
        assert!(!process_alive(shell_pid));
        assert_eq!(git::current_branch(&repo).await.unwrap(), "feat");
    }

    #[tokio::test]
    async fn a_running_agent_needs_confirmation_before_the_switch() {
        let (_tmp, repo, daemon, pid, primary) = primary_with_feat().await;
        seed_agent(&daemon, "runner", primary.id.as_str(), Some("conversation"));
        let agent = SessionRef::Agent(AgentId("runner".into()));
        let agent_pid = spawn_live(&daemon, agent.clone(), &repo, "exec sleep 60");
        seed_terminal(&daemon, "shell", primary.id.as_str());
        let shell = SessionRef::Terminal(TerminalId("shell".into()));
        spawn_live(&daemon, shell.clone(), &repo, "exec sleep 60");

        let err = daemon
            .checkout_primary(&pid, "feat", None, false)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "primary has sessions on \"main\"; switching to \"feat\" stops runner (agent, running), shell (terminal)"
        );
        assert!(daemon.is_alive(&agent) && daemon.is_alive(&shell));
        assert_eq!(git::current_branch(&repo).await.unwrap(), "main");
        let err = daemon
            .resolve_branch_checkout(&pid, "feat")
            .await
            .unwrap_err();
        assert!(err.to_string().starts_with(BUSY_SESSIONS_REFUSAL), "{err}");
        assert!(daemon.is_alive(&agent));

        daemon
            .checkout_primary(&pid, "feat", None, true)
            .await
            .unwrap();
        assert_eq!(git::current_branch(&repo).await.unwrap(), "feat");
        assert!(
            process_exits(agent_pid).await,
            "agent process still running"
        );
        let ServerEvent::Snapshot { agents, .. } = daemon.snapshot().unwrap() else {
            panic!()
        };
        let runner = agents.iter().find(|a| a.id.as_str() == "runner").unwrap();
        assert_eq!(runner.session_id.as_deref(), Some("conversation"));
        assert_eq!(runner.branch.as_deref(), Some("main"));
        assert!(!runner.archived);
    }

    #[tokio::test]
    async fn create_worktree_seeds_the_primary_env_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), ".env*\nnode_modules/\n").unwrap();
        git_in(&repo, &["add", ".gitignore"]);
        git_in(&repo, &["commit", "-q", "-m", "init"]);
        std::fs::write(repo.join(".env"), "A=1").unwrap();
        std::fs::create_dir_all(repo.join("apps/web")).unwrap();
        std::fs::write(repo.join("apps/web/.env.local"), "B=2").unwrap();
        std::fs::create_dir_all(repo.join("node_modules/x")).unwrap();
        std::fs::write(repo.join("node_modules/x/.env"), "nope").unwrap();

        let daemon = test_daemon();
        let pid = ProjectId("p".into());
        daemon
            .store
            .insert_project(&Project {
                workspace_id: Default::default(),
                id: pid.clone(),
                name: "p".into(),
                repo_path: repo.clone(),
                sort_order: 0,
                divider_after: false,
                divider_before: false,
                divider_label: None,
                divider_before_label: None,
                host: None,
            })
            .unwrap();
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("root".into()),
                project_id: pid.clone(),
                path: repo.clone(),
                branch: "main".into(),
                is_main: true,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();
        daemon.create_worktree(&pid, "feat", None).await.unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let wt = worktrees.iter().find(|w| w.branch == "feat").unwrap();
        assert_eq!(
            std::fs::read_to_string(wt.path.join(".env")).unwrap(),
            "A=1"
        );
        assert_eq!(
            std::fs::read_to_string(wt.path.join("apps/web/.env.local")).unwrap(),
            "B=2"
        );
        assert!(
            !wt.path.join("node_modules").exists(),
            "dependency dirs stay out"
        );
    }

    #[tokio::test]
    async fn create_worktree_derives_a_base_when_none_is_given() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "dad", "main"]);

        let daemon = test_daemon();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId("p".into()),
            name: "p".into(),
            repo_path: repo.clone(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        daemon.store.insert_project(&project).unwrap();

        // Existing branch, no base passed (the agent-on-branch path).
        daemon
            .create_worktree(&ProjectId("p".into()), "dad", None)
            .await
            .unwrap();
        // Fresh branch, no base passed: cut from the root HEAD (main).
        daemon
            .create_worktree(&ProjectId("p".into()), "fresh", None)
            .await
            .unwrap();

        // Pinning the primary's commit must retain the same display parent,
        // including when multiple sibling task branches already share that tip.
        let sha = git::branch_tip(&repo, "main").await.unwrap();
        for task in ["pinned-one", "pinned-two"] {
            daemon
                .create_worktree(&project.id, task, Some(&sha))
                .await
                .unwrap();
        }
        daemon
            .create_worktree(&project.id, "explicit-dad", Some("dad"))
            .await
            .unwrap();
        git_in(&repo, &["commit", "--allow-empty", "-m", "advance"]);
        daemon
            .create_worktree(&project.id, "historical", Some(&sha))
            .await
            .unwrap();

        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let base = |branch: &str| {
            worktrees
                .iter()
                .find(|w| w.branch == branch)
                .unwrap()
                .created_from
                .clone()
        };
        assert_eq!(base("dad").as_deref(), Some("main"));
        assert_eq!(base("fresh").as_deref(), Some("main"));
        assert_eq!(base("pinned-one").as_deref(), Some("main"));
        assert_eq!(base("pinned-two").as_deref(), Some("main"));
        assert_eq!(base("explicit-dad").as_deref(), Some("dad"));
        assert_eq!(base("historical").as_deref(), Some(sha.as_str()));
        assert_eq!(
            git::branch_tip(&repo, "pinned-one").await.as_deref(),
            Some(sha.as_str())
        );
        // Identity: checking out a pre-existing branch keeps it a branch;
        // a branch minted by the worktree flow is a worktree.
        let for_branch = |branch: &str| {
            worktrees
                .iter()
                .find(|w| w.branch == branch)
                .unwrap()
                .for_branch
        };
        assert!(for_branch("dad"), "existing branch keeps branch identity");
        assert!(!for_branch("fresh"), "minted branch is a worktree");
    }

    /// An in-place `checkout -b` inside a known checkout: the sync flips
    /// the row to the new branch, marks it a branch row (`for_branch`),
    /// and — though git logged only `Created from HEAD` — records the
    /// branch the checkout sat on as the base. Deleting that row then
    /// deletes the BRANCH: the checkout reverts to the base and the row
    /// survives as the worktree it was.
    #[tokio::test]
    async fn in_place_checkout_b_presents_and_deletes_as_a_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "dad", "main"]);
        let dad = root.join("repo-worktrees").join("dad");
        git_in(&repo, &["worktree", "add", &dad.to_string_lossy(), "dad"]);

        let daemon = test_daemon();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId("p".into()),
            name: "p".into(),
            repo_path: repo.clone(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        daemon.store.insert_project(&project).unwrap();
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("wt-dad".into()),
                project_id: ProjectId("p".into()),
                path: dad.clone(),
                branch: "dad".into(),
                is_main: false,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();

        git_in(&dad, &["checkout", "-b", "feat"]);
        daemon.sync_project_worktrees(&project).await.unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let row = worktrees
            .iter()
            .find(|w| w.id == WorktreeId("wt-dad".into()))
            .unwrap();
        assert_eq!(row.branch, "feat", "row tracks the in-place switch");
        assert!(
            row.for_branch,
            "a branch made in the terminal stays a branch"
        );
        assert_eq!(
            row.created_from.as_deref(),
            Some("dad"),
            "implicit-HEAD creation links back to the branch the checkout sat on"
        );

        daemon
            .delete_worktree(&WorktreeId("wt-dad".into()), true)
            .await
            .unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let row = worktrees
            .iter()
            .find(|w| w.id == WorktreeId("wt-dad".into()))
            .expect("the pre-existing checkout's row survives a branch delete");
        assert_eq!(row.branch, "dad", "checkout reverted to the base");
        assert!(!row.for_branch, "back to the worktree it was");
        assert_eq!(row.created_from, None, "the base described the dead branch");
        assert!(dad.exists(), "the checkout stays on disk");
        let feat = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--list", "feat"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&feat.stdout).trim().is_empty(),
            "the branch itself is gone"
        );
    }

    /// The in-place branch delete on a DIRTY checkout: uncommitted changes
    /// that conflict with the base make the plain revert refuse; a forced
    /// delete (the confirm dialog warned) retries with `checkout -f`.
    #[tokio::test]
    async fn forced_in_place_branch_delete_discards_conflicting_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "dad", "main"]);
        let dad = root.join("repo-worktrees").join("dad");
        git_in(&repo, &["worktree", "add", &dad.to_string_lossy(), "dad"]);

        let daemon = test_daemon();
        let project = Project {
            workspace_id: Default::default(),
            id: ProjectId("p".into()),
            name: "p".into(),
            repo_path: repo.clone(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: None,
        };
        daemon.store.insert_project(&project).unwrap();
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("wt-dad".into()),
                project_id: ProjectId("p".into()),
                path: dad.clone(),
                branch: "dad".into(),
                is_main: false,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();

        git_in(&dad, &["checkout", "-b", "feat"]);
        daemon.sync_project_worktrees(&project).await.unwrap();
        // A file committed on feat, then edited again: reverting to dad
        // (which lacks it) conflicts, so a plain `git checkout` refuses.
        std::fs::write(dad.join("f.txt"), "committed").unwrap();
        git_in(&dad, &["add", "f.txt"]);
        git_in(&dad, &["commit", "-m", "feat work"]);
        std::fs::write(dad.join("f.txt"), "uncommitted").unwrap();

        daemon
            .delete_worktree(&WorktreeId("wt-dad".into()), true)
            .await
            .unwrap();
        let (_, worktrees, _, _) = daemon.store.load_tree().unwrap();
        let row = worktrees
            .iter()
            .find(|w| w.id == WorktreeId("wt-dad".into()))
            .expect("the pre-existing checkout's row survives");
        assert_eq!(row.branch, "dad", "forced revert landed on the base");
        assert!(
            !dad.join("f.txt").exists(),
            "the conflicting uncommitted file went with the branch"
        );
    }

    /// The replay is scoped to the synced project and skips archived rows.
    #[test]
    fn cwd_replay_skips_other_projects_and_archived_agents() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p", "q"]);
        seed_worktree(&daemon, "p", "p-root", "/nebula-test/p", true);
        seed_worktree(&daemon, "q", "q-root", "/nebula-test/q", true);
        seed_worktree(&daemon, "q", "q-feat", "/nebula-test/q-feat", false);
        seed_agent(&daemon, "a1", "q-root", None);
        seed_agent(&daemon, "a2", "q-root", None);

        // Both agents report a cwd inside q-feat before it exists...
        daemon
            .store
            .delete_worktree(&WorktreeId("q-feat".into()))
            .unwrap();
        daemon.reparent_agent_by_cwd(&AgentId("a1".into()), "/nebula-test/q-feat", None, false);
        daemon.reparent_agent_by_cwd(&AgentId("a2".into()), "/nebula-test/q-feat", None, false);
        seed_worktree(&daemon, "q", "q-feat", "/nebula-test/q-feat", false);

        // ...but a replay for project p touches neither.
        let p = daemon
            .store
            .get_project(&ProjectId("p".into()))
            .unwrap()
            .unwrap();
        daemon.reparent_agents_by_last_cwd(&p);
        assert_eq!(agent_worktree(&daemon, "a1"), "q-root");

        // Archived agents stay put; live ones re-home.
        daemon
            .store
            .set_agent_archived(&AgentId("a2".into()), true)
            .unwrap();
        let q = daemon
            .store
            .get_project(&ProjectId("q".into()))
            .unwrap()
            .unwrap();
        daemon.reparent_agents_by_last_cwd(&q);
        assert_eq!(agent_worktree(&daemon, "a1"), "q-feat");
        assert_eq!(agent_worktree(&daemon, "a2"), "q-root");
    }

    #[tokio::test]
    async fn branch_checkout_resolution_switches_primary_and_refuses_parked_branches() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "feature"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "manual"]);
        std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        git_in(&repo, &["add", ".gitignore"]);
        git_in(&repo, &["commit", "-m", "ignore env"]);
        git_in(&repo, &["branch", "main"]);
        std::fs::write(repo.join(".env"), "ISOLATION=ready\n").unwrap();
        std::fs::write(repo.join("dirty"), "preserved").unwrap();
        let manual = tmp.path().join("custom-checkout");
        git_in(
            &repo,
            &["worktree", "add", manual.to_str().unwrap(), "manual"],
        );
        let daemon = test_daemon();
        let EntityId::Project(project) =
            daemon.add_project(&repo, None, false, None).await.unwrap()
        else {
            panic!()
        };
        let EntityId::Worktree(primary) = daemon
            .resolve_branch_checkout(&project, "feature")
            .await
            .unwrap()
        else {
            panic!()
        };
        let (left, right) = tokio::join!(
            daemon.resolve_branch_checkout(&project, "main"),
            daemon.resolve_branch_checkout(&project, "main"),
        );
        let id = left.unwrap();
        assert_eq!(id, right.unwrap(), "racing callers reuse one checkout");
        let EntityId::Worktree(main) = id else {
            panic!()
        };
        assert_eq!(main, primary);
        let err = daemon
            .resolve_branch_checkout(&project, "manual")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already has a checkout"), "{err}");
        let ServerEvent::Snapshot { worktrees, .. } = daemon.snapshot().unwrap() else {
            panic!()
        };
        let main_path = &worktrees.iter().find(|w| w.id == main).unwrap().path;
        assert_eq!(worktrees.len(), 2);
        assert_eq!(git::list_worktrees(&repo).await.unwrap().len(), 2);
        assert_eq!(
            worktrees.iter().find(|w| w.id == primary).unwrap().path,
            repo.canonicalize().unwrap()
        );
        assert_eq!(main_path, &repo.canonicalize().unwrap());
        assert_eq!(
            std::fs::read_to_string(main_path.join(".env")).unwrap(),
            "ISOLATION=ready\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("dirty")).unwrap(),
            "preserved"
        );
        assert!(daemon
            .resolve_branch_checkout(&project, "does-not-exist")
            .await
            .is_err());
        assert!(!git::branch_exists(&repo, "does-not-exist").await);
        assert_eq!(git::current_branch(&repo).await.unwrap(), "main");
        assert!(!git::worktree_dir(&repo, "main").exists());
        assert!(git::branch_exists(&repo, "main").await);
        assert!(manual.exists());
        // The same-named branch in another host/project never shares an identity.
        let mut remote = daemon.store.get_project(&project).unwrap().unwrap();
        remote.id = ProjectId("offline-host".into());
        remote.host = Some("findl".into());
        remote.repo_path = tmp.path().join("offline/repo");
        daemon.store.insert_project(&remote).unwrap();
        let err = daemon
            .resolve_branch_checkout(&remote.id, "feature")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("host checkout is unavailable"));
    }

    #[tokio::test]
    async fn legacy_resume_requires_recovery_and_a_moving_shell_keeps_its_pty() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git_in(&repo, &["branch", "feature"]);
        let db = tmp.path().join("state.sqlite");
        let daemon = Daemon::new(
            Arc::new(Store::open(&db).unwrap()),
            HookEnv {
                port: 0,
                token: String::new(),
            },
        );
        let EntityId::Project(project) =
            daemon.add_project(&repo, None, false, None).await.unwrap()
        else {
            panic!()
        };
        let EntityId::Worktree(primary) = daemon
            .resolve_branch_checkout(&project, "main")
            .await
            .unwrap()
        else {
            panic!()
        };
        // Deterministic inter-request race: resolution selected main, an explicit
        // switch ran before creation. Branch-bound creation must reject the new HEAD.
        daemon
            .checkout_primary(&project, "feature", None, false)
            .await
            .unwrap();
        let err = daemon
            .create_terminal(&primary, Some("main"), None)
            .unwrap_err();
        assert!(err.to_string().contains("expects branch \"main\""));
        let err = daemon
            .create_agent(
                &primary,
                Some("main"),
                "racer",
                AgentKind::Claude,
                None,
                None,
                false,
                None,
                WriterClaim::Off,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expects branch \"main\""));
        // Explicit physical selectors retain their documented meaning.
        let EntityId::Terminal(physical) = daemon.create_terminal(&primary, None, None).unwrap()
        else {
            panic!()
        };
        daemon.close_terminal(&physical).unwrap();
        daemon
            .checkout_primary(&project, "main", None, false)
            .await
            .unwrap();
        seed_agent(
            &daemon,
            "legacy",
            primary.as_str(),
            Some("saved-conversation"),
        );
        daemon
            .store
            .set_agent_branch(&AgentId("legacy".into()), Some("main"))
            .unwrap();
        seed_terminal(&daemon, "legacy-shell", primary.as_str());
        daemon
            .store
            .set_terminal_branch(&TerminalId("legacy-shell".into()), Some("main"))
            .unwrap();
        // Persisted metadata can disagree with disk after external Git operations.
        git_in(&repo, &["checkout", "feature"]);
        drop(daemon);
        let daemon = Daemon::new(
            Arc::new(Store::open(&db).unwrap()),
            HookEnv {
                port: 0,
                token: String::new(),
            },
        );
        for session in [
            SessionRef::Agent(AgentId("legacy".into())),
            SessionRef::Terminal(TerminalId("legacy-shell".into())),
        ] {
            let err = daemon
                .ensure_session(&session, 80, 24)
                .err()
                .expect("resume must refuse mismatched files");
            assert!(err.to_string().contains("expects branch \"main\""), "{err}");
            assert!(!daemon.is_alive(&session));
        }
        let err = daemon.restart_agent(&AgentId("legacy".into())).unwrap_err();
        assert!(err.to_string().contains("expects branch \"main\""), "{err}");
        let ServerEvent::Snapshot { agents, .. } = daemon.snapshot().unwrap() else {
            panic!()
        };
        assert_eq!(agents[0].session_id.as_deref(), Some("saved-conversation"));
        // Explicitly restoring the files makes a stopped shell resumable again.
        git_in(&repo, &["checkout", "main"]);
        let id = TerminalId("idle".into());
        seed_terminal(&daemon, id.as_str(), primary.as_str());
        let idle = SessionRef::Terminal(id.clone());
        spawn_live(&daemon, idle.clone(), &repo, "exec sleep 60");
        let refusals = [
            daemon
                .checkout_primary(&project, "feature", None, false)
                .await
                .unwrap_err(),
            daemon
                .resolve_branch_checkout(&project, "feature")
                .await
                .unwrap_err(),
        ];
        for err in refusals {
            assert_eq!(
                err.to_string(),
                "primary has sessions on \"main\"; switching to \"feature\" stops idle (terminal)"
            );
        }
        assert!(daemon.is_alive(&idle));
        assert_eq!(git::current_branch(&repo).await.unwrap(), "main");
        daemon
            .checkout_primary(&project, "feature", None, true)
            .await
            .unwrap();
        assert!(
            !daemon.is_alive(&idle),
            "the confirmed switch stopped the idle shell"
        );
        assert_eq!(git::current_branch(&repo).await.unwrap(), "feature");
        assert_eq!(git::list_worktrees(&repo).await.unwrap().len(), 1);
        assert!(!git::worktree_dir(&repo, "feature").exists());
        daemon.close_terminal(&id).unwrap();
        daemon
            .checkout_primary(&project, "main", None, false)
            .await
            .unwrap();
        let shell = SessionRef::Terminal(TerminalId("legacy-shell".into()));
        let live = daemon.ensure_session(&shell, 80, 24).unwrap();
        // A sole calling shell may deliberately move itself and keep its PTY.
        daemon
            .checkout_primary(&project, "feature", Some(&shell), false)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(
            &live,
            &daemon.ensure_session(&shell, 80, 24).unwrap()
        ));
        let ServerEvent::Snapshot { terminals, .. } = daemon.snapshot().unwrap() else {
            panic!()
        };
        assert_eq!(terminals[0].branch.as_deref(), Some("feature"));
        daemon
            .close_terminal(&TerminalId("legacy-shell".into()))
            .unwrap();
    }

    fn git_in(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn reparent_by_cwd_ignores_foreign_sessions_unless_capturing() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p"]);
        seed_worktree(&daemon, "p", "root", "/nebula-test/p", true);
        seed_worktree(&daemon, "p", "feat", "/nebula-test/p-feat", false);
        seed_agent(&daemon, "a1", "root", Some("s1"));

        // A different session id on a non-capturing event (a nested claude
        // launched inside the agent's PTY) must not move the row.
        daemon.reparent_agent_by_cwd(
            &AgentId("a1".into()),
            "/nebula-test/p-feat",
            Some("s2"),
            false,
        );
        assert_eq!(agent_worktree(&daemon, "a1"), "root");

        // A capturing event (re)establishes ownership, so it may move it.
        daemon.reparent_agent_by_cwd(
            &AgentId("a1".into()),
            "/nebula-test/p-feat",
            Some("s2"),
            true,
        );
        assert_eq!(agent_worktree(&daemon, "a1"), "feat");
    }

    #[test]
    fn agent_command_children_are_shells_and_helpers_are_not() {
        // Command lines seen under live agents (`ps -o args=`), 2026-10-22.
        for command in [
            "/bin/zsh -c source /Users/u/.claude/shell-snapshots/snapshot-zsh-1792669510354-vg2mom.sh 2>/dev/null || true",
            "/bin/zsh -c sleep 40 && echo probe-ok",
            "/bin/bash -lc cargo test",
            "sh -c sleep 30; true",
        ] {
            assert!(is_shell_command(command.split_whitespace()), "{command}");
        }
        for helper in [
            "caffeinate -i -t 300",
            "npm exec mcp-remote https://mcp.exa.ai/mcp",
            "node /Users/u/.nvm/versions/node/v22.23.2/bin/rea mcp",
            "/Applications/ChatGPT.app/Contents/Resources/cua_node/bin/node_repl",
            "/opt/homebrew/Caskroom/codex/0.162.1/bin/codex-code-mode-host",
            "/bin/zsh -l",
            "bash --login",
            "sleep 30",
        ] {
            assert!(!is_shell_command(helper.split_whitespace()), "{helper}");
        }
    }

    #[test]
    fn nebula_hook_delivery_is_not_a_background_command() {
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"] {
            let hook = crate::hooks::installer::hook_command("claude", "Stop");
            let args = format!("{shell} -c {hook}");
            assert!(!is_background_command(&args), "{args}");
        }
        assert!(is_background_command(
            "/bin/zsh -c sleep 40 && echo probe-ok"
        ));
    }

    #[test]
    fn normalize_url_adds_https_and_refuses_non_links() {
        // Pasted URLs pass through untouched.
        assert_eq!(
            normalize_url("https://github.com/o/r/pull/7").unwrap(),
            "https://github.com/o/r/pull/7"
        );
        assert_eq!(normalize_url("  http://x.dev  ").unwrap(), "http://x.dev");
        // Typed hosts gain the scheme.
        assert_eq!(
            normalize_url("github.com/o/r/pull/7").unwrap(),
            "https://github.com/o/r/pull/7"
        );
        // Anything that isn't an http(s) URL is refused, so `open(1)` can
        // never be handed a scheme the user didn't intend.
        for bad in [
            "",
            "   ",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://",
            "just a note",
            "notaurl",
        ] {
            assert!(normalize_url(bad).is_err(), "expected refusal: {bad:?}");
        }
    }

    #[test]
    fn cli_missing_message_names_the_binary_not_the_kind() {
        // Cursor ships its agent as `cursor-agent`; naming the kind would
        // send the user off to install the wrong thing.
        assert!(cli_missing_message(AgentKind::Cursor).starts_with("cursor-agent was not found"));
        assert!(cli_missing_message(AgentKind::Claude).starts_with("claude was not found"));
        assert!(cli_missing_message(AgentKind::Codex).starts_with("codex was not found"));
        assert!(cli_missing_message(AgentKind::Pi).starts_with("pi was not found"));
        // No "restart nebula": agent CLIs are spawned through the user's
        // login shell, so a fresh install is picked up on the next try.
        for kind in AgentKind::ALL {
            let msg = cli_missing_message(kind);
            assert!(msg.contains("try again"), "{msg}");
            assert!(!msg.contains("restart"), "{msg}");
        }
    }

    #[test]
    fn prewarm_pool_buffers_hooks_and_drops_dead_entries() {
        let daemon = test_daemon();
        let key = (WorktreeId("w1".into()), AgentKind::Claude);
        daemon.prewarmed.lock().unwrap().insert(
            key.clone(),
            PrewarmEntry {
                agent_id: AgentId("warm-1".into()),
                spawned_at: Instant::now(),
                model: None,
                effort: None,
                buffered_hooks: Vec::new(),
            },
        );

        // Hooks for the warm (row-less) id are buffered on the entry, not
        // dropped; hooks for unrelated unknown ids still vanish quietly.
        daemon.apply_hook_event(
            &AgentId("warm-1".into()),
            HookEvent::SessionStart { source: None },
            Some("sid-9".into()),
        );
        daemon.apply_hook_event(&AgentId("stranger".into()), HookEvent::Stop, None);
        {
            let pool = daemon.prewarmed.lock().unwrap();
            let entry = pool.get(&key).unwrap();
            assert_eq!(entry.buffered_hooks.len(), 1);
            assert_eq!(
                entry.buffered_hooks[0],
                (
                    HookEvent::SessionStart { source: None },
                    Some("sid-9".to_string())
                )
            );
        }

        // The buffer is bounded: overflow drops the oldest.
        for i in 0..(PREWARM_HOOK_BUFFER_CAP + 5) {
            daemon.apply_hook_event(
                &AgentId("warm-1".into()),
                HookEvent::Notification {
                    notification_type: Some(format!("n{i}")),
                },
                None,
            );
        }
        assert_eq!(
            daemon
                .prewarmed
                .lock()
                .unwrap()
                .get(&key)
                .unwrap()
                .buffered_hooks
                .len(),
            PREWARM_HOOK_BUFFER_CAP
        );

        // No live PTY backs the entry, so take() refuses it (create falls
        // back to a cold spawn) and reap clears it out.
        assert!(daemon
            .take_prewarmed(&WorktreeId("w1".into()), AgentKind::Claude, &None, &None)
            .is_none());
        assert!(daemon.prewarmed.lock().unwrap().is_empty());

        daemon.prewarmed.lock().unwrap().insert(
            key.clone(),
            PrewarmEntry {
                agent_id: AgentId("warm-2".into()),
                spawned_at: Instant::now(),
                model: None,
                effort: None,
                buffered_hooks: Vec::new(),
            },
        );
        daemon.reap_prewarmed();
        assert!(daemon.prewarmed.lock().unwrap().is_empty());
    }

    #[test]
    fn auto_archive_takes_only_stale_idle_unpinned_unattached_agents() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p"]);
        seed_worktree(&daemon, "p", "w1", "/nebula-test/p", true);
        let day_ms = 24 * 3_600_000;
        let old = epoch_ms() - day_ms - 60_000;
        let recent = epoch_ms() - 60_000;
        let rows = [
            ("old-finished", AgentStatus::Finished, old, false),
            ("old-fresh", AgentStatus::Fresh, old, false),
            ("old-disconnected", AgentStatus::Disconnected, old, false),
            ("recent-finished", AgentStatus::Finished, recent, false),
            ("old-pinned", AgentStatus::Finished, old, true),
            ("old-running", AgentStatus::Running, old, false),
            ("old-feedback", AgentStatus::NeedsFeedback, old, false),
            ("old-attached", AgentStatus::Finished, old, false),
        ];
        for (id, status, stamp, pinned) in rows {
            daemon
                .store
                .insert_agent(&Agent {
                    id: AgentId(id.into()),
                    worktree_id: WorktreeId("w1".into()),
                    name: id.into(),
                    status,
                    archived: false,
                    archived_at: 0,
                    pinned,
                    kind: AgentKind::Claude,
                    model: None,
                    effort: None,
                    session_id: None,
                    sort_order: 0,
                    status_changed_at: stamp,
                    alive: false,
                    branch: None,
                    writer: false,
                })
                .unwrap();
        }
        daemon.note_attached(&SessionRef::Agent(AgentId("old-attached".into())));
        let archived = |d: &Daemon| -> Vec<String> {
            let mut ids: Vec<String> = d
                .store
                .load_tree()
                .unwrap()
                .2
                .into_iter()
                .filter(|a| a.archived)
                .map(|a| a.id.0)
                .collect();
            ids.sort();
            ids
        };

        daemon.sweep_stale_agents(None);
        assert!(archived(&daemon).is_empty(), "\"off\" archives nothing");

        daemon.sweep_stale_agents(Some(Duration::from_secs(24 * 3600)));
        assert_eq!(
            archived(&daemon),
            vec!["old-disconnected", "old-finished", "old-fresh"]
        );
    }

    #[tokio::test]
    async fn disabled_prewarm_drains_live_pool() {
        let daemon = test_daemon();
        let cwd = tempfile::tempdir().unwrap();
        let warm = AgentId("warm-live".into());
        let pid = spawn_live(
            &daemon,
            SessionRef::Agent(warm.clone()),
            cwd.path(),
            "exec sleep 60",
        );
        daemon.prewarmed.lock().unwrap().insert(
            (WorktreeId("w1".into()), AgentKind::Claude),
            PrewarmEntry {
                agent_id: warm.clone(),
                spawned_at: Instant::now(),
                model: None,
                effort: None,
                buffered_hooks: Vec::new(),
            },
        );

        daemon.sweep_prewarmed(true);
        assert_eq!(
            daemon.prewarmed.lock().unwrap().len(),
            1,
            "young live slot kept"
        );

        daemon.sweep_prewarmed(false);
        assert!(daemon.prewarmed.lock().unwrap().is_empty());
        assert!(process_exits(pid).await, "warm CLI killed");
    }

    #[test]
    fn kill_prewarmed_in_scopes_to_worktrees() {
        let daemon = test_daemon();
        for (wt, id) in [("w1", "a"), ("w2", "b")] {
            daemon.prewarmed.lock().unwrap().insert(
                (WorktreeId(wt.into()), AgentKind::Codex),
                PrewarmEntry {
                    agent_id: AgentId(id.into()),
                    spawned_at: Instant::now(),
                    model: None,
                    effort: None,
                    buffered_hooks: Vec::new(),
                },
            );
        }
        daemon.kill_prewarmed_in(&[WorktreeId("w1".into())]);
        let pool = daemon.prewarmed.lock().unwrap();
        assert_eq!(pool.len(), 1);
        assert!(pool.contains_key(&(WorktreeId("w2".into()), AgentKind::Codex)));
    }

    #[test]
    fn reparent_by_cwd_skips_archived_agents() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p"]);
        seed_worktree(&daemon, "p", "root", "/nebula-test/p", true);
        seed_worktree(&daemon, "p", "feat", "/nebula-test/p-feat", false);
        seed_agent(&daemon, "a1", "root", None);
        daemon
            .store
            .set_agent_archived(&AgentId("a1".into()), true)
            .unwrap();

        daemon.reparent_agent_by_cwd(&AgentId("a1".into()), "/nebula-test/p-feat", None, false);
        assert_eq!(agent_worktree(&daemon, "a1"), "root");
    }

    // ---- workspaces ----

    #[test]
    fn workspace_lifecycle_add_open_rename_delete() {
        let daemon = test_daemon();
        let EntityId::Workspace(id) = daemon.add_workspace(" client ").unwrap() else {
            panic!("add returns the workspace id");
        };
        // Name is trimmed; duplicates (trimmed) and blanks are refused.
        assert_eq!(
            daemon.store.get_workspace(&id).unwrap().unwrap().name,
            "client"
        );
        assert!(daemon.add_workspace("client").is_err());
        assert!(daemon.add_workspace("   ").is_err());

        // Adding never opens; open does (and re-opening is a quiet no-op).
        assert_eq!(
            daemon.store.active_workspace_id().unwrap().as_str(),
            "default"
        );
        daemon.open_workspace(&id).unwrap();
        assert_eq!(daemon.store.active_workspace_id().unwrap(), id);
        daemon.open_workspace(&id).unwrap();
        assert!(daemon.open_workspace(&WorkspaceId("ghost".into())).is_err());

        // Rename keeps names unique (a rename to itself is fine).
        daemon.rename_workspace(&id, "acme").unwrap();
        daemon.rename_workspace(&id, "acme").unwrap();
        assert!(daemon.rename_workspace(&id, "default").is_err());

        // Deleting the open workspace opens the surviving one first.
        daemon.remove_workspace(&id).unwrap();
        assert_eq!(
            daemon.store.active_workspace_id().unwrap().as_str(),
            "default"
        );
        assert!(daemon.store.get_workspace(&id).unwrap().is_none());

        // The last workspace can't go.
        assert!(daemon
            .remove_workspace(&WorkspaceId("default".into()))
            .is_err());
    }

    #[test]
    fn workspace_with_projects_refuses_deletion() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["p"]); // lands in 'default'
        let EntityId::Workspace(empty) = daemon.add_workspace("empty").unwrap() else {
            panic!("add returns the workspace id");
        };
        let err = daemon
            .remove_workspace(&WorkspaceId("default".into()))
            .unwrap_err();
        assert!(
            err.to_string().contains("1 project"),
            "helpful refusal: {err}"
        );
        // An empty, closed workspace deletes cleanly.
        daemon.remove_workspace(&empty).unwrap();
    }

    /// Reorders only see the project's own workspace: a move never swaps
    /// across workspaces, and other workspaces' sort orders stay untouched.
    #[test]
    fn move_project_is_scoped_to_the_workspace() {
        let daemon = test_daemon();
        seed_projects(&daemon, &["a", "b"]); // default ws, sort 0 and 1
        let EntityId::Workspace(other) = daemon.add_workspace("other").unwrap() else {
            panic!("add returns the workspace id");
        };
        daemon
            .store
            .insert_project(&Project {
                workspace_id: other.clone(),
                id: ProjectId("x".into()),
                name: "x".into(),
                repo_path: "/tmp/x".into(),
                sort_order: 1, // interleaves between a and b globally
                divider_after: false,
                divider_label: None,
                divider_before: false,
                divider_before_label: None,
                host: None,
            })
            .unwrap();

        daemon.move_project(&ProjectId("a".into()), 1).unwrap();
        let (projects, _, _, _) = daemon.store.load_tree().unwrap();
        let default_order: Vec<&str> = projects
            .iter()
            .filter(|p| p.workspace_id.as_str() == "default")
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(default_order, ["b", "a"], "a swapped with b, not x");
        let x = projects.iter().find(|p| p.name == "x").unwrap();
        assert_eq!(x.sort_order, 1, "other workspace untouched");
    }
}
