//! The board on its host (`board = "local"`): every board op runs in one
//! SQLite transaction, so a read sees one version and a write bumps it once.

use crate::store::Store;
use anyhow::{bail, Result};
use nebula_core::board::{
    card_label, Author, Away, BoardHost, BoardOp, BoardPage, BoardPoll, BoardRead, BoardResult,
    BoardScope, Card, CardDraft, CardNote, CardRef, CardSession, CardShow, Dispatch, DispatchState,
    NeedsYou, Page, Placement, ProjectScope, Refusal, RunReceipt, SessionAt, Stage,
};
use nebula_core::{
    AgentId, AgentKind, AgentStatus, ArtifactId, DispatchId, NoteId, TodoId, MAX_FRAME_LEN,
};
use rusqlite::{params, OptionalExtension, Row, Transaction};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Bytes one page may carry, hosts and the away digest included; the rest
/// of the frame is headroom for the envelope.
pub(crate) fn page_budget() -> usize {
    std::env::var("NEBULA_BOARD_PAGE_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_FRAME_LEN as usize / 2)
}

pub(crate) fn serve(store: &Store, links: &HostLinks, op: BoardOp) -> Result<BoardResult> {
    let publishes = matches!(op, BoardOp::Publish(_));
    let result = store.board_transaction(|tx| apply(tx, links, op))?;
    if let (true, BoardResult::Artifact(artifact)) = (publishes, &result) {
        if artifact.kind == nebula_core::board::DECISION_KIND && !artifact.meta.contains_key("rag_note") {
            crate::rag_copy::kick();
        }
    }
    Ok(result)
}

fn apply(tx: &Transaction, links: &HostLinks, op: BoardOp) -> Result<BoardResult> {
    match op {
        BoardOp::Read(read) => read_op(tx, links, read),
        BoardOp::AddCard(draft) => add_card(tx, draft).map(BoardResult::Card),
        BoardOp::EditCard {
            card,
            title,
            project,
        } => {
            let id = resolve(tx, &card)?;
            if let Some(title) = title {
                let title = non_empty(&title, "a card title")?;
                tx.execute(
                    "UPDATE todos SET title = ?2 WHERE id = ?1 AND title IS NOT ?2",
                    params![id.as_str(), title],
                )?;
            }
            if let Some(placement) = project {
                let project = match placement {
                    Placement::Home => None,
                    Placement::Project(name) => Some(name),
                };
                tx.execute(
                    "UPDATE todos SET project = ?2 WHERE id = ?1 AND project IS NOT ?2",
                    params![id.as_str(), project],
                )?;
            }
            card_by_id(tx, &id).map(BoardResult::Card)
        }
        BoardOp::Move {
            card,
            stage,
            expect,
        } => {
            let id = resolve(tx, &card)?;
            let current = card_by_id(tx, &id)?;
            if let Some(expected) = expect.filter(|e| *e != current.stage) {
                bail!(Refusal::Conflict.message(format!(
                    "{} is in {}, not {}",
                    current.label,
                    current.stage.as_str(),
                    expected.as_str()
                )));
            }
            if current.stage != stage {
                tx.execute(
                    "UPDATE todos SET stage = ?2, stage_changed_at = ?3 WHERE id = ?1",
                    params![id.as_str(), stage.as_str(), now_ms()],
                )?;
            }
            card_by_id(tx, &id).map(BoardResult::Card)
        }
        BoardOp::Archive { card } => {
            let id = resolve(tx, &card)?;
            archive(tx, &id)?;
            card_by_id(tx, &id).map(BoardResult::Card)
        }
        BoardOp::Proposal { card, accept } => {
            let id = resolve(tx, &card)?;
            if accept {
                tx.execute(
                    "UPDATE todos SET proposed = 0 WHERE id = ?1 AND proposed = 1",
                    params![id.as_str()],
                )?;
            } else {
                let current = card_by_id(tx, &id)?;
                if !current.proposed {
                    bail!(Refusal::Conflict.message(format!(
                        "{} is not a proposal; archive it instead",
                        current.label
                    )));
                }
                archive(tx, &id)?;
            }
            card_by_id(tx, &id).map(BoardResult::Card)
        }
        BoardOp::Block { card, by, on } => {
            let id = resolve(tx, &card)?;
            let by = resolve(tx, &by)?;
            if on {
                block(tx, &id, &by)?;
            } else {
                tx.execute(
                    "DELETE FROM card_blocks WHERE card_id = ?1 AND by_id = ?2",
                    params![id.as_str(), by.as_str()],
                )?;
            }
            card_by_id(tx, &id).map(BoardResult::Card)
        }
        BoardOp::AddNote {
            id,
            card,
            text,
            author,
        } => {
            let note = add_note(tx, id, &card, &text, &author)?;
            card_by_id(tx, &note.card).map(BoardResult::Card)
        }
        BoardOp::Report(report) => crate::board_report::apply(tx, report),
        BoardOp::Publish(artifact) => {
            crate::artifacts::publish(tx, artifact).map(BoardResult::Artifact)
        }
        BoardOp::RemoveArtifact { id } => {
            crate::artifacts::remove(tx, &id).map(BoardResult::Artifact)
        }
        BoardOp::BeginRun(dispatch) => begin_run(tx, dispatch).map(BoardResult::Run),
        BoardOp::EndRun {
            agent: Some(_),
            error: Some(_),
            ..
        } => bail!("a run ends with its session or its error, not both"),
        BoardOp::EndRun {
            dispatch,
            agent,
            error,
        } => end_run(tx, &dispatch, agent, error).map(BoardResult::Run),
        BoardOp::Scoped { scope, mut op } => {
            within(tx, &scope, &mut op)?;
            apply(tx, links, *op)
        }
    }
}

fn begin_run(tx: &Transaction, dispatch: Dispatch) -> Result<RunReceipt> {
    let card = resolve(tx, &dispatch.card)?;
    let known = receipt(tx, &dispatch.id)?;
    if let Some(known) = &known {
        if known.card != card {
            bail!(Refusal::Conflict.message(format!(
                "dispatch {} runs another card",
                dispatch.id.as_str()
            )));
        }
        let (host, role, kind, worktree): (String, String, String, String) = tx.query_row(
            "SELECT host, role, kind, worktree FROM dispatches WHERE id = ?1",
            params![dispatch.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        if !host.eq_ignore_ascii_case(&dispatch.host)
            || (role.as_str(), kind.as_str(), worktree.as_str())
                != (
                    dispatch.role.as_str(),
                    dispatch.kind.as_str(),
                    dispatch.worktree.as_str(),
                )
        {
            bail!(Refusal::Conflict.message(format!(
                "dispatch {} runs with --host {host} --role {role} --kind {kind} --worktree {worktree}; rerun it with those",
                dispatch.id.as_str()
            )));
        }
        if known.state != DispatchState::Pending {
            return Ok(known.clone());
        }
        let launched: Option<(String, Option<String>, String)> = tx
            .query_row(
                "SELECT agent_id, role, kind FROM board_sessions
                 WHERE dispatch_id = ?1 AND card_id = ?2 AND host = ?3 COLLATE NOCASE
                   AND alive = 1 AND archived = 0",
                params![dispatch.id.as_str(), card.as_str(), host],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((agent, role, kind)) = launched {
            if role.as_deref() != Some(dispatch.role.as_str()) || kind != dispatch.kind.as_str() {
                bail!(Refusal::Conflict.message(format!(
                    "dispatch {} started {agent} with another role or kind",
                    dispatch.id.as_str()
                )));
            }
            return end_run(tx, &dispatch.id, Some(AgentId(agent)), None);
        }
    }
    let found = card_by_id(tx, &card)?;
    if found.archived {
        bail!(Refusal::Conflict.message(format!("{} is archived", found.label)));
    }
    let Some(project) = found.project else {
        bail!(Refusal::Conflict.message(format!(
            "{} is a home card; give it a project to run it",
            found.label
        )));
    };
    let open: Vec<&str> = found
        .blocked_by
        .iter()
        .filter(|b| !found.cleared_blockers.contains(b))
        .map(String::as_str)
        .collect();
    if !open.is_empty() {
        bail!(Refusal::Conflict.message(format!(
            "{} is blocked by {}",
            found.label,
            open.join(", ")
        )));
    }
    if let Some(known) = known {
        return Ok(known);
    }
    let here = nebula_core::host::hostname();
    let host = match here.eq_ignore_ascii_case(&dispatch.host) {
        true => Some(here),
        false => tx
            .query_row(
                "SELECT host FROM board_hosts WHERE host = ?1 COLLATE NOCASE",
                params![dispatch.host],
                |r| r.get::<_, String>(0),
            )
            .optional()?,
    };
    let Some(host) = host else {
        bail!(Refusal::Conflict.message(format!(
            "no host {} has reported to the board; --host takes a reported host name",
            dispatch.host
        )));
    };
    tx.execute(
        "INSERT INTO dispatches (id, card_id, host, role, kind, worktree, state, created_at, project)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8)",
        params![
            dispatch.id.as_str(),
            card.as_str(),
            host,
            dispatch.role.as_str(),
            dispatch.kind.as_str(),
            dispatch.worktree,
            now_ms(),
            project
        ],
    )?;
    receipt(tx, &dispatch.id)?.ok_or_else(|| anyhow::anyhow!("the dispatch vanished"))
}

fn end_run(
    tx: &Transaction,
    id: &DispatchId,
    agent: Option<AgentId>,
    error: Option<String>,
) -> Result<RunReceipt> {
    let known = receipt(tx, id)?.ok_or_else(|| {
        anyhow::anyhow!(Refusal::NotFound.message(format!("no dispatch {}", id.as_str())))
    })?;
    if known.state != DispatchState::Pending {
        return Ok(known);
    }
    match (agent, error) {
        (Some(agent), _) => tx.execute(
            "UPDATE dispatches SET state = 'started', agent_id = ?2 WHERE id = ?1",
            params![id.as_str(), agent.as_str()],
        )?,
        (None, Some(error)) => tx.execute(
            "UPDATE dispatches SET state = 'failed', error = ?2 WHERE id = ?1",
            params![id.as_str(), error],
        )?,
        (None, None) => bail!("a run ends with its session or its error"),
    };
    receipt(tx, id)?.ok_or_else(|| anyhow::anyhow!("the dispatch vanished"))
}

fn receipt(tx: &Transaction, id: &DispatchId) -> Result<Option<RunReceipt>> {
    Ok(tx
        .query_row(
            "SELECT card_id, host, state, agent_id, project FROM dispatches WHERE id = ?1",
            params![id.as_str()],
            |r| {
                let host: String = r.get(1)?;
                let state: String = r.get(2)?;
                Ok(RunReceipt {
                    dispatch: id.clone(),
                    state: match state.as_str() {
                        "started" => DispatchState::Started,
                        "failed" => DispatchState::Failed,
                        _ => DispatchState::Pending,
                    },
                    session: r.get::<_, Option<String>>(3)?.map(|agent| SessionAt {
                        host,
                        id: AgentId(agent),
                    }),
                    card: TodoId(r.get(0)?),
                    host: r.get(1)?,
                    project: r.get(4)?,
                })
            },
        )
        .optional()?)
}

fn within(tx: &Transaction, scope: &BoardScope, op: &mut BoardOp) -> Result<()> {
    if !matches!(
        op,
        BoardOp::EditCard { .. }
            | BoardOp::Move { .. }
            | BoardOp::Archive { .. }
            | BoardOp::Proposal { .. }
            | BoardOp::Block { .. }
            | BoardOp::AddNote { .. }
            | BoardOp::BeginRun(_)
    ) {
        bail!("a scoped op wraps one card write; reads and other ops carry their own scope");
    }
    for card in op.card_refs_mut() {
        let id = resolve(tx, card)?;
        if !in_scope(scope, card_by_id(tx, &id)?.project.as_deref()) {
            return Err(not_found(card));
        }
    }
    if let BoardOp::BeginRun(dispatch) = op {
        if let Some(project) = receipt(tx, &dispatch.id)?.and_then(|r| r.project) {
            if !in_scope(scope, Some(&project)) {
                bail!(Refusal::NotFound.message(format!(
                    "dispatch {} runs in project \"{project}\", outside the write scope",
                    dispatch.id.as_str()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn non_empty<'a>(text: &'a str, what: &str) -> Result<&'a str> {
    match text.trim() {
        "" => bail!("{what} cannot be empty"),
        text => Ok(text),
    }
}

fn not_found(card: &CardRef) -> anyhow::Error {
    let named = match card {
        CardRef::Number { number, board } => card_label(*number, board.as_deref()),
        CardRef::Id(id) => id.to_string(),
    };
    anyhow::anyhow!(Refusal::NotFound.message(format!("no card {named}")))
}

pub(crate) fn resolve(tx: &Transaction, card: &CardRef) -> Result<TodoId> {
    let id: Option<String> = match card {
        CardRef::Number {
            board: Some(host), ..
        } if *host != nebula_core::host::hostname() => None,
        CardRef::Number { number, .. } => tx
            .query_row(
                "SELECT id FROM todos WHERE number = ?1 AND stage IS NOT NULL",
                params![*number as i64],
                |r| r.get(0),
            )
            .optional()?,
        CardRef::Id(id) => tx
            .query_row(
                "SELECT id FROM todos WHERE id = ?1 AND stage IS NOT NULL",
                params![id.as_str()],
                |r| r.get(0),
            )
            .optional()?,
    };
    id.map(TodoId).ok_or_else(|| not_found(card))
}

fn archive(tx: &Transaction, id: &TodoId) -> Result<()> {
    tx.execute(
        "UPDATE todos SET archived_at = ?2 WHERE id = ?1 AND archived_at IS NULL",
        params![id.as_str(), now_ms()],
    )?;
    Ok(())
}

fn block(tx: &Transaction, card: &TodoId, by: &TodoId) -> Result<()> {
    if card == by {
        bail!(Refusal::Conflict.message("a card cannot block itself"));
    }
    let cycle: bool = tx.query_row(
        "WITH RECURSIVE upstream(id) AS (
           SELECT by_id FROM card_blocks WHERE card_id = ?1
           UNION SELECT b.by_id FROM card_blocks b JOIN upstream u ON b.card_id = u.id
         ) SELECT EXISTS (SELECT 1 FROM upstream WHERE id = ?2)",
        params![by.as_str(), card.as_str()],
        |r| r.get(0),
    )?;
    if cycle {
        bail!(Refusal::Conflict
            .message("that block would close a cycle, and no card in it could ever run"));
    }
    tx.execute(
        "INSERT OR IGNORE INTO card_blocks (card_id, by_id) VALUES (?1, ?2)",
        params![card.as_str(), by.as_str()],
    )?;
    Ok(())
}

fn add_card(tx: &Transaction, draft: CardDraft) -> Result<Card> {
    let known: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM todos WHERE id = ?1 AND stage IS NOT NULL",
            params![draft.id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    if known.is_some() {
        return card_by_id(tx, &draft.id);
    }
    let title = non_empty(&draft.title, "a card title")?;
    let number: i64 = tx.query_row("SELECT COALESCE(MAX(number), 0) + 1 FROM todos", [], |r| {
        r.get(0)
    })?;
    let now = now_ms();
    tx.execute(
        "INSERT INTO todos (id, sort_order, created_at, number, title, stage, project, checkout, created_by, proposed, stage_changed_at)
         VALUES (?1, ?3, ?4, ?3, ?2, ?5, ?6, ?7, ?8, ?9, ?4)",
        params![
            draft.id.as_str(),
            title,
            number,
            now,
            draft.stage.as_str(),
            draft.project,
            draft.checkout.map(|c| serde_json::to_string(&c)).transpose()?,
            serde_json::to_string(&draft.created_by)?,
            draft.proposed as i64,
        ],
    )?;
    card_by_id(tx, &draft.id)
}

pub(crate) fn add_note(
    tx: &Transaction,
    id: NoteId,
    card: &CardRef,
    text: &str,
    author: &Author,
) -> Result<CardNote> {
    let card = resolve(tx, card)?;
    let known = note_by_id(tx, &id)?;
    if let Some(note) = known {
        if note.card != card {
            bail!(Refusal::Conflict
                .message(format!("note {} was added to another card", id.as_str())));
        }
        return Ok(note);
    }
    let text = non_empty(text, "a note")?;
    tx.execute(
        "INSERT INTO notes (id, todo_id, text, sort_order, created_at, author)
         VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(sort_order) + 1, 0) FROM notes WHERE todo_id = ?2), ?4, ?5)",
        params![
            id.as_str(),
            card.as_str(),
            text,
            now_ms(),
            serde_json::to_string(author)?
        ],
    )?;
    note_by_id(tx, &id)?.ok_or_else(|| anyhow::anyhow!("the note vanished"))
}

const NOTE_COLUMNS: &str = "id, todo_id, text, author, created_at";

fn note_from_row(r: &Row) -> rusqlite::Result<CardNote> {
    Ok(CardNote {
        id: NoteId(r.get(0)?),
        card: TodoId(r.get(1)?),
        text: r.get(2)?,
        author: json_column(r, 3)?,
        created_at: r.get(4)?,
    })
}

fn note_by_id(tx: &Transaction, id: &NoteId) -> Result<Option<CardNote>> {
    Ok(tx
        .query_row(
            &format!("SELECT {NOTE_COLUMNS} FROM notes WHERE id = ?1 AND author IS NOT NULL"),
            params![id.as_str()],
            note_from_row,
        )
        .optional()?)
}

pub(crate) fn json_column<T: serde::de::DeserializeOwned>(
    r: &Row,
    i: usize,
) -> rusqlite::Result<T> {
    let text: String = r.get(i)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into())
    })
}

pub(crate) fn optional_json<T: serde::de::DeserializeOwned>(
    r: &Row,
    i: usize,
) -> rusqlite::Result<Option<T>> {
    match r.get::<_, Option<String>>(i)? {
        None => Ok(None),
        Some(_) => json_column(r, i).map(Some),
    }
}

const CARD_COLUMNS: &str =
    "id, number, title, stage, project, proposed, archived_at, created_by, checkout, created_at, stage_changed_at";

fn card_from_row(r: &Row) -> rusqlite::Result<Card> {
    let number: i64 = r.get(1)?;
    let stage: String = r.get(3)?;
    Ok(Card {
        id: TodoId(r.get(0)?),
        number: number as u64,
        label: card_label(number as u64, None),
        title: r.get(2)?,
        stage: Stage::parse(&stage).unwrap_or(Stage::Backlog),
        project: r.get(4)?,
        proposed: r.get::<_, i64>(5)? != 0,
        archived: r.get::<_, Option<i64>>(6)?.is_some(),
        created_by: json_column(r, 7)?,
        checkout: optional_json(r, 8)?,
        blocked_by: Vec::new(),
        cleared_blockers: Vec::new(),
        created_at: r.get(9)?,
        updated_at: r
            .get::<_, i64>(9)?
            .max(r.get::<_, Option<i64>>(10)?.unwrap_or(0)),
    })
}

fn with_blockers(tx: &Transaction, mut card: Card) -> Result<Card> {
    let blockers = tx
        .prepare(
            "SELECT t.number, t.stage = 'done' OR t.archived_at IS NOT NULL
             FROM card_blocks b JOIN todos t ON t.id = b.by_id
             WHERE b.card_id = ?1 ORDER BY t.number",
        )?
        .query_map(params![card.id.as_str()], |r| {
            Ok((
                card_label(r.get::<_, i64>(0)? as u64, None),
                r.get::<_, bool>(1)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (label, cleared) in blockers {
        if cleared {
            card.cleared_blockers.push(label.clone());
        }
        card.blocked_by.push(label);
    }
    Ok(card)
}

fn card_by_id(tx: &Transaction, id: &TodoId) -> Result<Card> {
    let card = tx.query_row(
        &format!("SELECT {CARD_COLUMNS} FROM todos WHERE id = ?1"),
        params![id.as_str()],
        card_from_row,
    )?;
    with_blockers(tx, card)
}

pub(crate) fn in_scope(scope: &BoardScope, project: Option<&str>) -> bool {
    match (project, &scope.projects) {
        (None, _) => scope.include_home,
        (Some(_), ProjectScope::All) => true,
        (Some(name), ProjectScope::Named(names)) => names.iter().any(|n| n == name),
    }
}

/// Cards in `scope` from `offset` on, in number order, without blockers:
/// `with_blockers` hydrates only the ones a page returns.
fn scoped_cards(
    tx: &Transaction,
    scope: &BoardScope,
    stage: Option<Stage>,
    include_archived: bool,
    offset: usize,
) -> Result<Vec<Card>> {
    let named = match &scope.projects {
        ProjectScope::All => None,
        ProjectScope::Named(names) => Some(serde_json::to_string(names)?),
    };
    Ok(tx
        .prepare(&format!(
            "SELECT {CARD_COLUMNS} FROM todos WHERE stage IS NOT NULL
               AND (CASE WHEN project IS NULL THEN ?1
                    ELSE ?2 IS NULL OR project IN (SELECT value FROM json_each(?2)) END)
               AND (?3 IS NULL OR stage = ?3)
               AND (?4 OR archived_at IS NULL)
             ORDER BY number LIMIT -1 OFFSET ?5"
        ))?
        .query_map(
            params![
                scope.include_home,
                named,
                stage.map(|s| s.as_str()),
                include_archived,
                offset as i64
            ],
            card_from_row,
        )?
        .collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn cursor(tx: &Transaction) -> Result<String> {
    Ok(tx.query_row(
        "SELECT board_id || '.' || version FROM board_meta",
        [],
        |r| r.get(0),
    )?)
}

/// A continuation is the version it was cut at and the offset it resumes
/// from, so a page read after any write is refused instead of skewed.
pub(crate) fn page_offset(page: Option<&str>, version: &str) -> Result<usize> {
    let Some(page) = page else { return Ok(0) };
    match page.rsplit_once('@') {
        Some((at, offset)) if at == version => offset
            .parse()
            .map_err(|_| anyhow::anyhow!("malformed page token {page:?}")),
        Some(_) => bail!(Refusal::Conflict
            .message("the board changed since that page was cut; read it again from the start")),
        None => bail!("malformed page token {page:?}"),
    }
}

/// Fills one page from `items`, which start at `offset`, and cuts a
/// continuation when an item does not fit.
pub(crate) fn paged<T: serde::Serialize>(
    items: impl IntoIterator<Item = Result<T>>,
    offset: usize,
    version: &str,
    budget: usize,
) -> Result<(Vec<T>, Option<String>)> {
    let mut used = 0;
    let mut page = Vec::new();
    for item in items {
        let item = item?;
        let size = serde_json::to_vec(&item)?.len();
        if !page.is_empty() && used + size > budget {
            let end = offset + page.len();
            return Ok((page, Some(format!("{version}@{end}"))));
        }
        used += size;
        page.push(item);
    }
    Ok((page, None))
}

fn read_op(tx: &Transaction, links: &HostLinks, read: BoardRead) -> Result<BoardResult> {
    match read {
        BoardRead::Board {
            scope,
            since,
            away_since,
            page,
        } => {
            let version = cursor(tx)?;
            if page.is_none() && since.as_deref() == Some(version.as_str()) {
                return Ok(BoardResult::Board(BoardPoll::Unchanged { version }));
            }
            let offset = page_offset(page.as_deref(), &version)?;
            let cards = scoped_cards(tx, &scope, None, false, offset)?;
            let (hosts, needs_you, away) = if offset == 0 {
                let away = match away_since {
                    Some(since) => away(tx, &scope, &cards, since)?,
                    None => Away::default(),
                };
                (hosts(tx, links, &scope)?, waiting(tx, &cards, None)?, away)
            } else {
                (Vec::new(), Vec::new(), Away::default())
            };
            let reserved = serde_json::to_vec(&hosts)?.len()
                + serde_json::to_vec(&needs_you)?.len()
                + serde_json::to_vec(&away)?.len();
            let budget = page_budget().saturating_sub(reserved);
            let cards = cards.into_iter().map(|c| with_blockers(tx, c));
            let (cards, next) = paged(cards, offset, &version, budget)?;
            Ok(BoardResult::Board(BoardPoll::Board {
                version,
                board: BoardPage {
                    cards,
                    hosts,
                    needs_you,
                    away,
                },
                next,
            }))
        }
        BoardRead::Cards {
            scope,
            stage,
            include_archived,
            page,
        } => {
            let version = cursor(tx)?;
            let offset = page_offset(page.as_deref(), &version)?;
            let cards = scoped_cards(tx, &scope, stage, include_archived, offset)?;
            let cards = cards.into_iter().map(|c| with_blockers(tx, c));
            let (items, next) = paged(cards, offset, &version, page_budget())?;
            Ok(BoardResult::Cards(Page { items, next }))
        }
        BoardRead::Card { scope, card } => {
            let id = resolve(tx, &card)?;
            let found = card_by_id(tx, &id)?;
            if !in_scope(&scope, found.project.as_deref()) {
                return Err(not_found(&card));
            }
            let sessions = sessions_of(tx, links, &id)?;
            let artifacts = crate::artifacts::of_card(tx, &id)?;
            let reserved = serde_json::to_vec(&found)?.len()
                + serde_json::to_vec(&sessions)?.len()
                + serde_json::to_vec(&artifacts)?.len();
            Ok(BoardResult::Show(Box::new(CardShow {
                log: newest_notes(tx, &id, page_budget().saturating_sub(reserved))?,
                sessions,
                artifacts,
                card: found,
            })))
        }
        BoardRead::Artifacts {
            scope,
            card,
            kind,
            query,
            page,
        } => crate::artifacts::list(
            tx,
            &scope,
            card.as_ref(),
            kind.as_deref(),
            query.as_deref(),
            page.as_deref(),
        )
        .map(BoardResult::Artifacts),
        BoardRead::Artifact { scope, id } => {
            crate::artifacts::show(tx, &scope, &id).map(BoardResult::Artifact)
        }
        BoardRead::Dispatch { id } => receipt(tx, &id)?.map(BoardResult::Run).ok_or_else(|| {
            anyhow::anyhow!(Refusal::NotFound.message(format!("no dispatch {}", id.as_str())))
        }),
    }
}

/// The card's log in order, cut at the oldest note so it fits in `budget`
/// bytes and the reply stays under the frame limit.
fn newest_notes(tx: &Transaction, card: &TodoId, budget: usize) -> Result<Vec<CardNote>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {NOTE_COLUMNS} FROM notes WHERE todo_id = ?1 AND author IS NOT NULL
         ORDER BY created_at DESC, sort_order DESC"
    ))?;
    let rows = stmt.query_map(params![card.as_str()], note_from_row)?;
    let mut used = 0;
    let mut log = Vec::new();
    for note in rows {
        let note = note?;
        used += serde_json::to_vec(&note)?.len();
        if used > budget {
            break;
        }
        log.push(note);
    }
    log.reverse();
    Ok(log)
}

fn sessions_of(tx: &Transaction, links: &HostLinks, card: &TodoId) -> Result<Vec<CardSession>> {
    let stale = stale_hosts(tx, links)?;
    Ok(tx
        .prepare(
            "SELECT host, agent_id, name, role, kind, status, status_changed_at, writer, alive, parent
             FROM board_sessions WHERE card_id = ?1 ORDER BY created_at, rowid",
        )?
        .query_map(params![card.as_str()], |r| {
            let host: String = r.get(0)?;
            Ok(CardSession {
                stale: stale.contains(&host),
                parent: optional_json(r, 9)?,
                host,
                id: AgentId(r.get(1)?),
                name: r.get(2)?,
                role: r
                    .get::<_, Option<String>>(3)?
                    .and_then(|role| nebula_core::board::Role::parse(&role)),
                kind: AgentKind::parse(&r.get::<_, String>(4)?).unwrap_or_default(),
                status: AgentStatus::parse(&r.get::<_, String>(5)?).unwrap_or(AgentStatus::Fresh),
                status_changed_at: r.get(6)?,
                writer: r.get::<_, i64>(7)? != 0,
                alive: r.get::<_, i64>(8)? != 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn hosts(tx: &Transaction, links: &HostLinks, scope: &BoardScope) -> Result<Vec<BoardHost>> {
    let stale = stale_hosts(tx, links)?;
    let waiting: Vec<(String, Option<String>)> = tx
        .prepare(
            "SELECT s.host, t.project FROM board_sessions s JOIN todos t ON t.id = s.card_id
             WHERE t.stage IS NOT NULL AND s.status = 'needs_feedback' AND s.archived = 0 AND s.alive = 1",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let needs_you = |host: &str| {
        waiting
            .iter()
            .filter(|(h, project)| h == host && in_scope(scope, project.as_deref()))
            .count() as u32
    };
    Ok(tx
        .prepare(
            "SELECT h.host, h.writers, h.writer_slots, h.checkouts
             FROM board_hosts h ORDER BY h.host",
        )?
        .query_map([], |r| {
            let host: String = r.get(0)?;
            let checkouts: Vec<nebula_core::board::HostCheckout> = json_column(r, 3)?;
            Ok(BoardHost {
                needs_you: needs_you(&host),
                stale: stale.contains(&host),
                host,
                writers: r.get(1)?,
                writer_slots: r.get(2)?,
                checkouts: checkouts
                    .into_iter()
                    .filter(|c| in_scope(scope, Some(&c.project)))
                    .collect(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn waiting(tx: &Transaction, cards: &[Card], since: Option<i64>) -> Result<Vec<NeedsYou>> {
    let scoped: std::collections::HashSet<&str> = cards.iter().map(|c| c.id.as_str()).collect();
    Ok(tx
        .prepare(
            "SELECT host, agent_id, name, card_id, status_changed_at FROM board_sessions
             WHERE status = 'needs_feedback' AND archived = 0 AND alive = 1
               AND status_changed_at > ?1
             ORDER BY status_changed_at",
        )?
        .query_map(params![since.unwrap_or(i64::MIN)], |r| {
            Ok(NeedsYou {
                host: r.get(0)?,
                id: AgentId(r.get(1)?),
                name: r.get(2)?,
                card: TodoId(r.get(3)?),
                status_changed_at: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|waiting| scoped.contains(waiting.card.as_str()))
        .collect())
}

fn away(tx: &Transaction, scope: &BoardScope, cards: &[Card], since: i64) -> Result<Away> {
    let changed_after = |id: &TodoId| -> Result<bool> {
        Ok(tx.query_row(
            "SELECT COALESCE(stage_changed_at, 0) > ?2 FROM todos WHERE id = ?1",
            params![id.as_str(), since],
            |r| r.get(0),
        )?)
    };
    let mut finished = Vec::new();
    for card in cards {
        if matches!(card.stage, Stage::Review | Stage::Done) && changed_after(&card.id)? {
            finished.push(card.id.clone());
        }
    }
    let needs_you = waiting(tx, cards, Some(since))?;
    let decisions = tx
        .prepare(
            "SELECT id, project FROM artifacts
             WHERE kind = ?2 AND removed_at IS NULL AND created_at > ?1
             ORDER BY created_at",
        )?
        .query_map(params![since, nebula_core::board::DECISION_KIND], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, project)| in_scope(scope, project.as_deref()))
        .map(|(id, _)| ArtifactId(id))
        .collect();
    Ok(Away {
        finished,
        needs_you,
        decisions,
    })
}

fn stale_after() -> Duration {
    std::env::var("NEBULA_BOARD_STALE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(300))
}

struct Links {
    boot: Instant,
    open: HashMap<String, usize>,
    closed_at: HashMap<String, Instant>,
}

impl Links {
    fn down(&self, host: &str, after: Duration) -> bool {
        self.open.get(host).copied().unwrap_or(0) == 0
            && self.closed_at.get(host).copied().unwrap_or(self.boot).elapsed() >= after
    }
}

/// On the board host: the report connections each host holds open, so a
/// host whose last one closed reads stale once `stale_after` passes.
#[derive(Clone)]
pub(crate) struct HostLinks(Arc<Mutex<Links>>);

impl Default for HostLinks {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Links {
            boot: Instant::now(),
            open: HashMap::new(),
            closed_at: HashMap::new(),
        })))
    }
}

impl HostLinks {
    /// Once `stale_after` passes from boot, a host that never linked reads
    /// stale: bump the version then, only when there is such a host.
    pub(crate) fn boot(&self, store: Arc<Store>) {
        let links = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(stale_after()).await;
            let stale = {
                let store = store.clone();
                tokio::task::spawn_blocking(move || {
                    store.board_transaction(|tx| stale_hosts(tx, &links))
                })
                .await
            };
            if matches!(stale, Ok(Ok(hosts)) if hosts.is_empty()) {
                return;
            }
            bump(store).await;
        });
    }

    /// True when `host` read stale until this link opened: the caller bumps
    /// the board version so a poll sees it fresh again.
    pub(crate) fn opened(&self, host: &str) -> bool {
        let mut links = self.0.lock().unwrap();
        let was_stale = links.down(host, stale_after());
        *links.open.entry(host.to_string()).or_default() += 1;
        links.closed_at.remove(host);
        was_stale
    }

    pub(crate) fn closed(&self, host: &str, store: Arc<Store>) {
        let closed_at = Instant::now();
        {
            let mut links = self.0.lock().unwrap();
            let open = links.open.entry(host.to_string()).or_default();
            *open = open.saturating_sub(1);
            if *open > 0 {
                return;
            }
            links.closed_at.insert(host.to_string(), closed_at);
        }
        let host = host.to_string();
        let links = self.0.clone();
        tokio::spawn(async move {
            tokio::time::sleep(stale_after()).await;
            if links.lock().unwrap().closed_at.get(&host) == Some(&closed_at) {
                bump(store).await;
            }
        });
    }
}

pub(crate) fn bump_now(store: &Store) -> Result<()> {
    store.board_transaction(|tx| {
        tx.execute("UPDATE board_meta SET version = version + 1", [])?;
        Ok(())
    })
}

async fn bump(store: Arc<Store>) {
    let bumped = tokio::task::spawn_blocking(move || bump_now(&store)).await;
    if let Ok(Err(e)) = bumped {
        tracing::warn!(error = %e, "board version bump failed");
    }
}

fn stale_hosts(tx: &Transaction, links: &HostLinks) -> Result<HashSet<String>> {
    let known: Vec<String> = tx
        .prepare("SELECT host FROM board_hosts UNION SELECT host FROM board_sessions")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let links = links.0.lock().unwrap();
    let here = nebula_core::host::hostname();
    let after = stale_after();
    Ok(known
        .into_iter()
        .filter(|h| *h != here && links.down(h, after))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nebula_core::board::BoardScope;

    #[test]
    fn scoped_admits_only_the_writes_its_guard_checks() {
        let store = Store::open_in_memory().unwrap();
        let scope = BoardScope {
            projects: ProjectScope::Named(vec!["web".into()]),
            include_home: false,
        };
        let read = BoardOp::Read(BoardRead::Cards {
            scope: BoardScope {
                projects: ProjectScope::All,
                include_home: true,
            },
            stage: None,
            include_archived: false,
            page: None,
        });
        let nested = BoardOp::Scoped {
            scope: scope.clone(),
            op: Box::new(read.clone()),
        };
        for op in [read, nested] {
            let scoped = BoardOp::Scoped {
                scope: scope.clone(),
                op: Box::new(op),
            };
            assert!(serve(&store, &HostLinks::default(), scoped).is_err());
        }
    }

    fn author() -> Author {
        Author {
            host: "here".into(),
            agent: None,
            name: "me".into(),
        }
    }

    fn add_card(store: &Store, links: &HostLinks) -> CardRef {
        let id = TodoId::generate();
        serve(
            store,
            links,
            BoardOp::AddCard(CardDraft {
                id: id.clone(),
                title: "Card".into(),
                project: Some("web".into()),
                stage: Stage::Backlog,
                proposed: false,
                created_by: author(),
                checkout: None,
            }),
        )
        .unwrap();
        CardRef::Id(id)
    }

    #[test]
    fn end_run_with_both_a_session_and_an_error_is_refused() {
        let store = Store::open_in_memory().unwrap();
        let links = HostLinks::default();
        let CardRef::Id(card) = add_card(&store, &links) else {
            unreachable!()
        };
        let id = DispatchId::generate();
        store
            .board_transaction(|tx| {
                tx.execute(
                    "INSERT INTO dispatches (id, card_id, host, role, kind, worktree, state, created_at)
                     VALUES (?1, ?2, 'here', 'worker', 'claude', 'new', 'pending', 0)",
                    params![id.as_str(), card.as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        let ended = serve(
            &store,
            &links,
            BoardOp::EndRun {
                dispatch: id.clone(),
                agent: Some(AgentId::generate()),
                error: Some("spawn failed".into()),
            },
        );
        assert!(ended.is_err(), "{ended:?}");
        let Ok(BoardResult::Run(receipt)) =
            serve(&store, &links, BoardOp::Read(BoardRead::Dispatch { id }))
        else {
            panic!("no receipt")
        };
        assert_eq!(receipt.state, DispatchState::Pending);
    }

    #[test]
    fn show_keeps_the_newest_notes_when_the_log_outgrows_a_frame() {
        let store = Store::open_in_memory().unwrap();
        let links = HostLinks::default();
        let author = author();
        let card = add_card(&store, &links);
        let notes = 2 * MAX_FRAME_LEN as usize / 100_000;
        for i in 0..notes {
            serve(
                &store,
                &links,
                BoardOp::AddNote {
                    id: NoteId::generate(),
                    card: card.clone(),
                    text: format!("{i} {}", "x".repeat(100_000)),
                    author: author.clone(),
                },
            )
            .unwrap();
        }
        let scope = BoardScope {
            projects: ProjectScope::All,
            include_home: true,
        };
        let BoardResult::Show(show) = serve(
            &store,
            &links,
            BoardOp::Read(BoardRead::Card { scope, card }),
        )
            .unwrap()
        else {
            panic!("not a show")
        };
        let frame = rmp_serde::to_vec(&BoardResult::Show(show.clone())).unwrap();
        assert!(frame.len() < MAX_FRAME_LEN as usize, "{} bytes", frame.len());
        let last = show.log.last().unwrap();
        assert!(last.text.starts_with(&format!("{} ", notes - 1)));
        assert!(show.log.len() < notes);
    }
}
