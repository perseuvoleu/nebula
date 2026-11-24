//! `nebula todo` card verbs: resolve the caller once, send one Board op
//! to the local daemon, print its reply (JSON with --json).

use crate::ipc::{self, Connection};
use anyhow::{bail, Context, Result};
use nebula_core::board::{
    parse_card_ref, Author, BoardOp, BoardPoll, BoardRead, BoardResult, BoardScope, Card,
    CardDraft, CardRef, Checkout, Placement, ProjectScope, Refusal, Role, Stage,
};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{paths, Agent, ClientRequest, NoteId, Project, ServerEvent, TodoId, Worktree};

/// Who runs a board verb: the session NEBULA_AGENT_ID names, else the user
/// in the checkout holding the cwd.
pub struct Caller {
    pub author: Author,
    pub project: Option<String>,
    pub checkout: Option<Checkout>,
    pub card: Option<TodoId>,
    pub role: Option<Role>,
}

impl Caller {
    pub fn resolve(projects: &[Project], worktrees: &[Worktree], agents: &[Agent]) -> Result<Self> {
        let place = |worktree: &Worktree| {
            let project = projects.iter().find(|p| p.id == worktree.project_id);
            let host = project
                .and_then(|p| p.host.clone())
                .unwrap_or_else(nebula_core::host::hostname);
            let checkout = Checkout {
                host,
                branch: worktree.branch.clone(),
                path: worktree.path.display().to_string(),
                worktree: !worktree.is_main,
            };
            (project.map(|p| p.name.clone()), checkout)
        };
        if let Some(id) = ipc::env_agent_id() {
            let agent = agents
                .iter()
                .find(|a| a.id.as_str() == id)
                .with_context(|| {
                    format!("NEBULA_AGENT_ID {id} names no session this daemon knows")
                })?;
            let (project, checkout) = worktrees
                .iter()
                .find(|w| w.id == agent.worktree_id)
                .map(place)
                .unzip();
            return Ok(Self {
                author: Author {
                    host: checkout
                        .as_ref()
                        .map(|c| c.host.clone())
                        .unwrap_or_else(nebula_core::host::hostname),
                    agent: Some(agent.id.clone()),
                    name: agent.name.clone(),
                },
                project: project.flatten(),
                checkout,
                card: agent.card_id.clone(),
                role: agent.role,
            });
        }
        let (project, checkout) = std::env::current_dir()
            .ok()
            .and_then(|cwd| cwd_checkout(&cwd, projects, worktrees))
            .map(place)
            .unzip();
        Ok(Self {
            author: Author {
                host: nebula_core::host::hostname(),
                agent: None,
                name: std::env::var("USER").unwrap_or_else(|_| "user".into()),
            },
            project: project.flatten(),
            checkout,
            card: None,
            role: None,
        })
    }
}

fn cwd_checkout<'a>(
    cwd: &std::path::Path,
    projects: &[Project],
    worktrees: &'a [Worktree],
) -> Option<&'a Worktree> {
    let local = |w: &Worktree| {
        projects
            .iter()
            .any(|p| p.id == w.project_id && p.host.is_none())
    };
    worktrees
        .iter()
        .filter(|w| local(w) && cwd.starts_with(&w.path))
        .max_by_key(|w| w.path.components().count())
}

#[derive(Default)]
pub struct ScopeFlags {
    pub projects: Vec<String>,
    pub all: bool,
    pub include_home: bool,
}

impl ScopeFlags {
    fn guard(self, op: BoardOp) -> BoardOp {
        match self.all || self.include_home || !self.projects.is_empty() {
            true => BoardOp::Scoped {
                scope: self.scope(None),
                op: Box::new(op),
            },
            false => op,
        }
    }

    fn scope(self, fallback: Option<&str>) -> BoardScope {
        let projects = match (self.all, self.projects.is_empty(), fallback) {
            (true, _, _) => ProjectScope::All,
            (false, false, _) => ProjectScope::Named(self.projects),
            (false, true, Some(project)) => ProjectScope::Named(vec![project.to_string()]),
            (false, true, None) => {
                return BoardScope {
                    projects: ProjectScope::All,
                    include_home: true,
                }
            }
        };
        BoardScope {
            projects,
            include_home: self.include_home,
        }
    }
}

pub enum TodoVerb {
    Add {
        title: String,
        project: Option<String>,
        home: bool,
        stage: Stage,
        propose: bool,
        id: Option<String>,
    },
    Edit {
        card: String,
        title: Option<String>,
        project: Option<String>,
        home: bool,
    },
    Move {
        card: String,
        stage: Stage,
        expect: Option<Stage>,
        scope: ScopeFlags,
    },
    Archive {
        card: String,
        scope: ScopeFlags,
    },
    Accept {
        card: String,
        scope: ScopeFlags,
    },
    Dismiss {
        card: String,
        scope: ScopeFlags,
    },
    Note {
        card: String,
        text: String,
        id: Option<String>,
        scope: ScopeFlags,
    },
    Block {
        card: String,
        by: String,
        on: bool,
        scope: ScopeFlags,
    },
    List {
        scope: ScopeFlags,
        stage: Option<Stage>,
        archived: bool,
        page: Option<String>,
    },
    Show {
        card: String,
        scope: ScopeFlags,
    },
    Board {
        scope: ScopeFlags,
        since: Option<String>,
        away_since: Option<i64>,
        page: Option<String>,
    },
}

pub fn card_ref(text: &str) -> Result<CardRef> {
    parse_card_ref(text)
        .with_context(|| format!("{text:?} is not a card ref (#12, 12, 12@host or a ULID)"))
}

fn client_id(id: Option<String>, minted: &mut Option<String>) -> Result<String> {
    match id {
        None => Ok(minted.insert(TodoId::generate().0).clone()),
        Some(id) => match parse_card_ref(&id) {
            Some(CardRef::Id(canonical)) => Ok(canonical.0),
            _ => bail!("--id {id:?} is not a ULID"),
        },
    }
}

async fn connect() -> Result<Connection> {
    let Ok(stream) = ipc::try_connect(&paths::socket_path()).await else {
        bail!("no nebula daemon is running");
    };
    ipc::handshake(stream).await
}

pub async fn request(conn: &mut Connection, op: BoardOp) -> Result<BoardResult> {
    let req_id = 1;
    write_frame(&mut conn.stream, &ClientRequest::Board { req_id, op }).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::BoardReply { req_id: r, result }) if r == req_id => {
                return Ok(result)
            }
            Some(ServerEvent::Error {
                req_id: Some(r),
                message,
            }) if r == req_id => bail!("{message}"),
            Some(_) => continue,
            None => bail!("daemon closed the connection before replying"),
        }
    }
}

/// Reads the card `text` names, so a spawn linking it is refused before
/// anything starts.
pub async fn check_card(conn: &mut Connection, text: &str) -> Result<TodoId> {
    let op = BoardOp::Read(BoardRead::Card {
        scope: ScopeFlags::default().scope(None),
        card: card_ref(text)?,
    });
    match request(conn, op).await? {
        BoardResult::Show(show) => Ok(show.card.id),
        other => bail!("the board answered a card read with {other:?}"),
    }
}

pub async fn run(verb: TodoVerb, json: bool) -> Result<()> {
    let mut conn = connect().await?;
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(&mut conn).await?;
    let caller = Caller::resolve(&projects, &worktrees, &agents)?;
    let mut minted = None;
    let mut op = match verb {
        TodoVerb::Add {
            title,
            project,
            home,
            stage,
            propose,
            id,
        } => BoardOp::AddCard(CardDraft {
            id: TodoId(client_id(id, &mut minted)?),
            title,
            project: match (project, home) {
                (_, true) => None,
                (Some(project), false) => Some(project),
                (None, false) => caller.project.clone(),
            },
            stage,
            proposed: propose,
            created_by: caller.author.clone(),
            checkout: caller.checkout.clone(),
        }),
        TodoVerb::Edit {
            card,
            title,
            project,
            home,
        } => BoardOp::EditCard {
            card: card_ref(&card)?,
            title,
            project: match (project, home) {
                (_, true) => Some(Placement::Home),
                (Some(project), false) => Some(Placement::Project(project)),
                (None, false) => None,
            },
        },
        TodoVerb::Move {
            card,
            stage,
            expect,
            scope,
        } => scope.guard(BoardOp::Move {
            card: card_ref(&card)?,
            stage,
            expect,
        }),
        TodoVerb::Archive { card, scope } => scope.guard(BoardOp::Archive {
            card: card_ref(&card)?,
        }),
        TodoVerb::Accept { card, scope } => scope.guard(BoardOp::Proposal {
            card: card_ref(&card)?,
            accept: true,
        }),
        TodoVerb::Dismiss { card, scope } => scope.guard(BoardOp::Proposal {
            card: card_ref(&card)?,
            accept: false,
        }),
        TodoVerb::Note {
            card,
            text,
            id,
            scope,
        } => scope.guard(BoardOp::AddNote {
            id: NoteId(client_id(id, &mut minted)?),
            card: card_ref(&card)?,
            text,
            author: caller.author.clone(),
        }),
        TodoVerb::Block {
            card,
            by,
            on,
            scope,
        } => scope.guard(BoardOp::Block {
            card: card_ref(&card)?,
            by: card_ref(&by)?,
            on,
        }),
        TodoVerb::List {
            scope,
            stage,
            archived,
            page,
        } => BoardOp::Read(BoardRead::Cards {
            scope: scope.scope(caller.project.as_deref()),
            stage,
            include_archived: archived,
            page,
        }),
        TodoVerb::Show { card, scope } => BoardOp::Read(BoardRead::Card {
            scope: scope.scope(None),
            card: card_ref(&card)?,
        }),
        TodoVerb::Board {
            scope,
            since,
            away_since,
            page,
        } => BoardOp::Read(BoardRead::Board {
            scope: scope.scope(caller.project.as_deref()),
            since,
            away_since,
            page,
        }),
    };
    loop {
        let result = request(&mut conn, op.clone())
            .await
            .map_err(|why| with_rerun_id(why, minted.as_deref()))?;
        if json {
            println!("{}", result_json(&result)?);
            return Ok(());
        }
        print_human(&result);
        if !turn_page(&mut op, &result) {
            return Ok(());
        }
    }
}

fn with_rerun_id(why: anyhow::Error, minted: Option<&str>) -> anyhow::Error {
    match (Refusal::of(&why.to_string()), minted) {
        (Some(Refusal::OutcomeUnknown), Some(id)) => anyhow::anyhow!("{why} with --id {id}"),
        _ => why,
    }
}

fn turn_page(op: &mut BoardOp, result: &BoardResult) -> bool {
    let next = match result {
        BoardResult::Cards(page) => page.next.clone(),
        BoardResult::Board(BoardPoll::Board { next, .. }) => next.clone(),
        _ => None,
    };
    match (op, next) {
        (
            BoardOp::Read(BoardRead::Cards { page, .. } | BoardRead::Board { page, .. }),
            Some(next),
        ) => {
            *page = Some(next);
            true
        }
        _ => false,
    }
}

fn result_json(result: &BoardResult) -> Result<serde_json::Value> {
    Ok(match result {
        BoardResult::Board(poll) => serde_json::to_value(poll)?,
        BoardResult::Cards(page) => serde_json::to_value(page)?,
        BoardResult::Card(card) => serde_json::to_value(card)?,
        BoardResult::Show(show) => serde_json::to_value(show)?,
        BoardResult::Artifacts(page) => serde_json::to_value(page)?,
        BoardResult::Artifact(artifact) => serde_json::to_value(artifact)?,
        BoardResult::Note(note) => serde_json::to_value(note)?,
        BoardResult::Run(receipt) => serde_json::to_value(receipt)?,
        BoardResult::Done => serde_json::Value::Null,
    })
}

fn card_line(card: &Card) -> String {
    let mut line = format!("{} [{}] {}", card.label, card.stage.as_str(), card.title);
    if card.proposed {
        line.push_str("  (proposed)");
    }
    if card.archived {
        line.push_str("  (archived)");
    }
    if !card.blocked_by.is_empty() {
        line.push_str(&format!("  blocked by {}", card.blocked_by.join(", ")));
    }
    line
}

fn print_human(result: &BoardResult) {
    match result {
        BoardResult::Card(card) => println!("{}", card_line(card)),
        BoardResult::Cards(page) => {
            if page.items.is_empty() {
                println!("no cards — add one with `nebula todo add <title>`");
            }
            for card in &page.items {
                println!("{}", card_line(card));
            }
        }
        BoardResult::Show(show) => {
            println!("{}", card_line(&show.card));
            for note in &show.log {
                println!("  {}: {}", note.author.name, note.text);
            }
        }
        BoardResult::Note(note) => println!("note added: {}", note.text),
        BoardResult::Board(BoardPoll::Unchanged { version }) => println!("unchanged at {version}"),
        BoardResult::Board(BoardPoll::Board { board, .. }) => {
            for card in &board.cards {
                println!("{}", card_line(card));
            }
        }
        other => println!("{other:?}"),
    }
}

/// A refusal exits with its own code (5 not found, 6 conflict) so callers
/// such as the gateway can tell them apart; anything else stays an error.
pub fn exit_on_refusal(result: Result<()>) -> Result<()> {
    match result {
        Err(why) => match Refusal::of(&why.to_string()) {
            Some(refusal) => {
                eprintln!("nebula: {why:#}");
                std::process::exit(refusal.exit_code());
            }
            None => Err(why),
        },
        ok => ok,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nebula_core::{ProjectId, WorktreeId};
    use std::path::Path;

    #[test]
    fn cwd_matches_only_local_checkouts() {
        let project = |id: &str, host: Option<&str>| Project {
            workspace_id: Default::default(),
            id: ProjectId(id.into()),
            name: "app".into(),
            repo_path: "/src/app".into(),
            sort_order: 0,
            divider_after: false,
            divider_before: false,
            divider_label: None,
            divider_before_label: None,
            host: host.map(str::to_owned),
        };
        let worktree = |id: &str, project: &str| Worktree {
            id: WorktreeId(id.into()),
            project_id: ProjectId(project.into()),
            path: "/src/app".into(),
            branch: "main".into(),
            is_main: true,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        let projects = [project("local", None), project("twin", Some("findl"))];
        let worktrees = [worktree("here", "local"), worktree("there", "twin")];
        let found = cwd_checkout(Path::new("/src/app/crates"), &projects, &worktrees);
        assert_eq!(found.map(|w| w.id.0.as_str()), Some("here"));
        let remote_only = cwd_checkout(Path::new("/src/app"), &projects, &worktrees[1..]);
        assert!(remote_only.is_none());
    }
}
