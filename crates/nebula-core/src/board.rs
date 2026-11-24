use crate::entities::{AgentKind, AgentStatus};
use crate::ids::{AgentId, ArtifactId, DispatchId, NoteId, TodoId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Backlog,
    Next,
    Doing,
    Review,
    Done,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Backlog => "backlog",
            Stage::Next => "next",
            Stage::Doing => "doing",
            Stage::Review => "review",
            Stage::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "backlog" => Stage::Backlog,
            "next" => Stage::Next,
            "doing" => Stage::Doing,
            "review" => Stage::Review,
            "done" => Stage::Done,
            _ => return None,
        })
    }
}

/// Closed because stage automation matches on it; never inherited by a child.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Worker,
    Review,
    Fix,
    Verify,
    Research,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Review => "review",
            Role::Fix => "fix",
            Role::Verify => "verify",
            Role::Research => "research",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "worker" => Role::Worker,
            "review" => Role::Review,
            "fix" => Role::Fix,
            "verify" => Role::Verify,
            "research" => Role::Research,
            _ => return None,
        })
    }
}

/// A card as a caller names it: its board number (optionally with the board
/// host it was printed for) or its ULID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardRef {
    Number { number: u64, board: Option<String> },
    Id(TodoId),
}

/// Accepts `#12`, `12`, `12@leaseweb`, `#12@leaseweb` and a ULID.
pub fn parse_card_ref(text: &str) -> Option<CardRef> {
    if let Ok(ulid) = ulid::Ulid::from_string(text) {
        return text
            .starts_with(|c| ('0'..='7').contains(&c))
            .then(|| CardRef::Id(TodoId(ulid.to_string())));
    }
    let text = text.strip_prefix('#').unwrap_or(text);
    let (number, board) = match text.split_once('@') {
        Some((number, board)) => (number, Some(board)),
        None => (text, None),
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number: u64 = number.parse().ok().filter(|n| *n > 0)?;
    let board = match board {
        None => None,
        Some(host)
            if !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b)) =>
        {
            Some(host.to_string())
        }
        Some(_) => return None,
    };
    Some(CardRef::Number { number, board })
}

/// The ref a card prints as: `#12` on the board host, `#12@<board>` elsewhere.
pub fn card_label(number: u64, board: Option<&str>) -> String {
    match board {
        Some(host) => format!("#{number}@{host}"),
        None => format!("#{number}"),
    }
}

/// Who wrote a card, note or artifact: the session when one did, else the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    pub host: String,
    pub agent: Option<AgentId>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    pub host: String,
    pub branch: String,
    pub path: String,
    /// A linked worktree rather than the project's primary checkout.
    pub worktree: bool,
}

/// One checkout in a host's inventory, as the board sidebar lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCheckout {
    pub project: String,
    pub branch: String,
    pub path: String,
    pub worktree: bool,
}

/// A session on any host; the id alone is ambiguous across daemons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAt {
    pub host: String,
    pub id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLink {
    pub host: String,
    pub id: AgentId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: TodoId,
    pub number: u64,
    /// [`card_label`] for the host the reply is printed on.
    #[serde(rename = "ref")]
    pub label: String,
    pub title: String,
    pub stage: Stage,
    /// A project name; None is a home card.
    pub project: Option<String>,
    pub proposed: bool,
    pub archived: bool,
    pub created_by: Author,
    pub checkout: Option<Checkout>,
    pub blocked_by: Vec<String>,
}

/// One entry of a card's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardNote {
    pub id: NoteId,
    pub card: TodoId,
    pub text: String,
    pub author: Author,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSession {
    pub host: String,
    pub id: AgentId,
    pub name: String,
    pub role: Option<Role>,
    pub kind: AgentKind,
    pub status: AgentStatus,
    pub status_changed_at: i64,
    pub writer: bool,
    pub alive: bool,
    /// The host has not reported recently, so the row may be out of date.
    pub stale: bool,
}

/// `todo show`: the card's fields with its sessions, artifacts and log beside them.
#[derive(Debug, Clone, PartialEq)]
pub struct CardShow {
    pub card: Card,
    pub sessions: Vec<CardSession>,
    pub artifacts: Vec<Artifact>,
    pub log: Vec<CardNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactContent {
    /// Files stored under the publishing daemon's data dir; `entry` opens first.
    Files {
        host: String,
        entry: String,
    },
    Url {
        url: String,
    },
    Body {
        body: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    /// Free text validated by the CLI, so a new kind needs no protocol bump.
    pub kind: String,
    pub title: String,
    pub card: Option<TodoId>,
    pub project: Option<String>,
    pub session: Option<SessionLink>,
    pub checkout: Option<Checkout>,
    pub content: ArtifactContent,
    /// `rag_note` holds a decision's RAG note id.
    pub meta: serde_json::Map<String, serde_json::Value>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardHost {
    pub host: String,
    pub writers: u32,
    pub writer_slots: Option<u32>,
    pub checkouts: Vec<HostCheckout>,
    pub needs_you: u32,
    pub stale: bool,
}

/// What changed since the viewer's last visit (`--away-since`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Away {
    pub finished: Vec<TodoId>,
    pub needs_you: Vec<SessionAt>,
    pub decisions: Vec<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardPage {
    pub cards: Vec<Card>,
    pub hosts: Vec<BoardHost>,
    pub away: Away,
}

/// A board poll's answer. `version` is `<board_id>.<version>`, so a rebuilt
/// database never matches an old cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardPoll {
    Unchanged {
        version: String,
    },
    /// One page; `next` continues it at the same version, since one frame
    /// is capped at MAX_FRAME_LEN.
    Board {
        version: String,
        board: BoardPage,
        next: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    Pending,
    Started,
    Failed,
}

/// `todo run`'s receipt; a rerun with the same dispatch id returns the first one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReceipt {
    pub dispatch: DispatchId,
    pub state: DispatchState,
    pub session: Option<SessionAt>,
    pub card: TodoId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    pub id: DispatchId,
    pub card: CardRef,
    pub host: String,
    pub role: Role,
    pub kind: AgentKind,
    /// `new` or a branch name, as `todo run --worktree` takes it.
    pub worktree: String,
}

/// A card-linked session as its host reports it to the board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub agent_id: AgentId,
    pub host: String,
    pub card_id: TodoId,
    pub role: Option<Role>,
    pub kind: AgentKind,
    pub name: String,
    pub parent: Option<SessionAt>,
    pub status: AgentStatus,
    pub status_changed_at: i64,
    pub writer: bool,
    pub alive: bool,
    pub archived: bool,
    pub branch: Option<String>,
    pub dispatch_id: Option<DispatchId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReport {
    pub host: String,
    pub writers: u32,
    pub writer_slots: Option<u32>,
    pub checkouts: Vec<HostCheckout>,
    pub sessions: Vec<SessionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectScope {
    All,
    Named(Vec<String>),
}

/// Applied inside the read transaction, an id lookup included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardScope {
    pub projects: ProjectScope,
    pub include_home: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardRead {
    Board {
        scope: BoardScope,
        since: Option<String>,
        away_since: Option<i64>,
        page: Option<String>,
    },
    Cards {
        scope: BoardScope,
        stage: Option<Stage>,
        include_archived: bool,
        page: Option<String>,
    },
    Card {
        scope: BoardScope,
        card: CardRef,
    },
    Artifacts {
        scope: BoardScope,
        card: Option<CardRef>,
        kind: Option<String>,
        query: Option<String>,
        page: Option<String>,
    },
    Artifact {
        scope: BoardScope,
        id: ArtifactId,
    },
}

/// Carries its client-minted id, so a rerun creates nothing twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardDraft {
    pub id: TodoId,
    pub title: String,
    pub project: Option<String>,
    pub stage: Stage,
    pub proposed: bool,
    pub created_by: Author,
    pub checkout: Option<Checkout>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Placement {
    Home,
    Project(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BoardOp {
    Read(BoardRead),
    AddCard(CardDraft),
    EditCard {
        card: CardRef,
        title: Option<String>,
        project: Option<Placement>,
    },
    /// Refused when `expect` is set and the card has since left that stage.
    Move {
        card: CardRef,
        stage: Stage,
        expect: Option<Stage>,
    },
    Archive {
        card: CardRef,
    },
    Proposal {
        card: CardRef,
        accept: bool,
    },
    Block {
        card: CardRef,
        by: CardRef,
        on: bool,
    },
    AddNote {
        id: NoteId,
        card: CardRef,
        text: String,
        author: Author,
    },
    Publish(Artifact),
    RemoveArtifact {
        id: ArtifactId,
    },
    BeginRun(Dispatch),
    EndRun {
        dispatch: DispatchId,
        agent: Option<AgentId>,
        error: Option<String>,
    },
    Report(HostReport),
    /// A write allowed only on cards inside `scope`, checked in the write's
    /// own transaction.
    Scoped {
        scope: BoardScope,
        op: Box<BoardOp>,
    },
}

impl BoardOp {
    pub fn card_refs_mut(&mut self) -> Vec<&mut CardRef> {
        match self {
            BoardOp::Read(BoardRead::Card { card, .. }) => vec![card],
            BoardOp::Read(BoardRead::Artifacts { card, .. }) => card.iter_mut().collect(),
            BoardOp::Read(
                BoardRead::Board { .. } | BoardRead::Cards { .. } | BoardRead::Artifact { .. },
            ) => vec![],
            BoardOp::EditCard { card, .. }
            | BoardOp::Move { card, .. }
            | BoardOp::Archive { card }
            | BoardOp::Proposal { card, .. }
            | BoardOp::AddNote { card, .. } => vec![card],
            BoardOp::Block { card, by, .. } => vec![card, by],
            BoardOp::BeginRun(dispatch) => vec![&mut dispatch.card],
            BoardOp::Scoped { op, .. } => op.card_refs_mut(),
            BoardOp::AddCard(_)
            | BoardOp::Publish(_)
            | BoardOp::RemoveArtifact { .. }
            | BoardOp::EndRun { .. }
            | BoardOp::Report(_) => vec![],
        }
    }
}

/// Why the board refused an op. It leads the `Error` message, so it crosses
/// the wire and the board link unchanged, and the CLI turns it into an exit
/// code the gateway maps to 404 or 409.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NotFound,
    Conflict,
    /// The board link failed before the request was written.
    Unreachable,
    /// The board link failed after the request was written.
    OutcomeUnknown,
}

impl Refusal {
    const ALL: [Refusal; 4] = [
        Refusal::NotFound,
        Refusal::Conflict,
        Refusal::Unreachable,
        Refusal::OutcomeUnknown,
    ];

    fn tag(self) -> &'static str {
        match self {
            Refusal::NotFound => "not found: ",
            Refusal::Conflict => "conflict: ",
            Refusal::Unreachable => "; nothing was sent",
            Refusal::OutcomeUnknown => "; outcome unknown; rerun the same command",
        }
    }

    fn trails(self) -> bool {
        matches!(self, Refusal::Unreachable | Refusal::OutcomeUnknown)
    }

    pub fn message(self, why: impl std::fmt::Display) -> String {
        match self.trails() {
            true => format!("{why}{}", self.tag()),
            false => format!("{}{why}", self.tag()),
        }
    }

    pub fn of(message: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| match r.trails() {
            true => message.ends_with(r.tag()),
            false => message.starts_with(r.tag()),
        })
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Refusal::Unreachable => 3,
            Refusal::OutcomeUnknown => 4,
            Refusal::NotFound => 5,
            Refusal::Conflict => 6,
        }
    }
}

/// Failures travel as the ordinary `ServerEvent::Error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BoardResult {
    Board(BoardPoll),
    Cards(Page<Card>),
    Card(Card),
    Show(Box<CardShow>),
    Artifacts(Page<Artifact>),
    Artifact(Artifact),
    Note(CardNote),
    Run(RunReceipt),
    Done,
}

/// The column a card moves to when one of its session rows changes, or None.
/// `others` are the card's other rows. Fires only on a transition, so a
/// duplicate or late report never undoes a manual move, and never leaves
/// review or done. Only a live, unarchived worker moves its card.
pub fn auto_stage(
    stage: Stage,
    before: Option<&SessionRow>,
    after: &SessionRow,
    others: &[SessionRow],
) -> Option<Stage> {
    if before == Some(after)
        || before.is_some_and(|b| after.status_changed_at < b.status_changed_at)
        || after.role != Some(Role::Worker)
        || !after.alive
        || after.archived
    {
        return None;
    }
    let live_unfinished_worker = |row: &SessionRow| {
        row.agent_id != after.agent_id
            && row.role == Some(Role::Worker)
            && row.alive
            && !row.archived
            && row.status != AgentStatus::Finished
    };
    match stage {
        Stage::Backlog | Stage::Next
            if before.is_none() && after.status != AgentStatus::Finished =>
        {
            Some(Stage::Doing)
        }
        Stage::Doing
            if after.status == AgentStatus::Finished
                && before.is_none_or(|b| b.status != AgentStatus::Finished)
                && !others.iter().any(live_unfinished_worker) =>
        {
            Some(Stage::Review)
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Failed,
    Skipped,
}

/// An e2e-run artifact's `result.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eRun {
    pub title: String,
    pub status: RunStatus,
    pub started_at: i64,
    pub duration_ms: u64,
    pub steps: Vec<E2eStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct E2eStep {
    pub name: String,
    pub status: RunStatus,
    pub duration_ms: u64,
    /// Path relative to the artifact directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Parses `result.json` and checks every screenshot it names is among
/// `files`, the artifact's relative paths.
pub fn validate_e2e_run(result_json: &str, files: &BTreeSet<String>) -> Result<E2eRun, String> {
    let value: serde_json::Value =
        serde_json::from_str(result_json).map_err(|e| format!("result.json: {e}"))?;
    let objects = value.is_object()
        && value["steps"]
            .as_array()
            .is_some_and(|steps| steps.iter().all(serde_json::Value::is_object));
    if !objects {
        return Err("result.json: the run and each step must be JSON objects".into());
    }
    let run: E2eRun = serde_json::from_value(value).map_err(|e| format!("result.json: {e}"))?;
    if let Some(missing) = run
        .steps
        .iter()
        .filter_map(|step| step.screenshot.as_ref())
        .find(|shot| !files.contains(*shot))
    {
        return Err(format!(
            "result.json names {missing}, which is not in the run"
        ));
    }
    Ok(run)
}

// Three board types print as the CLI table has them (flattened or untagged)
// but cross the wire externally tagged: MessagePack structs are positional,
// so an untagged `{"url"}` and `{"body"}` would decode alike.

#[derive(Serialize, Deserialize)]
#[serde(remote = "ArtifactContent", untagged)]
enum ContentJson {
    Files { host: String, entry: String },
    Url { url: String },
    Body { body: String },
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "ArtifactContent")]
enum ContentWire {
    Files { host: String, entry: String },
    Url { url: String },
    Body { body: String },
}

impl Serialize for ArtifactContent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            ContentJson::serialize(self, s)
        } else {
            ContentWire::serialize(self, s)
        }
    }
}

impl<'de> Deserialize<'de> for ArtifactContent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            ContentJson::deserialize(d)
        } else {
            ContentWire::deserialize(d)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "CardShow")]
struct CardShowJson {
    #[serde(flatten)]
    card: Card,
    sessions: Vec<CardSession>,
    artifacts: Vec<Artifact>,
    log: Vec<CardNote>,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "CardShow")]
struct CardShowWire {
    card: Card,
    sessions: Vec<CardSession>,
    artifacts: Vec<Artifact>,
    log: Vec<CardNote>,
}

impl Serialize for CardShow {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if s.is_human_readable() {
            CardShowJson::serialize(self, s)
        } else {
            CardShowWire::serialize(self, s)
        }
    }
}

impl<'de> Deserialize<'de> for CardShow {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if d.is_human_readable() {
            CardShowJson::deserialize(d)
        } else {
            CardShowWire::deserialize(d)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "BoardPoll")]
enum BoardPollWire {
    Unchanged {
        version: String,
    },
    Board {
        version: String,
        board: BoardPage,
        next: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct BoardPollJson {
    version: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    unchanged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    board: Option<BoardPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

impl Serialize for BoardPoll {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if !s.is_human_readable() {
            return BoardPollWire::serialize(self, s);
        }
        let json = match self.clone() {
            BoardPoll::Unchanged { version } => BoardPollJson {
                version,
                unchanged: true,
                board: None,
                next: None,
            },
            BoardPoll::Board {
                version,
                board,
                next,
            } => BoardPollJson {
                version,
                unchanged: false,
                board: Some(board),
                next,
            },
        };
        json.serialize(s)
    }
}

impl<'de> Deserialize<'de> for BoardPoll {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        if !d.is_human_readable() {
            return BoardPollWire::deserialize(d);
        }
        let json = BoardPollJson::deserialize(d)?;
        match (json.unchanged, json.board) {
            (true, None) => Ok(BoardPoll::Unchanged {
                version: json.version,
            }),
            (false, Some(board)) => Ok(BoardPoll::Board {
                version: json.version,
                board,
                next: json.next,
            }),
            _ => Err(serde::de::Error::custom(
                "a board reply has exactly one of unchanged and board",
            )),
        }
    }
}
