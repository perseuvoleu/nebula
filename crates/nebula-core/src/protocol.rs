use crate::entities::{
    Agent, AgentKind, AgentStatus, Entity, EntityId, Link, Note, NoteOwner, Project, TerminalTab,
    Todo, TodoOwner, Workspace, Worktree,
};
use crate::ids::{AgentId, LinkId, NoteId, ProjectId, TerminalId, TodoId, WorkspaceId, WorktreeId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Bump on any breaking change to these enums. The daemon refuses mismatched
/// clients; the client then offers a kill-and-restart of the old daemon.
pub const PROTOCOL_VERSION: u32 = 29;

/// Max IPC frame size (length prefix sanity bound).
pub const MAX_FRAME_LEN: u32 = 4 * 1024 * 1024;

/// Starts the `Error` a `CheckoutPrimary` or `ResolveBranchCheckout` gets
/// when live sessions sit on primary; clients answer it with a confirmation
/// that resends `CheckoutPrimary` with `stop_busy`.
pub const BUSY_SESSIONS_REFUSAL: &str = "primary has sessions on";

/// Sits in the `Error` a `CreateAgent` with `WriterClaim::Exclusive` gets
/// while another live session is the checkout's writer; the CLI exits 2 on it.
pub const WRITER_HELD_REFUSAL: &str = "is the writer on";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionRef {
    Agent(AgentId),
    Terminal(TerminalId),
}

/// What a `CreateAgent` claims of its checkout's single writer slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriterClaim {
    #[default]
    Off,
    /// Refused while another live session holds the slot.
    Exclusive,
    /// Takes the slot alongside a live holder; the user approved two writers.
    Forced,
}

impl WriterClaim {
    pub fn is_off(&self) -> bool {
        *self == WriterClaim::Off
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientRequest {
    Hello {
        protocol_version: u32,
    },
    /// Reply is one Snapshot, then deltas stream on this connection forever.
    Subscribe,

    // -- PTY plane --
    Attach {
        session: SessionRef,
        /// Resume point for gap-free re-attach; None = replay whole ring.
        from_seq: Option<u64>,
        cols: u16,
        rows: u16,
    },
    Detach {
        session: SessionRef,
    },
    Input {
        session: SessionRef,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// One explicit key/input write, acknowledged only after the PTY accepted it.
    InputAcknowledged {
        req_id: u64,
        id: AgentId,
        project: ProjectId,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    Resize {
        session: SessionRef,
        cols: u16,
        rows: u16,
    },

    // -- entity CRUD (RPC-style; answered by Ack/Error with matching req_id) --
    /// Create a workspace. Does not open it — that stays a separate step.
    AddWorkspace {
        req_id: u64,
        name: String,
    },
    /// Delete a workspace. Refused while it still holds projects, or when it
    /// is the last workspace. Deleting the open workspace opens another one
    /// first (broadcast as ActiveWorkspaceChanged).
    RemoveWorkspace {
        req_id: u64,
        id: WorkspaceId,
    },
    RenameWorkspace {
        req_id: u64,
        id: WorkspaceId,
        name: String,
    },
    /// Make this the open workspace — daemon-global state, persisted and
    /// broadcast to every client as ActiveWorkspaceChanged.
    OpenWorkspace {
        req_id: u64,
        id: WorkspaceId,
    },
    AddProject {
        req_id: u64,
        path: PathBuf,
        name: Option<String>,
        /// Create `path` (and `git init` it, per config) when it doesn't
        /// exist on disk. Set only after the user confirmed in the client.
        create_missing: bool,
        /// ssh destination when `path` is on another machine (`nebula add
        /// host:/path`). The daemon reads the repo over ssh and never
        /// creates a missing remote directory.
        #[serde(default)]
        host: Option<String>,
    },
    RemoveProject {
        req_id: u64,
        id: ProjectId,
    },
    /// Move a project `delta` slots in the list (clamped at the edges).
    MoveProject {
        req_id: u64,
        id: ProjectId,
        delta: i64,
    },
    /// Set a group divider on a project row: presence and label.
    /// `before` targets the leading divider drawn above the row (only
    /// valid on the first project) instead of the one hanging below it.
    /// `present: false` removes it (label is dropped too).
    SetProjectDivider {
        req_id: u64,
        id: ProjectId,
        before: bool,
        present: bool,
        label: Option<String>,
    },
    /// Move the divider on project `id` (`before` picks which one) to the
    /// neighboring gap (sign of `delta`). No-op past the list's edges or
    /// when the destination gap already has a divider.
    MoveDivider {
        req_id: u64,
        id: ProjectId,
        before: bool,
        delta: i64,
    },
    CreateWorktree {
        req_id: u64,
        project: ProjectId,
        branch: String,
        base: Option<String>,
    },
    /// Resolve a usable checkout of an existing branch on the project's host.
    /// Moves primary under `CheckoutPrimary`'s rules, live sessions refused;
    /// never creates a branch or a linked checkout.
    ResolveBranchCheckout {
        req_id: u64,
        project: ProjectId,
        branch: String,
    },
    /// Conservative web removal: no sessions, clean and merged, branch preserved.
    CheckWorktreeRemoval {
        req_id: u64,
        id: WorktreeId,
        branch: String,
        path: PathBuf,
        delete: bool,
        /// Some: reply with a per-criterion `Report` before the Ack/Error.
        /// Skipped when None so protocol-29 requests keep their wire shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<RemovalFlags>,
    },
    /// Drop worktree rows whose checkout directory and git worktree entry
    /// are both gone; replies with a `Report` naming each one first.
    PruneWorktrees {
        req_id: u64,
        project: ProjectId,
        delete: bool,
        with_archived: bool,
    },
    DeleteWorktree {
        req_id: u64,
        id: WorktreeId,
        force: bool,
    },
    /// Explicitly move primary. Refuses uncommitted changes, a blocking
    /// untracked file, branches already hosted by a linked checkout, and live
    /// sessions on primary unless `stop_busy`; then stops them, and they keep
    /// their rows and branch.
    CheckoutPrimary {
        req_id: u64,
        project: ProjectId,
        branch: String,
        /// Sole caller moving its own physical checkout; it keeps running.
        #[serde(default)]
        mover: Option<SessionRef>,
        /// The user confirmed stopping the sessions on primary. Skipped
        /// when false so unconfirmed requests stay readable by older daemons.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stop_busy: bool,
    },
    /// Pin/unpin the worktree in the worktrees list (pure metadata).
    SetWorktreePinned {
        req_id: u64,
        id: WorktreeId,
        pinned: bool,
    },
    CreateAgent {
        req_id: u64,
        worktree: WorktreeId,
        /// Requested branch for a branch/session action; None explicitly selects
        /// the physical checkout regardless of branch (e.g. --worktree primary).
        expected_branch: Option<String>,
        name: String,
        kind: AgentKind,
        /// Model the CLI launches with; None = the CLI's own default.
        model: Option<String>,
        /// Reasoning effort the CLI launches with; None = the CLI's own default.
        effort: Option<String>,
        /// True when the user accepted the generated default name, marking
        /// the session eligible for one agent-driven auto-title (the CLI
        /// runs `nebula rename` on its first prompt).
        auto_title: bool,
        /// Initial task handed to the CLI as its positional prompt argument
        /// (`nebula agent new --prompt`). Skips warm-slot adoption — an
        /// already-booted CLI can't take a launch argument.
        prompt: Option<String>,
        /// Skipped when `Off` so the request keeps the wire shape older
        /// daemons decode; one that trails a value they don't know fails
        /// to decode there and drops the connection.
        #[serde(default, skip_serializing_if = "WriterClaim::is_off")]
        writer: WriterClaim,
    },
    /// Fire-and-forget: pre-spawn an agent CLI for this (worktree, kind) so
    /// the next CreateAgent adopts an already-booted session. Sent the
    /// moment the user picks the kind, before they type the name. No reply;
    /// a missing CLI or failed spawn silently degrades to a cold spawn.
    PrewarmAgent {
        worktree: WorktreeId,
        kind: AgentKind,
        /// Must match the CreateAgent that follows or the warm session is
        /// discarded (a CLI booted with the wrong model can't be adopted).
        model: Option<String>,
        effort: Option<String>,
    },
    /// Retired: the daemon ignores it. Kept so older clients still parse
    /// against the same PROTOCOL_VERSION.
    PrewarmWorktreeSessions {
        worktree: WorktreeId,
        cols: u16,
        rows: u16,
    },
    RenameAgent {
        req_id: u64,
        id: AgentId,
        name: String,
    },
    /// Agent-initiated one-shot title (`nebula rename` inside the session's
    /// CLI). Applies only while the session still awaits its auto-title;
    /// answered with Error (informational, not a fault) once a title —
    /// user- or agent-set — already sticks, so a user rename is never
    /// clobbered by a late or repeated agent attempt.
    AutoRenameAgent {
        req_id: u64,
        id: AgentId,
        name: String,
    },
    /// Re-home the agent row under another worktree of the same project.
    /// A live PTY is killed and respawned (resumed) in the new path — left
    /// running, its hooks would keep reporting the old checkout's cwd and
    /// the daemon would re-home the row right back.
    MoveAgent {
        req_id: u64,
        id: AgentId,
        worktree: WorktreeId,
    },
    /// Kills the PTY, sets archived=1.
    ArchiveAgent {
        req_id: u64,
        id: AgentId,
    },
    UnarchiveAgent {
        req_id: u64,
        id: AgentId,
    },
    /// Pin/unpin the agent in the sessions list (pure metadata; PTY untouched).
    SetAgentPinned {
        req_id: u64,
        id: AgentId,
        pinned: bool,
    },
    /// Delete only a non-archived agent in the explicitly selected project.
    DeleteAgentScoped {
        req_id: u64,
        id: AgentId,
        project: ProjectId,
    },
    DeleteAgent {
        req_id: u64,
        id: AgentId,
    },
    /// Respawn; resumes the stored session id (`claude --resume` /
    /// `codex resume` / `cursor-agent --resume`) when one is stored.
    RestartAgent {
        req_id: u64,
        id: AgentId,
    },
    CreateTerminal {
        req_id: u64,
        worktree: WorktreeId,
        /// Requested branch for a branch/session action; None explicitly selects
        /// the physical checkout regardless of branch (e.g. --worktree primary).
        expected_branch: Option<String>,
        name: Option<String>,
    },
    CreateNote {
        req_id: u64,
        owner: NoteOwner,
        text: String,
    },
    /// Rewrite a note's text.
    UpdateNote {
        req_id: u64,
        id: NoteId,
        text: String,
    },
    SetNoteDone {
        req_id: u64,
        id: NoteId,
        done: bool,
    },
    DeleteNote {
        req_id: u64,
        id: NoteId,
    },
    CreateTodo {
        req_id: u64,
        owner: TodoOwner,
        text: String,
    },
    /// Rewrite a todo's text.
    UpdateTodo {
        req_id: u64,
        id: TodoId,
        text: String,
    },
    SetTodoDone {
        req_id: u64,
        id: TodoId,
        done: bool,
    },
    /// Delete a todo; its child notes go with it (DB cascade).
    DeleteTodo {
        req_id: u64,
        id: TodoId,
    },
    /// Pin a URL to a worktree. `url` is normalized daemon-side (a bare
    /// `github.com/...` gains an https:// scheme) and refused if it can't be
    /// made into an http(s) URL.
    CreateLink {
        req_id: u64,
        worktree: WorktreeId,
        url: String,
    },
    /// Rewrite a link's URL (same normalization as CreateLink).
    UpdateLink {
        req_id: u64,
        id: LinkId,
        url: String,
    },
    DeleteLink {
        req_id: u64,
        id: LinkId,
    },
    RenameTerminal {
        req_id: u64,
        id: TerminalId,
        name: String,
    },
    CloseTerminal {
        req_id: u64,
        id: TerminalId,
    },

    /// Fire-and-forget opaque TUI blob (last selection etc.).
    SaveUiState {
        json: String,
    },

    /// Fire-and-forget: the user just opened this pull request, so
    /// everything up to `marker` has now been read.
    MarkPrSeen {
        url: String,
        marker: String,
    },

    /// One point-in-time memory reading — the daemon plus every live
    /// session's process subtree. Answered by `ServerEvent::Metrics` with
    /// the same req_id (not an Ack).
    GetMetrics {
        req_id: u64,
    },

    /// One plain-text reading of a live session's screen — scrollback tail
    /// included — rendered daemon-side at the session's current size, so
    /// nothing is attached, resized, or respawned by looking. Answered by
    /// `ServerEvent::SessionText`. With no live PTY it answers the screen
    /// saved when the session stopped, headed by a not-running line, or an
    /// Error when nothing was saved. `nebula agent read` is the caller.
    ReadSession {
        req_id: u64,
        session: SessionRef,
        /// Keep only the last N lines; None = everything retained.
        lines: Option<usize>,
    },

    /// Read-only source ownership check, answered by correlated Ack/Error.
    /// A relay's cached snapshot cannot establish that a source is settled.
    InspectCollect {
        /// Correlates the source response through the relay.
        req_id: u64,
        /// Source checkout id (used for host routing).
        worktree: WorktreeId,
        /// Branch whose writers must all be settled.
        branch: String,
    },

    /// Source-daemon guarded handoff. Ack is emitted only after this request
    /// observes a new prompt submission and its completed turn; a cached
    /// remote status is never sufficient. No lifecycle operation is performed.
    CollectHandoff {
        /// Correlates the completion response through relays.
        req_id: u64,
        /// Sole owning agent on the source daemon.
        id: AgentId,
        /// Reject if the source owner's revision differs from the caller's.
        expected_status_changed_at: i64,
        /// Expected checkout identity and branch, validated at the source.
        worktree: WorktreeId,
        /// Expected source branch, checked again before submission.
        branch: String,
        /// Explicit-file handoff instruction, passed as PTY text only.
        prompt: String,
        /// Maximum wait, in milliseconds; timeout leaves all sessions intact.
        timeout_ms: u64,
    },

    Shutdown,
}

/// CLI options of a safe worktree removal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalFlags {
    /// Delete archived sessions and exited terminals with the checkout.
    pub with_archived: bool,
}

/// How much of a pull request's conversation the user had already seen the
/// last time they opened it. `marker` is the newest thing anyone else had
/// posted at that moment, as GitHub's RFC 3339 stamp — those sort
/// lexicographically, so "arrived since" is a string compare and nebula
/// never has to consult a clock. Empty means the PR was opened while its
/// conversation was still empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSeen {
    pub url: String,
    pub marker: String,
}

/// Memory usage of one live session: the PTY child plus every descendant
/// (an agent CLI typically fans out into node workers, shells, MCP servers).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetrics {
    pub session: SessionRef,
    /// OS pid of the PTY child (the subtree's root).
    pub pid: u32,
    /// Resident set size summed over the whole subtree, bytes.
    pub rss_bytes: u64,
    /// Live processes in the subtree, the root included.
    pub procs: u32,
}

/// Daemon-side half of the metrics modal's data; the client stacks its own
/// RSS on top. Session subtrees are daemon descendants, so `daemon_rss_bytes`
/// counts the daemon process alone — the total stays double-count-free.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub daemon_pid: u32,
    pub daemon_rss_bytes: u64,
    /// Physical memory installed on the machine, bytes; 0 = unknown.
    pub system_total_bytes: u64,
    pub sessions: Vec<SessionMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerEvent {
    HelloOk {
        protocol_version: u32,
        daemon_pid: u32,
    },
    Incompatible {
        daemon_protocol_version: u32,
    },
    Snapshot {
        workspaces: Vec<Workspace>,
        /// The open workspace clients scope their project lists to.
        active_workspace: WorkspaceId,
        projects: Vec<Project>,
        worktrees: Vec<Worktree>,
        agents: Vec<Agent>,
        terminals: Vec<TerminalTab>,
        notes: Vec<Note>,
        todos: Vec<Todo>,
        links: Vec<Link>,
        /// How far the user has read into each pull request they've opened.
        pr_seen: Vec<PrSeen>,
        ui_state: Option<String>,
    },

    Ack {
        req_id: u64,
        created: Option<EntityId>,
    },
    Error {
        req_id: Option<u64>,
        message: String,
    },
    /// Verdict lines for a request that asked for them (safe worktree
    /// removal, prune); precedes that request's Ack or Error.
    Report {
        req_id: u64,
        lines: Vec<String>,
    },

    // -- deltas (pushed to all subscribers) --
    EntityUpserted {
        entity: Entity,
    },
    EntityRemoved {
        id: EntityId,
    },
    /// A different workspace was opened (`nebula workspace open`, or the
    /// TUI's switcher — daemon-global, so every client follows).
    ActiveWorkspaceChanged {
        id: WorkspaceId,
    },
    StatusChanged {
        agent: AgentId,
        status: AgentStatus,
        /// Epoch ms the change was stamped with (matches the persisted
        /// `status_changed_at`, so clients regroup consistently).
        changed_at: i64,
    },

    // -- PTY plane (only to clients attached to that session) --
    /// Ring replay on attach; client resets its parser before applying.
    Scrollback {
        session: SessionRef,
        base_seq: u64,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// Live coalesced output. `seq` = byte offset of the first byte.
    Output {
        session: SessionRef,
        seq: u64,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    SessionExited {
        session: SessionRef,
        exit_code: Option<i32>,
    },
    /// The child's kitty-keyboard-protocol flags changed (or, right after
    /// Scrollback on attach, the current value). 0 = legacy encoding.
    KittyFlags {
        session: SessionRef,
        flags: u8,
    },

    /// Reply to `ClientRequest::GetMetrics`.
    Metrics {
        req_id: u64,
        snapshot: MetricsSnapshot,
    },

    /// Reply to `ClientRequest::ReadSession`: the session's screen (and
    /// scrollback tail) as plain text, plus the grid it was rendered at
    /// (0×0 for a stopped session's saved screen).
    SessionText {
        req_id: u64,
        cols: u16,
        rows: u16,
        text: String,
    },
}
