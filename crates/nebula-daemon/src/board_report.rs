//! Host reports: each host tells the board which of its sessions work on a
//! card, how many writers it runs and which checkouts it holds. The host
//! side sends a full report on start and, debounced, whenever that content
//! changes; the board side applies one in a single transaction and moves
//! cards with `auto_stage`.

use crate::registry::Daemon;
use crate::store::Store;
use anyhow::Result;
use nebula_core::board::{
    auto_stage, card_label, Author, BoardOp, BoardResult, Checkout, HostCheckout, HostReport, Role,
    SessionAt, SessionRow, Stage,
};
use nebula_core::{
    AgentId, AgentKind, AgentStatus, DispatchId, NoteId, ServerEvent, SessionRef, TodoId,
};
use rusqlite::{params, OptionalExtension, Row, Transaction};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Notify};

const DEBOUNCE: Duration = Duration::from_secs(1);

static RESEND: Notify = Notify::const_new();

/// Sends the next report in full even when nothing changed, as a board link
/// that has just connected needs.
pub fn resend() {
    RESEND.notify_one();
}

pub fn spawn(daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        let mut events = daemon.events.subscribe();
        let mut sent: Option<HostReport> = None;
        loop {
            if let Err(e) = send_if_changed(&daemon, &mut sent).await {
                tracing::warn!(error = %e, "board report failed");
            }
            tokio::select! {
                _ = daemon.shutdown.cancelled() => break,
                _ = RESEND.notified() => sent = None,
                changed = next_change(&mut events) => if !changed { break },
            }
            tokio::select! {
                _ = daemon.shutdown.cancelled() => break,
                _ = tokio::time::sleep(DEBOUNCE) => {}
            }
            while events.try_recv().is_ok() {}
        }
    });
}

async fn next_change(events: &mut broadcast::Receiver<ServerEvent>) -> bool {
    loop {
        match events.recv().await {
            Ok(
                ServerEvent::EntityUpserted { .. }
                | ServerEvent::EntityRemoved { .. }
                | ServerEvent::StatusChanged { .. }
                | ServerEvent::SessionExited { .. },
            )
            | Err(broadcast::error::RecvError::Lagged(_)) => return true,
            Ok(_) => continue,
            Err(broadcast::error::RecvError::Closed) => return false,
        }
    }
}

async fn send_if_changed(daemon: &Arc<Daemon>, sent: &mut Option<HostReport>) -> Result<()> {
    let config = crate::config::Config::load();
    if config.board.as_deref() != Some("local") {
        return Ok(());
    }
    let report = host_report(daemon, config.writer_slots)?;
    if sent.as_ref() == Some(&report) {
        return Ok(());
    }
    let store = daemon.store.clone();
    let op = BoardOp::Report(report.clone());
    tokio::task::spawn_blocking(move || crate::board::serve(&store, op)).await??;
    *sent = Some(report);
    Ok(())
}

fn host_report(daemon: &Daemon, writer_slots: Option<u32>) -> Result<HostReport> {
    let host = nebula_core::host::hostname();
    let (projects, worktrees, agents, _) = daemon.store.load_tree()?;
    let local: HashMap<_, _> = projects
        .iter()
        .filter(|p| p.host.is_none())
        .map(|p| (&p.id, p.name.as_str()))
        .collect();
    let checkouts: HashMap<_, _> = worktrees
        .iter()
        .filter_map(|w| {
            local
                .get(&w.project_id)
                .map(|project| (&w.id, (w, *project)))
        })
        .collect();
    let mut sessions: Vec<SessionRow> = agents
        .iter()
        .filter_map(|a| {
            let card_id = a.card_id.clone()?;
            let (worktree, _) = checkouts.get(&a.worktree_id)?;
            Some(SessionRow {
                agent_id: a.id.clone(),
                host: host.clone(),
                card_id,
                role: a.role,
                kind: a.kind,
                name: a.name.clone(),
                parent: a.spawned_by.clone().map(|id| SessionAt {
                    host: host.clone(),
                    id,
                }),
                status: a.status,
                status_changed_at: a.status_changed_at,
                writer: a.writer,
                alive: daemon.is_alive(&SessionRef::Agent(a.id.clone())),
                archived: a.archived,
                branch: Some(worktree.branch.clone()),
                dispatch_id: a.dispatch_id.clone(),
            })
        })
        .collect();
    sessions.sort_by(|a, b| a.agent_id.as_str().cmp(b.agent_id.as_str()));
    Ok(HostReport {
        writers: agents.iter().filter(|a| daemon.holds_writer(a)).count() as u32,
        writer_slots,
        checkouts: worktrees
            .iter()
            .filter_map(|w| {
                local.get(&w.project_id).map(|project| HostCheckout {
                    project: project.to_string(),
                    branch: w.branch.clone(),
                    path: w.path.display().to_string(),
                    worktree: !w.is_main,
                })
            })
            .collect(),
        host,
        sessions,
    })
}

/// The board side of a report. Only content that changed is written, so a
/// repeated report leaves the board version alone; rows are never deleted,
/// and a session the host no longer reports is marked not alive.
pub fn apply(tx: &Transaction, report: HostReport) -> Result<BoardResult> {
    let now = crate::board::now_ms();
    let checkouts = serde_json::to_string(&report.checkouts)?;
    let same_host: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM board_hosts WHERE host = ?1 AND writers = ?2
                        AND writer_slots IS ?3 AND checkouts = ?4)",
        params![report.host, report.writers, report.writer_slots, checkouts],
        |r| r.get(0),
    )?;
    if !same_host {
        tx.execute(
            "INSERT INTO board_hosts (host, writers, writer_slots, checkouts, reported_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (host) DO UPDATE SET writers = excluded.writers,
               writer_slots = excluded.writer_slots, checkouts = excluded.checkouts,
               reported_at = excluded.reported_at",
            params![
                report.host,
                report.writers,
                report.writer_slots,
                checkouts,
                now
            ],
        )?;
    }
    for row in &report.sessions {
        apply_row(tx, &report, row, now)?;
    }
    let reported: HashSet<&str> = report
        .sessions
        .iter()
        .map(|s| s.agent_id.as_str())
        .collect();
    let gone: Vec<String> = tx
        .prepare("SELECT agent_id FROM board_sessions WHERE host = ?1 AND alive = 1")?
        .query_map(params![report.host], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?
        .into_iter()
        .filter(|id| !reported.contains(id.as_str()))
        .collect();
    for id in gone {
        tx.execute(
            "UPDATE board_sessions SET alive = 0, reported_at = ?2 WHERE agent_id = ?1",
            params![id, now],
        )?;
    }
    Ok(BoardResult::Done)
}

fn apply_row(tx: &Transaction, report: &HostReport, row: &SessionRow, now: i64) -> Result<()> {
    let before = session_row(tx, &row.agent_id)?;
    if before.as_ref() == Some(row)
        || before
            .as_ref()
            .is_some_and(|b| row.status_changed_at < b.status_changed_at)
    {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO board_sessions (agent_id, host, card_id, role, kind, name, parent, status,
           status_changed_at, writer, alive, archived, branch, dispatch_id, reported_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT (agent_id) DO UPDATE SET host = excluded.host, card_id = excluded.card_id,
           role = excluded.role, kind = excluded.kind, name = excluded.name,
           parent = excluded.parent, status = excluded.status,
           status_changed_at = excluded.status_changed_at, writer = excluded.writer,
           alive = excluded.alive, archived = excluded.archived, branch = excluded.branch,
           dispatch_id = excluded.dispatch_id, reported_at = excluded.reported_at",
        params![
            row.agent_id.as_str(),
            row.host,
            row.card_id.as_str(),
            row.role.map(|r| r.as_str()),
            row.kind.as_str(),
            row.name,
            row.parent.as_ref().map(serde_json::to_string).transpose()?,
            row.status.as_str(),
            row.status_changed_at,
            row.writer,
            row.alive,
            row.archived,
            row.branch,
            row.dispatch_id.as_ref().map(|d| d.0.as_str()),
            now,
        ],
    )?;
    let card: Option<(String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT stage, checkout, project FROM todos WHERE id = ?1 AND stage IS NOT NULL",
            params![row.card_id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((stage, checkout, project)) = card else {
        return Ok(());
    };
    if before.is_none() && row.role == Some(Role::Worker) && checkout.is_none() {
        let held = report.checkouts.iter().find(|c| {
            row.branch.as_deref() == Some(c.branch.as_str())
                && project.as_deref().is_none_or(|p| p == c.project)
        });
        if let Some(held) = held {
            let checkout = Checkout {
                host: report.host.clone(),
                branch: held.branch.clone(),
                path: held.path.clone(),
                worktree: held.worktree,
            };
            tx.execute(
                "UPDATE todos SET checkout = ?2 WHERE id = ?1",
                params![row.card_id.as_str(), serde_json::to_string(&checkout)?],
            )?;
        }
    }
    let others: Vec<SessionRow> = tx
        .prepare(&format!(
            "SELECT {ROW_COLUMNS} FROM board_sessions WHERE card_id = ?1 AND agent_id <> ?2"
        ))?
        .query_map(
            params![row.card_id.as_str(), row.agent_id.as_str()],
            session_from_row,
        )?
        .collect::<rusqlite::Result<_>>()?;
    let stage = Stage::parse(&stage).unwrap_or(Stage::Backlog);
    let Some(next) = auto_stage(stage, before.as_ref(), row, &others) else {
        return Ok(());
    };
    tx.execute(
        "UPDATE todos SET stage = ?2, stage_changed_at = ?3 WHERE id = ?1",
        params![row.card_id.as_str(), next.as_str(), now],
    )?;
    let did = if row.status == AgentStatus::Finished {
        "finished"
    } else {
        "started"
    };
    crate::board::add_note(
        tx,
        NoteId::generate(),
        &nebula_core::board::CardRef::Id(row.card_id.clone()),
        &format!("worker {} {did}: moved to {}", row.name, next.as_str()),
        &Author {
            host: row.host.clone(),
            agent: Some(row.agent_id.clone()),
            name: row.name.clone(),
        },
    )?;
    Ok(())
}

const ROW_COLUMNS: &str = "agent_id, host, card_id, role, kind, name, parent, status,
    status_changed_at, writer, alive, archived, branch, dispatch_id";

fn session_from_row(r: &Row) -> rusqlite::Result<SessionRow> {
    let parent: Option<String> = r.get(6)?;
    Ok(SessionRow {
        agent_id: AgentId(r.get(0)?),
        host: r.get(1)?,
        card_id: TodoId(r.get(2)?),
        role: r
            .get::<_, Option<String>>(3)?
            .and_then(|role| Role::parse(&role)),
        kind: AgentKind::parse(&r.get::<_, String>(4)?).unwrap_or_default(),
        name: r.get(5)?,
        parent: parent.and_then(|p| serde_json::from_str(&p).ok()),
        status: AgentStatus::parse(&r.get::<_, String>(7)?).unwrap_or(AgentStatus::Fresh),
        status_changed_at: r.get(8)?,
        writer: r.get(9)?,
        alive: r.get(10)?,
        archived: r.get(11)?,
        branch: r.get(12)?,
        dispatch_id: r.get::<_, Option<String>>(13)?.map(DispatchId),
    })
}

fn session_row(tx: &Transaction, agent: &AgentId) -> Result<Option<SessionRow>> {
    Ok(tx
        .query_row(
            &format!("SELECT {ROW_COLUMNS} FROM board_sessions WHERE agent_id = ?1"),
            params![agent.as_str()],
            session_from_row,
        )
        .optional()?)
}

/// The card a session works on, from its local agent row and, for the title,
/// the local board; it never reaches the network. A card this host's board
/// does not hold is named by its id.
pub fn card_pointer(store: &Store, agent: &AgentId) -> Result<Option<(String, Option<String>)>> {
    let Some(card) = store.get_agent(agent)?.and_then(|a| a.card_id) else {
        return Ok(None);
    };
    store.board_transaction(|tx| {
        let local: Option<(i64, String)> = tx
            .query_row(
                "SELECT number, title FROM todos WHERE id = ?1 AND stage IS NOT NULL",
                params![card.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(Some(match local {
            Some((number, title)) => (card_label(number as u64, None), Some(title)),
            None => (card.0.clone(), None),
        }))
    })
}
