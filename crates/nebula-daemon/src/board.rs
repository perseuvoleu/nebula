//! The board on its host (`board = "local"`): every board op runs in one
//! SQLite transaction, so a read sees one version and a write bumps it once.

use crate::store::Store;
use anyhow::{bail, Result};
use nebula_core::board::{
    card_label, Artifact, ArtifactContent, Author, Away, BoardHost, BoardOp, BoardPage, BoardPoll,
    BoardRead, BoardResult, BoardScope, Card, CardDraft, CardNote, CardRef, CardSession, CardShow,
    Page, Placement, ProjectScope, Refusal, SessionAt, Stage,
};
use nebula_core::{AgentId, AgentKind, AgentStatus, ArtifactId, NoteId, TodoId, MAX_FRAME_LEN};
use rusqlite::{params, OptionalExtension, Row, Transaction};

/// Bytes of cards one page may carry; the rest of the frame is headroom
/// for hosts, the away digest and the envelope.
fn page_budget() -> usize {
    std::env::var("NEBULA_BOARD_PAGE_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_FRAME_LEN as usize / 2)
}

pub fn serve(store: &Store, op: BoardOp) -> Result<BoardResult> {
    store.board_transaction(|tx| apply(tx, op))
}

fn apply(tx: &Transaction, op: BoardOp) -> Result<BoardResult> {
    match op {
        BoardOp::Read(read) => read_op(tx, read),
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
                    "UPDATE todos SET title = ?2, text = ?2 WHERE id = ?1",
                    params![id.as_str(), title],
                )?;
            }
            if let Some(placement) = project {
                let project = match placement {
                    Placement::Home => None,
                    Placement::Project(name) => Some(name),
                };
                tx.execute(
                    "UPDATE todos SET project = ?2 WHERE id = ?1",
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
        } => add_note(tx, id, &card, &text, &author).map(BoardResult::Note),
        BoardOp::Publish(_)
        | BoardOp::RemoveArtifact { .. }
        | BoardOp::BeginRun(_)
        | BoardOp::EndRun { .. }
        | BoardOp::Report(_) => bail!("this board does not serve that operation yet"),
    }
}

fn now_ms() -> i64 {
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
        CardRef::Number { number, .. } => format!("#{number}"),
        CardRef::Id(id) => id.to_string(),
    };
    anyhow::anyhow!(Refusal::NotFound.message(format!("no card {named}")))
}

fn resolve(tx: &Transaction, card: &CardRef) -> Result<TodoId> {
    let id: Option<String> = match card {
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
        "INSERT INTO todos (id, text, sort_order, created_at, number, title, stage, project, checkout, created_by, proposed, stage_changed_at)
         VALUES (?1, ?2, ?3, ?4, ?3, ?2, ?5, ?6, ?7, ?8, ?9, ?4)",
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

fn add_note(
    tx: &Transaction,
    id: NoteId,
    card: &CardRef,
    text: &str,
    author: &Author,
) -> Result<CardNote> {
    let card = resolve(tx, card)?;
    let known = note_by_id(tx, &id)?;
    if let Some(note) = known {
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

fn json_column<T: serde::de::DeserializeOwned>(r: &Row, i: usize) -> rusqlite::Result<T> {
    let text: String = r.get(i)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into())
    })
}

fn optional_json<T: serde::de::DeserializeOwned>(r: &Row, i: usize) -> rusqlite::Result<Option<T>> {
    match r.get::<_, Option<String>>(i)? {
        None => Ok(None),
        Some(_) => json_column(r, i).map(Some),
    }
}

const CARD_COLUMNS: &str =
    "id, number, title, stage, project, proposed, archived_at, created_by, checkout";

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
    })
}

fn with_blockers(tx: &Transaction, mut card: Card) -> Result<Card> {
    card.blocked_by = tx
        .prepare(
            "SELECT t.number FROM card_blocks b JOIN todos t ON t.id = b.by_id
             WHERE b.card_id = ?1 ORDER BY t.number",
        )?
        .query_map(params![card.id.as_str()], |r| r.get::<_, i64>(0))?
        .map(|n| n.map(|n| card_label(n as u64, None)))
        .collect::<rusqlite::Result<_>>()?;
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

fn in_scope(scope: &BoardScope, project: Option<&str>) -> bool {
    match (project, &scope.projects) {
        (None, _) => scope.include_home,
        (Some(_), ProjectScope::All) => true,
        (Some(name), ProjectScope::Named(names)) => names.iter().any(|n| n == name),
    }
}

/// Every card in scope, by number.
fn scoped_cards(
    tx: &Transaction,
    scope: &BoardScope,
    stage: Option<Stage>,
    include_archived: bool,
) -> Result<Vec<Card>> {
    let cards = tx
        .prepare(&format!(
            "SELECT {CARD_COLUMNS} FROM todos WHERE stage IS NOT NULL ORDER BY number"
        ))?
        .query_map([], card_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    cards
        .into_iter()
        .filter(|c| in_scope(scope, c.project.as_deref()))
        .filter(|c| stage.is_none_or(|s| s == c.stage))
        .filter(|c| include_archived || !c.archived)
        .map(|c| with_blockers(tx, c))
        .collect()
}

fn cursor(tx: &Transaction) -> Result<String> {
    Ok(tx.query_row(
        "SELECT board_id || '.' || version FROM board_meta",
        [],
        |r| r.get(0),
    )?)
}

/// A continuation is the version it was cut at and the offset it resumes
/// from, so a page read after any write is refused instead of skewed.
fn page_offset(page: Option<&str>, version: &str) -> Result<usize> {
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

/// The cards from `offset` that fit the page budget (at least one), and
/// the continuation when some are left.
fn paged<T: serde::Serialize>(
    items: Vec<T>,
    offset: usize,
    version: &str,
) -> Result<(Vec<T>, Option<String>)> {
    let budget = page_budget();
    let total = items.len();
    let mut used = 0;
    let mut page = Vec::new();
    for item in items.into_iter().skip(offset) {
        let size = serde_json::to_vec(&item)?.len();
        if !page.is_empty() && used + size > budget {
            break;
        }
        used += size;
        page.push(item);
    }
    let end = offset + page.len();
    Ok((page, (end < total).then(|| format!("{version}@{end}"))))
}

fn read_op(tx: &Transaction, read: BoardRead) -> Result<BoardResult> {
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
            let cards = scoped_cards(tx, &scope, None, false)?;
            let (hosts, away) = if offset == 0 {
                let away = match away_since {
                    Some(since) => away(tx, &scope, &cards, since)?,
                    None => Away::default(),
                };
                (hosts(tx)?, away)
            } else {
                (Vec::new(), Away::default())
            };
            let (cards, next) = paged(cards, offset, &version)?;
            Ok(BoardResult::Board(BoardPoll::Board {
                version,
                board: BoardPage { cards, hosts, away },
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
            let cards = scoped_cards(tx, &scope, stage, include_archived)?;
            let (items, next) = paged(cards, offset, &version)?;
            Ok(BoardResult::Cards(Page { items, next }))
        }
        BoardRead::Card { scope, card } => {
            let id = resolve(tx, &card)?;
            let found = card_by_id(tx, &id)?;
            if !in_scope(&scope, found.project.as_deref()) {
                return Err(not_found(&card));
            }
            Ok(BoardResult::Show(Box::new(CardShow {
                sessions: sessions_of(tx, &id)?,
                artifacts: artifacts_of(tx, &id)?,
                log: tx
                    .prepare(&format!(
                        "SELECT {NOTE_COLUMNS} FROM notes WHERE todo_id = ?1 AND author IS NOT NULL ORDER BY created_at, sort_order"
                    ))?
                    .query_map(params![id.as_str()], note_from_row)?
                    .collect::<rusqlite::Result<_>>()?,
                card: found,
            })))
        }
        BoardRead::Artifacts { .. } | BoardRead::Artifact { .. } => {
            bail!("this board does not serve artifact reads yet")
        }
    }
}

fn sessions_of(tx: &Transaction, card: &TodoId) -> Result<Vec<CardSession>> {
    Ok(tx
        .prepare(
            "SELECT host, agent_id, name, role, kind, status, status_changed_at, writer, alive
             FROM board_sessions WHERE card_id = ?1 ORDER BY rowid",
        )?
        .query_map(params![card.as_str()], |r| {
            Ok(CardSession {
                host: r.get(0)?,
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
                stale: false,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn artifacts_of(tx: &Transaction, card: &TodoId) -> Result<Vec<Artifact>> {
    Ok(tx
        .prepare(
            "SELECT id, kind, title, storage_host, entry, url, body, card_id, project, session, checkout, meta, created_at
             FROM artifacts WHERE card_id = ?1 AND removed_at IS NULL ORDER BY created_at",
        )?
        .query_map(params![card.as_str()], |r| {
            let content = match (
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ) {
                (Some(host), Some(entry), _, _) => ArtifactContent::Files { host, entry },
                (_, _, Some(url), _) => ArtifactContent::Url { url },
                (_, _, _, body) => ArtifactContent::Body {
                    body: body.unwrap_or_default(),
                },
            };
            Ok(Artifact {
                id: ArtifactId(r.get(0)?),
                kind: r.get(1)?,
                title: r.get(2)?,
                card: r.get::<_, Option<String>>(7)?.map(TodoId),
                project: r.get(8)?,
                session: optional_json(r, 9)?,
                checkout: optional_json(r, 10)?,
                content,
                meta: json_column(r, 11)?,
                created_at: r.get(12)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn hosts(tx: &Transaction) -> Result<Vec<BoardHost>> {
    Ok(tx
        .prepare(
            "SELECT h.host, h.writers, h.writer_slots, h.checkouts,
                    (SELECT COUNT(*) FROM board_sessions s
                     WHERE s.host = h.host AND s.status = 'needs_feedback' AND s.archived = 0)
             FROM board_hosts h ORDER BY h.host",
        )?
        .query_map([], |r| {
            Ok(BoardHost {
                host: r.get(0)?,
                writers: r.get(1)?,
                writer_slots: r.get(2)?,
                checkouts: json_column(r, 3)?,
                needs_you: r.get(4)?,
                stale: false,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// What happened on the scoped board after `since`: cards that reached
/// review or done, sessions waiting on the user, decisions taken.
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
    let scoped: std::collections::HashSet<&str> = cards.iter().map(|c| c.id.as_str()).collect();
    let needs_you = tx
        .prepare(
            "SELECT host, agent_id, card_id FROM board_sessions
             WHERE status = 'needs_feedback' AND archived = 0 AND status_changed_at > ?1
             ORDER BY status_changed_at",
        )?
        .query_map(params![since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, _, card)| scoped.contains(card.as_str()))
        .map(|(host, id, _)| SessionAt {
            host,
            id: AgentId(id),
        })
        .collect();
    let decisions = tx
        .prepare(
            "SELECT id, project FROM artifacts
             WHERE kind = 'decision' AND removed_at IS NULL AND created_at > ?1
             ORDER BY created_at",
        )?
        .query_map(params![since], |r| {
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
