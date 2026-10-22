//! The agent status state machine. Pure logic — no I/O, injected clock — so
//! the tricky interleavings (Stop racing SubagentStart, post-turn helpers,
//! idle notifications, foreign sessions) are unit-testable.
//!
//! Semantics ported from mission-control's battle-tested implementation:
//! - `Stop` does NOT mean done while Task-tool subagents are still active;
//!   hold `running` and promote to `finished` only after a drain grace.
//! - A `SubagentStart` shortly *after* a finish heals back to `running`
//!   (the Stop raced the subagent's own hook POST) — but only within a
//!   window, because Claude runs post-turn helpers that fire subagent events
//!   with no following Stop.
//! - Hooks from a different Claude session in the same cwd are foreign and
//!   must not drive status.
//! - An `idle_prompt` notification is Claude reporting it is parked at the
//!   input box with nothing in flight; it is what un-sticks a turn that
//!   ended without a `Stop` (rejected prompt, escape mid-turn).
//! - `Progress` is the same end-of-turn news read straight off the PTY
//!   (OSC 9;4, see `pty::progress`). It is the only signal that survives a
//!   user cancel — no hook fires at all there, and Claude suppresses
//!   `idle_prompt` precisely because the user just touched the keyboard.
//! - The subagent hold is Claude-only (`track_subagents`). Codex fires
//!   `SubagentStart` from a spawned child thread but no `SubagentStop` when
//!   the parent aborts that child's turn (verified on 0.152: every wedged
//!   session had a child rollout ending in `turn_aborted`), and it emits
//!   neither `idle_prompt` nor OSC 9;4, so a held `Stop` had nothing to
//!   release it short of the 2h subagent TTL. For those CLIs `Stop` is the
//!   only end-of-turn signal there is, so it is authoritative.
//! - A Claude `Stop` while the agent still runs a command (Bash
//!   `run_in_background`, Monitor) is held the same way: the command's
//!   completion starts the turn that reports it. The registry probes for the
//!   command child and feeds the answer in as `CommandProbe`.

use nebula_core::{AgentKind, AgentStatus};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const RECENT_FINISH_WINDOW: Duration = Duration::from_secs(30);
pub const DRAIN_GRACE: Duration = Duration::from_secs(180);
/// Covers the gap between a background command exiting and Claude's turn reporting it.
pub const COMMAND_GRACE: Duration = Duration::from_secs(15);
pub const BACKGROUND_HOLD_CAP: Duration = Duration::from_secs(60 * 60);
pub const SUBAGENT_TTL: Duration = Duration::from_secs(2 * 60 * 60);
pub const MAX_TRACKED_SUBAGENTS: usize = 512;

#[derive(Debug, Clone, PartialEq)]
pub enum HookEvent {
    UserPromptSubmit,
    Stop,
    SessionStart {
        source: Option<String>,
    },
    PermissionRequest,
    Notification {
        notification_type: Option<String>,
    },
    PreToolUse {
        tool_name: Option<String>,
    },
    PostToolUse {
        tool_name: Option<String>,
    },
    SubagentStart {
        subagent_id: Option<String>,
    },
    SubagentStop {
        subagent_id: Option<String>,
    },
    /// Synthetic: the agent's PTY died.
    SessionEnded {
        exit_code: Option<i32>,
    },
    /// Synthetic: the CLI's OSC 9;4 progress state flipped. `busy: false` is
    /// end-of-turn — including the cancel that fires no hook.
    Progress {
        busy: bool,
    },
    /// Synthetic: nebula stopped the PTY on purpose to free its checkout.
    Parked,
    /// Synthetic: a new PTY was spawned for the agent.
    PtySpawned,
    /// Synthetic: whether the agent CLI has a command child right now.
    CommandProbe {
        running: bool,
    },
}

impl HookEvent {
    /// Events that (re)establish which Claude session id owns this agent.
    pub fn captures_session(&self) -> bool {
        matches!(
            self,
            HookEvent::UserPromptSubmit | HookEvent::SessionStart { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    SetStatus(AgentStatus),
    SaveSessionId(String),
}

#[derive(Clone, Copy)]
enum CommandHold {
    Waiting {
        since: Instant,
        gone_since: Option<Instant>,
    },
    /// The command released, but the held Stop still waits on subagents.
    Released,
}

impl CommandHold {
    fn released(&self, now: Instant) -> bool {
        match self {
            CommandHold::Waiting { since, gone_since } => {
                now.duration_since(*since) >= BACKGROUND_HOLD_CAP
                    || gone_since.is_some_and(|gone| now.duration_since(gone) >= COMMAND_GRACE)
            }
            CommandHold::Released => false,
        }
    }
}

#[derive(Default)]
struct SubagentSet {
    keyed: HashMap<String, Instant>,
    anon: usize,
}

impl SubagentSet {
    fn start(&mut self, id: Option<String>, now: Instant) {
        match id {
            Some(id) if self.keyed.len() < MAX_TRACKED_SUBAGENTS => {
                self.keyed.insert(id, now);
            }
            Some(_) | None => self.anon = self.anon.saturating_add(1).min(MAX_TRACKED_SUBAGENTS),
        }
    }

    /// Cross-cancel bias toward finishing: a keyed stop with no matching start
    /// cancels an anon start; an anon stop cancels the oldest keyed start.
    fn stop(&mut self, id: Option<String>) {
        match id {
            Some(id) => {
                if self.keyed.remove(&id).is_none() {
                    self.anon = self.anon.saturating_sub(1);
                }
            }
            None => {
                if self.anon > 0 {
                    self.anon -= 1;
                } else if let Some(oldest) = self
                    .keyed
                    .iter()
                    .min_by_key(|(_, t)| **t)
                    .map(|(k, _)| k.clone())
                {
                    self.keyed.remove(&oldest);
                }
            }
        }
    }

    fn prune_expired(&mut self, now: Instant) {
        self.keyed
            .retain(|_, started| now.duration_since(*started) < SUBAGENT_TTL);
        // Anon starts can't be aged individually; they are cleared wholesale on
        // session change / clear / prompt.
    }

    fn is_empty(&self) -> bool {
        self.keyed.is_empty() && self.anon == 0
    }

    fn clear(&mut self) {
        self.keyed.clear();
        self.anon = 0;
    }
}

pub struct AgentStatusMachine {
    status: AgentStatus,
    session_id: Option<String>,
    subagents: SubagentSet,
    finished_at: Option<Instant>,
    /// Set while a Stop is being held open because subagents were active.
    stop_held: bool,
    /// When the subagent set last became empty during a held Stop.
    drain_idle_since: Option<Instant>,
    /// Whether subagent events are tracked at all. Off, `Stop` finishes the
    /// turn regardless of children and SubagentStart never heals.
    track_subagents: bool,
    command_running: bool,
    command_hold: Option<CommandHold>,
    turn: u64,
}

impl AgentStatusMachine {
    pub fn new(status: AgentStatus, session_id: Option<String>) -> Self {
        Self {
            status,
            session_id,
            subagents: SubagentSet::default(),
            finished_at: None,
            stop_held: false,
            drain_idle_since: None,
            track_subagents: true,
            command_running: false,
            command_hold: None,
            turn: 0,
        }
    }

    /// Only Claude's Stop can precede live Task subagents *and* has the
    /// idle/progress backstops that release a held Stop; every other CLI
    /// gets `Stop` as the authoritative end of turn. See the module doc.
    pub fn for_kind(status: AgentStatus, session_id: Option<String>, kind: AgentKind) -> Self {
        let mut m = Self::new(status, session_id);
        m.track_subagents = kind == AgentKind::Claude;
        m
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

    /// Whether `event` ends a turn that a running command would hold open,
    /// so the caller should send a `CommandProbe` first.
    pub fn wants_command_probe(&self, event: &HookEvent) -> bool {
        self.track_subagents
            && matches!(event, HookEvent::Stop | HookEvent::Progress { busy: false })
    }

    /// Whether a Stop is held open for a background command, the
    /// post-exit `COMMAND_GRACE` included: the agent wants probing on the
    /// next tick, not proof that a command still runs.
    pub fn command_held(&self) -> bool {
        matches!(self.command_hold, Some(CommandHold::Waiting { .. }))
    }

    /// Counts turn starts, so a caller that probed outside the delivery lock
    /// can tell whether a newer turn started meanwhile.
    pub fn turn(&self) -> u64 {
        self.turn
    }

    /// `event` preceded by its `CommandProbe`, both taken while the machine
    /// was at `turn`. Dropped when a newer turn started in between: that turn
    /// ends with its own Stop.
    pub fn handle_probed(
        &mut self,
        event: HookEvent,
        payload_session_id: Option<&str>,
        running: bool,
        turn: u64,
        now: Instant,
    ) -> Vec<Effect> {
        if turn != self.turn {
            return Vec::new();
        }
        self.handle(HookEvent::CommandProbe { running }, None, now);
        self.handle(event, payload_session_id, now)
    }

    pub fn handle(
        &mut self,
        event: HookEvent,
        payload_session_id: Option<&str>,
        now: Instant,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.status == AgentStatus::Disconnected && event != HookEvent::PtySpawned {
            return effects;
        }

        // ---- session reconciliation ----
        if event.captures_session() {
            if let Some(sid) = payload_session_id {
                if self.session_id.as_deref() != Some(sid) {
                    // New claude process (restart / manual relaunch): adopt it
                    // and forget the old session's subagents.
                    self.session_id = Some(sid.to_string());
                    self.subagents.clear();
                    self.stop_held = false;
                    self.drain_idle_since = None;
                    self.command_hold = None;
                    effects.push(Effect::SaveSessionId(sid.to_string()));
                }
            }
        } else if !matches!(event, HookEvent::SessionEnded { .. }) {
            if let (Some(mine), Some(theirs)) = (self.session_id.as_deref(), payload_session_id) {
                if mine != theirs {
                    return effects; // foreign session — ignore entirely
                }
            }
        }

        match event {
            HookEvent::UserPromptSubmit => {
                self.turn += 1;
                self.stop_held = false;
                self.drain_idle_since = None;
                self.command_hold = None;
                self.finished_at = None;
                self.set_status(AgentStatus::Running, &mut effects);
            }
            HookEvent::Stop => {
                self.subagents.prune_expired(now);
                self.hold_for_command(now);
                if self.subagents.is_empty() {
                    self.stop_held = false;
                    self.settle(now, &mut effects);
                } else {
                    // Foreground turn ended but subagents are still working.
                    self.stop_held = true;
                    self.drain_idle_since = None;
                    self.set_status(AgentStatus::Running, &mut effects);
                }
            }
            HookEvent::SessionStart { source } => {
                if source.as_deref() == Some("clear") {
                    // Same session id, but /clear killed any live subagents.
                    self.subagents.clear();
                    self.stop_held = false;
                    self.drain_idle_since = None;
                }
            }
            HookEvent::PermissionRequest => {
                self.set_status(AgentStatus::NeedsFeedback, &mut effects);
            }
            HookEvent::Notification { notification_type } => {
                match notification_type.as_deref() {
                    Some("permission_prompt") => {
                        self.set_status(AgentStatus::NeedsFeedback, &mut effects)
                    }
                    // "Claude is waiting for your input". Claude fires this
                    // only with nothing in flight and no dialog open, so it
                    // means the turn really is over — see `mark_idle`.
                    Some("idle_prompt") => self.mark_idle(&mut effects),
                    // Every other notification type (auth, quota, nested
                    // fleet sessions) is none of our business.
                    _ => {}
                }
            }
            HookEvent::PreToolUse { tool_name } => {
                if tool_name.as_deref() == Some("AskUserQuestion") {
                    self.set_status(AgentStatus::NeedsFeedback, &mut effects);
                }
            }
            HookEvent::PostToolUse { tool_name } => {
                if tool_name.as_deref() == Some("AskUserQuestion") {
                    self.set_status(AgentStatus::Running, &mut effects);
                }
            }
            HookEvent::SubagentStart { .. } | HookEvent::SubagentStop { .. }
                if !self.track_subagents => {}
            HookEvent::SubagentStart { subagent_id } => {
                self.subagents.start(subagent_id, now);
                if self.status == AgentStatus::Finished {
                    match self.finished_at {
                        // The Stop raced this subagent's own POST — heal.
                        Some(finished) if now.duration_since(finished) < RECENT_FINISH_WINDOW => {
                            self.stop_held = true;
                            self.drain_idle_since = None;
                            self.set_status(AgentStatus::Running, &mut effects);
                        }
                        // Post-turn internal helper (away-summary etc.): a
                        // start with no Stop coming. Track it, don't heal —
                        // healing here wedges the agent on running forever.
                        _ => {}
                    }
                }
            }
            HookEvent::SubagentStop { subagent_id } => {
                self.subagents.stop(subagent_id);
            }
            HookEvent::Progress { busy } => {
                if busy {
                    self.turn += 1;
                    if self.command_hold.take().is_some() {
                        self.stop_held = false;
                        self.drain_idle_since = None;
                    }
                    // A turn started. Normally `UserPromptSubmit` already
                    // said so; this also catches turns nebula never saw a
                    // prompt for (a resumed session, a scheduled wake-up).
                    // Deliberately narrow: it must not talk over a pending
                    // permission prompt (which holds progress at *busy*
                    // anyway, so no edge arrives) or revive a dead agent.
                    if matches!(self.status, AgentStatus::Fresh | AgentStatus::Finished) {
                        self.stop_held = false;
                        self.drain_idle_since = None;
                        self.finished_at = None;
                        self.set_status(AgentStatus::Running, &mut effects);
                    }
                } else if matches!(
                    self.status,
                    AgentStatus::Running | AgentStatus::NeedsFeedback
                ) {
                    // The CLI cleared its progress bar: the turn is over,
                    // however it ended. Same bookkeeping as `Stop`, so a
                    // real Stop arriving either side of this is a no-op and
                    // the subagent drain hold still applies.
                    self.subagents.prune_expired(now);
                    self.hold_for_command(now);
                    if self.subagents.is_empty() {
                        self.stop_held = false;
                        self.settle(now, &mut effects);
                    } else {
                        self.stop_held = true;
                        self.drain_idle_since = None;
                        self.set_status(AgentStatus::Running, &mut effects);
                    }
                }
                // Fresh / Terminated / Disconnected are left alone: a CLI
                // clears its progress bar on startup and on exit too, and
                // neither is a finished turn.
            }
            HookEvent::SessionEnded { exit_code } => {
                // Dead process: laggard subagent POSTs must never resurrect it.
                self.subagents.clear();
                self.stop_held = false;
                self.drain_idle_since = None;
                self.command_hold = None;
                self.finished_at = None;
                if matches!(
                    self.status,
                    AgentStatus::Running | AgentStatus::NeedsFeedback
                ) {
                    let status = if exit_code == Some(0) {
                        AgentStatus::Finished
                    } else {
                        AgentStatus::Terminated
                    };
                    self.set_status(status, &mut effects);
                }
            }
            HookEvent::Parked => {
                self.subagents.clear();
                self.stop_held = false;
                self.drain_idle_since = None;
                self.command_hold = None;
                self.finished_at = None;
                self.set_status(AgentStatus::Disconnected, &mut effects);
            }
            HookEvent::PtySpawned => {
                if self.status == AgentStatus::Disconnected {
                    self.set_status(AgentStatus::Fresh, &mut effects);
                }
            }
            HookEvent::CommandProbe { running } => {
                self.command_running = running;
                if let Some(CommandHold::Waiting { gone_since, .. }) = &mut self.command_hold {
                    *gone_since = if running {
                        None
                    } else {
                        gone_since.or(Some(now))
                    };
                }
            }
        }
        effects
    }

    /// Periodic tick (the deferred-finish recheck): while a Stop is held open,
    /// promote to finished once the subagent set has drained and stayed empty
    /// for the grace period, and the command hold has released.
    pub fn tick(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.status != AgentStatus::Running {
            return effects;
        }
        if self.command_hold.is_some_and(|hold| hold.released(now)) {
            self.command_hold = Some(CommandHold::Released);
            self.subagents.prune_expired(now);
            if self.stop_held || !self.subagents.is_empty() {
                self.stop_held = true;
            } else {
                self.settle(now, &mut effects);
            }
        }
        if !self.stop_held {
            return effects;
        }
        self.subagents.prune_expired(now);
        if self.subagents.is_empty() {
            match self.drain_idle_since {
                None => self.drain_idle_since = Some(now),
                Some(idle_since) if now.duration_since(idle_since) >= DRAIN_GRACE => {
                    self.stop_held = false;
                    self.drain_idle_since = None;
                    self.settle(now, &mut effects);
                }
                Some(_) => {}
            }
        } else {
            self.drain_idle_since = None;
        }
        effects
    }

    /// Claude reports itself idle at the input box. This is the only end-of-
    /// turn signal that survives the paths where no `Stop` ever fires: the
    /// user rejecting a permission prompt or an `AskUserQuestion`, or hitting
    /// escape mid-turn. Without it those leave the agent pinned on red (or
    /// yellow) until the next prompt, long after the CLI went quiet.
    ///
    /// Safe to trust as authoritative because Claude gates the notification
    /// on an idle main loop AND an empty dialog stack — a permission prompt
    /// still waiting on the user suppresses it, so this can't green out an
    /// agent that genuinely needs feedback.
    fn mark_idle(&mut self, effects: &mut Vec<Effect>) {
        if !matches!(
            self.status,
            AgentStatus::Running | AgentStatus::NeedsFeedback
        ) {
            return;
        }
        // Anything still tracked is orphaned: the main loop can't be idle
        // while it waits on a Task subagent.
        self.subagents.clear();
        self.stop_held = false;
        self.drain_idle_since = None;
        // Deliberately no `finished_at`: after this much idle time a
        // SubagentStart is a post-turn helper, never a Stop that raced its
        // own POST, so it must not heal back to running.
        self.finished_at = None;
        // Claude sits idle at the input box while its background command
        // runs; that hold releases on its own.
        if self.command_held() {
            return;
        }
        self.command_hold = None;
        self.set_status(AgentStatus::Finished, effects);
    }

    fn hold_for_command(&mut self, now: Instant) {
        if self.track_subagents && self.command_running && self.command_hold.is_none() {
            self.command_hold = Some(CommandHold::Waiting {
                since: now,
                gone_since: None,
            });
        }
    }

    fn settle(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        if self.command_held() {
            self.set_status(AgentStatus::Running, effects);
        } else {
            self.command_hold = None;
            self.finished_at = Some(now);
            self.set_status(AgentStatus::Finished, effects);
        }
    }

    fn set_status(&mut self, status: AgentStatus, effects: &mut Vec<Effect>) {
        if self.status != status {
            self.status = status;
            effects.push(Effect::SetStatus(status));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn status_of(effects: &[Effect]) -> Option<AgentStatus> {
        effects.iter().rev().find_map(|e| match e {
            Effect::SetStatus(s) => Some(*s),
            _ => None,
        })
    }

    #[test]
    fn normal_turn_lifecycle() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        let fx = m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        assert_eq!(status_of(&fx), Some(AgentStatus::Running));
        assert!(fx.contains(&Effect::SaveSessionId("s1".into())));
        let fx = m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(10));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn permission_prompt_flow() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        let fx = m.handle(HookEvent::PermissionRequest, Some("s1"), now);
        assert_eq!(status_of(&fx), Some(AgentStatus::NeedsFeedback));
        // Answering the prompt resumes the turn (next event is tool traffic /
        // eventually Stop; a PostToolUse AskUserQuestion also flips back).
        let fx = m.handle(
            HookEvent::PostToolUse {
                tool_name: Some("AskUserQuestion".into()),
            },
            Some("s1"),
            now,
        );
        assert_eq!(status_of(&fx), Some(AgentStatus::Running));
    }

    fn idle(m: &mut AgentStatusMachine, now: Instant) -> Vec<Effect> {
        m.handle(
            HookEvent::Notification {
                notification_type: Some("idle_prompt".into()),
            },
            Some("s1"),
            now,
        )
    }

    #[test]
    fn idle_notification_is_a_noop_when_already_finished() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(HookEvent::Stop, Some("s1"), now);
        let fx = idle(&mut m, now);
        assert!(fx.is_empty(), "already finished: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Finished);
    }

    #[test]
    fn idle_notification_clears_a_rejected_question() {
        // The reported bug: AskUserQuestion goes red, the user rejects it,
        // and the interrupted turn fires neither PostToolUse nor Stop.
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        let fx = m.handle(
            HookEvent::PreToolUse {
                tool_name: Some("AskUserQuestion".into()),
            },
            Some("s1"),
            now,
        );
        assert_eq!(status_of(&fx), Some(AgentStatus::NeedsFeedback));
        let fx = idle(&mut m, now + Duration::from_secs(60));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn idle_notification_clears_a_stale_running_turn() {
        // Escape mid-turn: no Stop ever arrives, so running would stick.
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        let fx = idle(&mut m, now + Duration::from_secs(60));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn idle_notification_drops_held_stop_and_orphaned_subagents() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now,
        );
        m.handle(HookEvent::Stop, Some("s1"), now);
        assert_eq!(m.status(), AgentStatus::Running, "stop held open");
        let fx = idle(&mut m, now + Duration::from_secs(60));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
        // A helper subagent afterwards must not heal it back to running.
        let fx = m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub2".into()),
            },
            Some("s1"),
            now + Duration::from_secs(61),
        );
        assert!(fx.is_empty(), "post-idle subagent must not heal: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Finished);
    }

    #[test]
    fn idle_notification_leaves_fresh_and_dead_agents_alone() {
        for start in [
            AgentStatus::Fresh,
            AgentStatus::Terminated,
            AgentStatus::Disconnected,
        ] {
            let mut m = AgentStatusMachine::new(start, Some("s1".into()));
            let fx = idle(&mut m, t0());
            assert!(fx.is_empty(), "{start:?} must not be touched: {fx:?}");
            assert_eq!(m.status(), start);
        }
    }

    #[test]
    fn foreign_idle_notification_is_ignored() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(HookEvent::PermissionRequest, Some("s1"), now);
        let fx = m.handle(
            HookEvent::Notification {
                notification_type: Some("idle_prompt".into()),
            },
            Some("someone-elses-claude"),
            now,
        );
        assert!(fx.is_empty(), "foreign session: {fx:?}");
        assert_eq!(m.status(), AgentStatus::NeedsFeedback);
    }

    #[test]
    fn unknown_notification_types_are_ignored() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        for ty in [
            "auth_success",
            "agent_needs_input",
            "quota_auto_resume_fired",
        ] {
            let fx = m.handle(
                HookEvent::Notification {
                    notification_type: Some(ty.into()),
                },
                Some("s1"),
                now,
            );
            assert!(fx.is_empty(), "{ty} must not flip status: {fx:?}");
        }
        assert_eq!(m.status(), AgentStatus::Running);
    }

    #[test]
    fn stop_with_active_subagents_holds_running_until_drained() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now,
        );
        let fx = m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(5));
        assert_eq!(status_of(&fx), None, "stays running — no transition");
        assert_eq!(m.status(), AgentStatus::Running);

        // Subagent finishes; drain grace must elapse before finished.
        m.handle(
            HookEvent::SubagentStop {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now + Duration::from_secs(60),
        );
        let fx = m.tick(now + Duration::from_secs(61));
        assert!(fx.is_empty(), "grace not elapsed yet");
        let fx = m.tick(now + Duration::from_secs(61) + DRAIN_GRACE);
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn codex_stop_is_authoritative_despite_unstopped_subagents() {
        // Codex 0.152: a child thread whose turn the parent aborts fires
        // SubagentStart but never SubagentStop, and codex has no idle or
        // progress signal to release a held Stop.
        let mut m = AgentStatusMachine::for_kind(AgentStatus::Fresh, None, AgentKind::Codex);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("child".into()),
            },
            Some("s1"),
            now,
        );
        let fx = m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(5));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));

        // A late SubagentStart must not heal it back either.
        let fx = m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("child2".into()),
            },
            Some("s1"),
            now + Duration::from_secs(6),
        );
        assert!(fx.is_empty(), "{fx:?}");
        assert_eq!(m.status(), AgentStatus::Finished);
    }

    #[test]
    fn claude_keeps_the_subagent_hold_via_for_kind() {
        let mut m = AgentStatusMachine::for_kind(AgentStatus::Fresh, None, AgentKind::Claude);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now,
        );
        m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(5));
        assert_eq!(m.status(), AgentStatus::Running);
    }

    #[test]
    fn subagent_start_shortly_after_finish_heals_to_running() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(10));
        assert_eq!(m.status(), AgentStatus::Finished);
        // The subagent's own POST arrives 2s later — race heal.
        let fx = m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now + Duration::from_secs(12),
        );
        assert_eq!(status_of(&fx), Some(AgentStatus::Running));
    }

    #[test]
    fn post_turn_helper_outside_window_does_not_heal() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(10));
        // An away-summary helper fires SubagentStart minutes later.
        let fx = m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("helper".into()),
            },
            Some("s1"),
            now + Duration::from_secs(10) + RECENT_FINISH_WINDOW + Duration::from_secs(1),
        );
        assert!(fx.is_empty(), "must stay finished: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Finished);
    }

    fn progress(m: &mut AgentStatusMachine, busy: bool, now: Instant) -> Vec<Effect> {
        m.handle(HookEvent::Progress { busy }, None, now)
    }

    #[test]
    fn progress_idle_finishes_a_cancelled_turn() {
        // The reported bug: the user hits escape. Claude Code fires no Stop
        // for an interrupted turn and never sends `idle_prompt` either (it
        // suppresses that when the user has just touched the keyboard), so
        // the only news is the progress bar clearing.
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        progress(&mut m, true, now);
        assert_eq!(m.status(), AgentStatus::Running);
        let fx = progress(&mut m, false, now + Duration::from_secs(8));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn progress_idle_clears_a_rejected_permission_prompt() {
        // Escaping out of a permission prompt: same story, from red.
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(HookEvent::PermissionRequest, Some("s1"), now);
        assert_eq!(m.status(), AgentStatus::NeedsFeedback);
        let fx = progress(&mut m, false, now + Duration::from_secs(5));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn progress_idle_leaves_fresh_and_dead_agents_alone() {
        // Every CLI clears its progress bar at startup and again on exit;
        // neither is a finished turn.
        for start in [
            AgentStatus::Fresh,
            AgentStatus::Finished,
            AgentStatus::Terminated,
            AgentStatus::Disconnected,
        ] {
            let mut m = AgentStatusMachine::new(start, Some("s1".into()));
            let fx = progress(&mut m, false, t0());
            assert!(fx.is_empty(), "{start:?} must not be touched: {fx:?}");
            assert_eq!(m.status(), start);
        }
    }

    #[test]
    fn progress_busy_starts_a_turn_but_never_talks_over_feedback() {
        // A turn nebula saw no prompt for (resumed session, scheduled wake).
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, Some("s1".into()));
        let now = t0();
        let fx = progress(&mut m, true, now);
        assert_eq!(status_of(&fx), Some(AgentStatus::Running));

        // …but a pending question outranks it, and the dead stay dead.
        for start in [
            AgentStatus::NeedsFeedback,
            AgentStatus::Terminated,
            AgentStatus::Disconnected,
        ] {
            let mut m = AgentStatusMachine::new(start, Some("s1".into()));
            let fx = progress(&mut m, true, now);
            assert!(fx.is_empty(), "{start:?} must not be touched: {fx:?}");
            assert_eq!(m.status(), start);
        }
    }

    #[test]
    fn progress_idle_respects_the_subagent_drain_hold() {
        // The progress bar clears when the *main loop* parks, which on a
        // normal turn end beats the Stop hook's HTTP round-trip. It must not
        // finish out from under still-running subagents.
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now,
        );
        let fx = progress(&mut m, false, now + Duration::from_secs(5));
        assert_eq!(status_of(&fx), None, "stays running — no transition");
        assert_eq!(m.status(), AgentStatus::Running);
        // The Stop that follows a beat later agrees, and the drain still owns
        // the promotion.
        let fx = m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(5));
        assert!(fx.is_empty());
        m.handle(
            HookEvent::SubagentStop {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now + Duration::from_secs(6),
        );
        let fx = m.tick(now + Duration::from_secs(7));
        assert!(fx.is_empty(), "grace not elapsed yet");
        let fx = m.tick(now + Duration::from_secs(7) + DRAIN_GRACE);
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn progress_idle_preserves_the_subagent_race_heal() {
        // Progress clears just before the Stop hook lands. Both stamp
        // `finished_at`, so a subagent POST that raced the Stop still heals —
        // unlike `mark_idle`, which deliberately blocks healing.
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        progress(&mut m, false, now + Duration::from_secs(10));
        m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(10));
        assert_eq!(m.status(), AgentStatus::Finished);
        let fx = m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now + Duration::from_secs(12),
        );
        assert_eq!(status_of(&fx), Some(AgentStatus::Running));
    }

    #[test]
    fn progress_edges_across_a_whole_turn_settle_on_finished() {
        // The captured Claude Code 2.1.241 sequence, hooks and all.
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        progress(&mut m, false, now); // startup, parked at the input box
        assert_eq!(m.status(), AgentStatus::Fresh);
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        progress(&mut m, true, now);
        m.handle(
            HookEvent::PermissionRequest,
            Some("s1"),
            now + Duration::from_secs(3),
        );
        assert_eq!(m.status(), AgentStatus::NeedsFeedback);
        // Approving does not move the progress bar — it never left "busy".
        m.handle(
            HookEvent::PostToolUse {
                tool_name: Some("Bash".into()),
            },
            Some("s1"),
            now + Duration::from_secs(19),
        );
        let fx = progress(&mut m, false, now + Duration::from_secs(20));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn foreign_session_events_are_ignored() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        // A manually-launched claude in the same cwd posts a Stop.
        let fx = m.handle(HookEvent::Stop, Some("other-session"), now);
        assert!(fx.is_empty());
        assert_eq!(m.status(), AgentStatus::Running);
    }

    #[test]
    fn new_session_id_on_capture_adopts_and_clears_subagents() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now,
        );
        // claude restarted (new session id) and a fresh prompt arrives.
        let fx = m.handle(
            HookEvent::UserPromptSubmit,
            Some("s2"),
            now + Duration::from_secs(5),
        );
        assert!(fx.contains(&Effect::SaveSessionId("s2".into())));
        // Old subagents gone: a Stop finishes immediately.
        let fx = m.handle(HookEvent::Stop, Some("s2"), now + Duration::from_secs(6));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn pty_death_while_running_terminates_and_blocks_heal() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        let fx = m.handle(
            HookEvent::SessionEnded {
                exit_code: Some(137),
            },
            None,
            now,
        );
        assert_eq!(status_of(&fx), Some(AgentStatus::Terminated));
        // Laggard subagent POST must not resurrect the dead agent.
        let fx = m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now + Duration::from_secs(1),
        );
        assert!(fx.is_empty());
        assert_eq!(m.status(), AgentStatus::Terminated);
    }

    #[test]
    fn pty_clean_exit_while_running_is_finished() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        let fx = m.handle(HookEvent::SessionEnded { exit_code: Some(0) }, None, now);
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn pty_exit_when_already_finished_keeps_finished() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(HookEvent::Stop, Some("s1"), now);
        let fx = m.handle(HookEvent::SessionEnded { exit_code: Some(1) }, None, now);
        assert!(
            fx.is_empty(),
            "finished agent whose pty closes stays finished"
        );
        assert_eq!(m.status(), AgentStatus::Finished);
    }

    #[test]
    fn anon_and_keyed_subagent_cross_cancel() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        // Keyed start, anon stop → biases toward finishing.
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now,
        );
        m.handle(
            HookEvent::SubagentStop { subagent_id: None },
            Some("s1"),
            now,
        );
        let fx = m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(1));
        assert_eq!(
            status_of(&fx),
            Some(AgentStatus::Finished),
            "set drained via cross-cancel"
        );
    }

    #[test]
    fn clear_source_session_start_clears_subagents() {
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        let now = t0();
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(
            HookEvent::SubagentStart {
                subagent_id: Some("sub1".into()),
            },
            Some("s1"),
            now,
        );
        m.handle(
            HookEvent::SessionStart {
                source: Some("clear".into()),
            },
            Some("s1"),
            now,
        );
        let fx = m.handle(HookEvent::Stop, Some("s1"), now + Duration::from_secs(1));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    fn probe(m: &mut AgentStatusMachine, running: bool, now: Instant) {
        let fx = m.handle(HookEvent::CommandProbe { running }, None, now);
        assert!(fx.is_empty(), "a probe never moves status: {fx:?}");
    }

    fn subagent_start(id: &str) -> HookEvent {
        HookEvent::SubagentStart {
            subagent_id: Some(id.into()),
        }
    }

    fn subagent_stop(id: &str) -> HookEvent {
        HookEvent::SubagentStop {
            subagent_id: Some(id.into()),
        }
    }

    fn held_on_command(kind: AgentKind, now: Instant) -> AgentStatusMachine {
        let mut m = AgentStatusMachine::for_kind(AgentStatus::Fresh, None, kind);
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        probe(&mut m, true, now);
        m.handle(HookEvent::Stop, Some("s1"), now);
        m
    }

    #[test]
    fn claude_stop_with_a_running_command_stays_running() {
        let now = t0();
        let mut m = held_on_command(AgentKind::Claude, now);
        assert_eq!(m.status(), AgentStatus::Running);
        probe(&mut m, true, now + Duration::from_secs(600));
        let fx = m.tick(now + Duration::from_secs(600));
        assert!(fx.is_empty(), "command still running: {fx:?}");
        let fx = idle(&mut m, now + Duration::from_secs(660));
        assert!(
            fx.is_empty(),
            "idle at the prompt while the command runs: {fx:?}"
        );
        assert_eq!(m.status(), AgentStatus::Running);
    }

    #[test]
    fn command_hold_finishes_once_the_command_is_gone_for_the_grace() {
        let now = t0();
        let mut m = held_on_command(AgentKind::Claude, now);
        let gone = now + Duration::from_secs(120);
        probe(&mut m, false, gone);
        assert!(m.tick(gone).is_empty(), "grace not elapsed yet");
        probe(&mut m, false, gone + COMMAND_GRACE);
        let fx = m.tick(gone + COMMAND_GRACE);
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn new_turn_within_the_command_grace_cancels_the_release() {
        let now = t0();
        let mut m = held_on_command(AgentKind::Claude, now);
        let gone = now + Duration::from_secs(120);
        probe(&mut m, false, gone);
        m.tick(gone);
        let fx = progress(&mut m, true, gone + Duration::from_secs(2));
        assert!(fx.is_empty(), "already running: {fx:?}");
        probe(&mut m, false, gone + COMMAND_GRACE);
        let fx = m.tick(gone + COMMAND_GRACE);
        assert!(fx.is_empty(), "the reporting turn is still going: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Running);
        probe(&mut m, false, gone + COMMAND_GRACE + Duration::from_secs(5));
        let fx = m.handle(
            HookEvent::Stop,
            Some("s1"),
            gone + COMMAND_GRACE + Duration::from_secs(5),
        );
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn command_hold_releases_at_the_cap() {
        let now = t0();
        let mut m = held_on_command(AgentKind::Claude, now);
        let almost = now + BACKGROUND_HOLD_CAP - Duration::from_secs(1);
        probe(&mut m, true, almost);
        assert!(m.tick(almost).is_empty(), "under the cap");
        probe(&mut m, true, now + BACKGROUND_HOLD_CAP);
        let fx = m.tick(now + BACKGROUND_HOLD_CAP);
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn new_turn_within_the_command_grace_cancels_a_pending_subagent_release() {
        let now = t0();
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        m.handle(subagent_start("a"), Some("s1"), now);
        probe(&mut m, true, now);
        m.handle(HookEvent::Stop, Some("s1"), now);
        m.handle(subagent_stop("a"), Some("s1"), now + Duration::from_secs(1));
        probe(&mut m, false, now + Duration::from_secs(30));
        m.tick(now + Duration::from_secs(30));
        progress(&mut m, true, now + Duration::from_secs(32));
        let later = now + Duration::from_secs(210);
        let fx = m.tick(later);
        assert!(fx.is_empty(), "the reporting turn has no Stop yet: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Running);
    }

    #[test]
    fn command_release_waits_for_a_subagent_started_during_the_hold() {
        let now = t0();
        let mut m = held_on_command(AgentKind::Claude, now);
        m.handle(
            subagent_start("a"),
            Some("s1"),
            now + Duration::from_secs(1),
        );
        probe(&mut m, false, now + Duration::from_secs(30));
        m.tick(now + Duration::from_secs(30));
        let fx = m.tick(now + Duration::from_secs(60));
        assert!(fx.is_empty(), "subagent a never stopped: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Running);
        m.handle(
            subagent_stop("a"),
            Some("s1"),
            now + Duration::from_secs(90),
        );
        m.tick(now + Duration::from_secs(90));
        let fx = m.tick(now + Duration::from_secs(90) + DRAIN_GRACE);
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn a_stop_probed_before_the_next_turn_started_does_not_finish_it() {
        let now = t0();
        let mut m = AgentStatusMachine::new(AgentStatus::Fresh, None);
        m.handle(HookEvent::UserPromptSubmit, Some("s1"), now);
        let probed_at = m.turn();
        progress(&mut m, true, now + Duration::from_secs(1));
        let fx = m.handle_probed(
            HookEvent::Stop,
            Some("s1"),
            false,
            probed_at,
            now + Duration::from_secs(1),
        );
        assert!(fx.is_empty(), "the newer turn has no Stop yet: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Running);
    }

    #[test]
    fn new_turn_cancels_the_release_after_the_command_handed_over_to_a_subagent() {
        let now = t0();
        let at = |s| now + Duration::from_secs(s);
        let mut m = held_on_command(AgentKind::Claude, now);
        m.handle(subagent_start("a"), Some("s1"), at(1));
        probe(&mut m, false, at(30));
        m.tick(at(30));
        m.tick(at(60));
        m.handle(subagent_stop("a"), Some("s1"), at(90));
        m.tick(at(90));
        progress(&mut m, true, at(92));
        let fx = m.tick(at(270));
        assert!(fx.is_empty(), "the reporting turn has no Stop yet: {fx:?}");
        assert_eq!(m.status(), AgentStatus::Running);
        probe(&mut m, false, at(300));
        let fx = m.handle(HookEvent::Stop, Some("s1"), at(300));
        assert_eq!(status_of(&fx), Some(AgentStatus::Finished));
    }

    #[test]
    fn codex_stop_is_authoritative_despite_a_running_command() {
        let m = held_on_command(AgentKind::Codex, t0());
        assert_eq!(m.status(), AgentStatus::Finished);
    }
}
