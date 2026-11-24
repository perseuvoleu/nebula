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
use tokio_util::sync::CancellationToken;

const DEBOUNCE: Duration = Duration::from_secs(1);

static RESEND: Notify = Notify::const_new();

/// Sends the next report in full even when nothing changed, as a board link
/// that has just connected needs.
pub fn resend() {
    RESEND.notify_one();
}

pub fn spawn(daemon: Arc<Daemon>) {
    let events = daemon.events.subscribe();
    let shutdown = daemon.shutdown.clone();
    tokio::spawn(run(
        HostReporter { daemon, sent: None },
        events,
        shutdown,
        DEBOUNCE,
    ));
}

trait Reporter {
    async fn send(&mut self, full: bool) -> Result<()>;
}

struct HostReporter {
    daemon: Arc<Daemon>,
    sent: Option<HostReport>,
}

impl Reporter for HostReporter {
    async fn send(&mut self, full: bool) -> Result<()> {
        if full {
            self.sent = None;
        }
        send_if_changed(&self.daemon, &mut self.sent).await
    }
}

async fn run(
    mut reporter: impl Reporter,
    mut events: broadcast::Receiver<ServerEvent>,
    shutdown: CancellationToken,
    debounce: Duration,
) {
    let mut full = false;
    loop {
        match reporter.send(std::mem::take(&mut full)).await {
            Err(e) => tracing::warn!(error = %e, "board report failed; retrying"),
            Ok(()) => tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = RESEND.notified() => full = true,
                changed = next_change(&mut events) => if !changed { break },
            },
        }
        tokio::select! {
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(debounce) => {}
        }
        while events.try_recv().is_ok() {}
    }
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
    let origins = daemon.store.agent_origins()?;
    let local: HashMap<_, _> = projects
        .iter()
        .filter(|p| p.host.is_none())
        .map(|p| (&p.id, p.name.as_str()))
        .collect();
    let local_worktrees: HashMap<_, _> = worktrees
        .iter()
        .filter(|w| local.contains_key(&w.project_id))
        .map(|w| (&w.id, w))
        .collect();
    let mut sessions: Vec<SessionRow> = agents
        .iter()
        .filter_map(|a| {
            let card_id = a.card_id.clone()?;
            let worktree = local_worktrees.get(&a.worktree_id)?;
            Some(SessionRow {
                agent_id: a.id.clone(),
                host: host.clone(),
                card_id,
                role: a.role,
                kind: a.kind,
                name: a.name.clone(),
                parent: a.spawned_by.clone().map(|id| SessionAt {
                    host: origins
                        .get(&a.id)
                        .and_then(|(_, parent_host)| parent_host.clone())
                        .unwrap_or_else(|| host.clone()),
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
        writers: agents
            .iter()
            .filter(|a| daemon.holds_writer(a) && daemon.is_alive(&SessionRef::Agent(a.id.clone())))
            .count() as u32,
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
    Ok(Some(match store.board_card_title(&card)? {
        Some((number, title)) => (card_label(number as u64, None), Some(title)),
        None => (card.0.clone(), None),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookEnv;
    use crate::registry::DaemonOverrides;
    use nebula_core::{Agent, Project, ProjectId, Worktree, WorktreeId};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn daemon() -> Arc<Daemon> {
        let daemon = Daemon::with_overrides(
            Arc::new(Store::open_in_memory().unwrap()),
            HookEnv {
                port: 0,
                token: String::new(),
            },
            DaemonOverrides::default(),
        );
        daemon
            .store
            .insert_project(&Project {
                workspace_id: Default::default(),
                id: ProjectId("p1".into()),
                name: "p".into(),
                repo_path: "/tmp/p".into(),
                sort_order: 0,
                divider_after: false,
                divider_label: None,
                divider_before: false,
                divider_before_label: None,
                host: None,
            })
            .unwrap();
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("w1".into()),
                project_id: ProjectId("p1".into()),
                path: "/tmp/p".into(),
                branch: "main".into(),
                is_main: true,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();
        daemon
    }

    fn agent(id: &str) -> Agent {
        Agent {
            id: AgentId(id.into()),
            worktree_id: WorktreeId("w1".into()),
            name: id.into(),
            status: AgentStatus::Running,
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
            branch: Some("main".into()),
            spawned_by: None,
            writer: false,
            card_id: Some(TodoId("card".into())),
            role: Some(Role::Worker),
            dispatch_id: None,
            code_mode_off: false,
        }
    }

    #[test]
    fn report_counts_only_live_writers() {
        let daemon = daemon();
        daemon
            .store
            .insert_agent(&Agent {
                writer: true,
                status: AgentStatus::Finished,
                ..agent("exited")
            })
            .unwrap();
        let report = host_report(&daemon, None).unwrap();
        assert_eq!(report.sessions[0].alive, false);
        assert_eq!(report.writers, 0, "an exited writer is no live writer");
    }

    #[test]
    fn report_names_the_parents_host() {
        let daemon = daemon();
        let parent = |id: &str| Some(AgentId(id.into()));
        for (id, spawned_by) in [
            ("far-child", parent("far-parent")),
            ("near-child", parent("far-child")),
        ] {
            daemon
                .store
                .insert_agent(&Agent {
                    spawned_by,
                    ..agent(id)
                })
                .unwrap();
        }
        daemon
            .store
            .set_spawned_by_host(&AgentId("far-child".into()), "host-a")
            .unwrap();
        let report = host_report(&daemon, None).unwrap();
        let parents: Vec<(&str, Option<&SessionAt>)> = report
            .sessions
            .iter()
            .map(|s| (s.agent_id.as_str(), s.parent.as_ref()))
            .collect();
        let here = nebula_core::host::hostname();
        assert_eq!(
            parents,
            [
                (
                    "far-child",
                    Some(&SessionAt {
                        host: "host-a".into(),
                        id: AgentId("far-parent".into()),
                    })
                ),
                (
                    "near-child",
                    Some(&SessionAt {
                        host: here,
                        id: AgentId("far-child".into()),
                    })
                ),
            ]
        );
    }

    struct Flaky(Arc<AtomicUsize>);

    impl Reporter for Flaky {
        async fn send(&mut self, _full: bool) -> Result<()> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("database is locked");
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_failed_report_is_retried_after_the_debounce() {
        let (_events, rx) = broadcast::channel(16);
        let shutdown = CancellationToken::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let debounce = Duration::from_millis(20);
        let task = tokio::spawn(run(Flaky(attempts.clone()), rx, shutdown.clone(), debounce));
        tokio::time::sleep(debounce * 25).await;
        shutdown.cancel();
        task.await.unwrap();
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "no event came, yet the report retried once"
        );
    }
}
