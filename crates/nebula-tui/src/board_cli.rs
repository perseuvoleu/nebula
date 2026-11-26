//! `nebula todo` card verbs: resolve the caller once, send one Board op
//! to the local daemon, print its reply (JSON with --json).

use crate::ipc::{self, Connection};
use anyhow::{bail, Context, Result};
use nebula_core::board::{
    parse_card_ref, Author, BoardOp, BoardPoll, BoardRead, BoardResult, BoardScope, Card,
    CardDraft, CardRef, CardShow, Checkout, Dispatch, DispatchState, Placement, ProjectScope,
    Refusal, Role, RunReceipt, SessionAt, Stage,
};
use nebula_core::codec::write_frame;
use nebula_core::{
    paths, Agent, AgentId, AgentKind, ClientRequest, DispatchId, NoteId, Project, ServerEvent,
    TodoId, Worktree,
};

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
    fn write_scope(self) -> Option<BoardScope> {
        if !self.all && !self.include_home && self.projects.is_empty() {
            return None;
        }
        Some(BoardScope {
            projects: match self.all {
                true => ProjectScope::All,
                false => ProjectScope::Named(self.projects),
            },
            include_home: self.include_home,
        })
    }

    fn guard(self, op: BoardOp) -> BoardOp {
        match self.write_scope() {
            None => op,
            Some(scope) => BoardOp::Scoped {
                scope,
                op: Box::new(op),
            },
        }
    }

    fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for project in &self.projects {
            args.push(format!("--scope={project}"));
        }
        if self.all {
            args.extend(["--scope".to_string(), "*".to_string()]);
        }
        if self.include_home {
            args.push("--include-home".to_string());
        }
        args
    }

    pub(crate) fn scope(self, fallback: Option<&str>) -> BoardScope {
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
        scope: ScopeFlags,
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
    Run {
        card: String,
        role: Role,
        kind: AgentKind,
        host: String,
        worktree: String,
        dispatch: Option<String>,
        scope: ScopeFlags,
        forwarded: bool,
        parent: Option<SessionAt>,
    },
    Approve {
        card: String,
        session: String,
        expected_status_at: i64,
        scope: ScopeFlags,
    },
}

pub fn card_ref(text: &str) -> Result<CardRef> {
    parse_card_ref(text)
        .with_context(|| format!("{text:?} is not a card ref (#12, 12, 12@host or a ULID)"))
}

pub fn dispatch_id(text: &str) -> Result<DispatchId> {
    match parse_card_ref(text) {
        Some(CardRef::Id(canonical)) => Ok(DispatchId(canonical.0)),
        _ => bail!("--dispatch {text:?} is not a ULID"),
    }
}

pub(crate) fn client_id(id: Option<String>) -> Result<String> {
    match id {
        None => Ok(TodoId::generate().0),
        Some(id) => match parse_card_ref(&id) {
            Some(CardRef::Id(canonical)) => Ok(canonical.0),
            _ => bail!("--id {id:?} is not a ULID"),
        },
    }
}

pub(crate) async fn connect() -> Result<Connection> {
    let Ok(stream) = ipc::try_connect(&paths::socket_path()).await else {
        bail!("no nebula daemon is running");
    };
    ipc::handshake(stream).await
}

/// The one request id of a CLI connection: each request awaits its reply
/// before the next is written, so a reply can never answer another request.
pub(crate) const REQ: u64 = 1;

/// Writes `req`, sent with [`REQ`], and returns the first reply to it. An
/// Error reply becomes Err, a reply that never arrives [`ipc::ReplyLost`].
pub(crate) async fn exchange(conn: &mut Connection, req: ClientRequest) -> Result<ServerEvent> {
    write_frame(&mut conn.stream, &req).await?;
    loop {
        match ipc::read_reply(conn).await? {
            ServerEvent::Error {
                req_id: Some(REQ),
                message,
            } => bail!("{message}"),
            reply @ (ServerEvent::BoardReply { req_id: REQ, .. }
            | ServerEvent::ArtifactStored { req_id: REQ, .. }
            | ServerEvent::ArtifactLocated { req_id: REQ, .. }
            | ServerEvent::Ack { req_id: REQ, .. }) => return Ok(reply),
            _ => continue,
        }
    }
}

pub async fn request(conn: &mut Connection, op: BoardOp) -> Result<BoardResult> {
    match exchange(conn, ClientRequest::Board { req_id: REQ, op }).await? {
        ServerEvent::BoardReply { result, .. } => Ok(result),
        other => bail!("the daemon answered a board request with {other:?}"),
    }
}

/// Reads the card `text` names, so a spawn linking it is refused before
/// anything starts.
pub async fn check_card(conn: &mut Connection, text: &str) -> Result<TodoId> {
    Ok(read_card(conn, ScopeFlags::default().scope(None), text)
        .await?
        .card
        .id)
}

/// Refuses a spawn for a dispatch the board has not admitted on this
/// executor: only `todo run` gives a dispatch its id.
pub async fn check_executor(
    conn: &mut Connection,
    dispatch: &DispatchId,
    executor: &str,
) -> Result<()> {
    let op = BoardOp::Read(BoardRead::Dispatch {
        id: dispatch.clone(),
    });
    match request(conn, op).await {
        Ok(BoardResult::Run(receipt)) if !receipt.host.eq_ignore_ascii_case(executor) => {
            bail!(Refusal::Conflict.message(format!(
                "dispatch {} runs on {}, not on {executor}",
                dispatch.as_str(),
                receipt.host
            )))
        }
        Ok(BoardResult::Run(_)) => Ok(()),
        Ok(other) => bail!("the board answered a dispatch read with {other:?}"),
        Err(why) if Refusal::of(&why.to_string()) == Some(Refusal::NotFound) => {
            bail!(Refusal::Conflict.message(format!(
                "dispatch {} was not admitted by `todo run`",
                dispatch.as_str()
            )))
        }
        Err(why) => Err(why),
    }
}

/// The card `text` names within `scope`, with its sessions and log.
pub async fn read_card(conn: &mut Connection, scope: BoardScope, text: &str) -> Result<CardShow> {
    let op = BoardOp::Read(BoardRead::Card {
        scope,
        card: card_ref(text)?,
    });
    match request(conn, op).await? {
        BoardResult::Show(show) => Ok(*show),
        other => bail!("the board answered a card read with {other:?}"),
    }
}

pub async fn run(verb: TodoVerb, json: bool) -> Result<()> {
    let mut conn = connect().await?;
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(&mut conn).await?;
    let caller = Caller::resolve(&projects, &worktrees, &agents)?;
    let mut written = None;
    let mut op = match verb {
        TodoVerb::Run {
            card,
            role,
            kind,
            host,
            worktree,
            dispatch,
            scope,
            forwarded,
            parent,
        } => {
            let dispatch = Dispatch {
                id: match dispatch {
                    Some(text) => dispatch_id(&text)?,
                    None => DispatchId(TodoId::generate().0),
                },
                card: card_ref(&card)?,
                host,
                role,
                kind,
                worktree,
            };
            return run_dispatch(
                &mut conn, dispatch, scope, forwarded, parent, &projects, json,
            )
            .await;
        }
        TodoVerb::Approve {
            card,
            session,
            expected_status_at,
            scope,
        } => {
            return approve(
                &mut conn,
                &card,
                &session,
                expected_status_at,
                scope,
                &agents,
                json,
            )
            .await
        }
        TodoVerb::Add {
            title,
            project,
            home,
            stage,
            propose,
            id,
        } => BoardOp::AddCard(CardDraft {
            id: TodoId(written.insert(client_id(id)?).clone()),
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
            scope,
        } => scope.guard(BoardOp::EditCard {
            card: card_ref(&card)?,
            title,
            project: match (project, home) {
                (_, true) => Some(Placement::Home),
                (Some(project), false) => Some(Placement::Project(project)),
                (None, false) => None,
            },
        }),
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
            id: NoteId(written.insert(client_id(id)?).clone()),
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
    let rerun = |why: anyhow::Error| match written.as_deref() {
        Some(id) => with_rerun(why, &format!("with --id {id}")),
        None => why,
    };
    if json {
        let result = request(&mut conn, op).await.map_err(rerun)?;
        println!("{}", result_json(&result)?);
        return Ok(());
    }
    let first = op.clone();
    let mut pages = Vec::new();
    let mut rereads = 0;
    loop {
        match request(&mut conn, op.clone()).await {
            Err(why)
                if !pages.is_empty()
                    && rereads < REREADS
                    && Refusal::of(&why.to_string()) == Some(Refusal::Conflict) =>
            {
                rereads += 1;
                pages.clear();
                op = first.clone();
            }
            result => {
                let result = result.map_err(rerun)?;
                let more = turn_page(&mut op, &result);
                pages.push(result);
                if !more {
                    break;
                }
            }
        }
    }
    pages.iter().for_each(print_human);
    Ok(())
}

/// How often a human page walk starts over when the board changes under
/// it, before the change is reported.
const REREADS: usize = 3;

async fn run_dispatch(
    conn: &mut Connection,
    dispatch: Dispatch,
    scope: ScopeFlags,
    forwarded: bool,
    parent: Option<SessionAt>,
    projects: &[Project],
    json: bool,
) -> Result<()> {
    let id = dispatch.id.clone();
    let rerun_with = format!("with --dispatch {}", id.as_str());
    let rerun = |why: anyhow::Error| with_rerun(why, &rerun_with);
    let (role, kind, worktree) = (dispatch.role, dispatch.kind, dispatch.worktree.clone());
    let forwarded_scope = scope.args();
    let begun = request(conn, scope.guard(BoardOp::BeginRun(dispatch)))
        .await
        .map_err(rerun)?;
    let receipt = run_receipt(begun)?;
    match receipt.state {
        DispatchState::Started => return print_receipt(&receipt, json),
        DispatchState::Failed => {
            print_receipt(&receipt, json)?;
            bail!(Refusal::Conflict.message(format!(
                "dispatch {} already failed; a new run needs a new dispatch id",
                id.as_str()
            )));
        }
        DispatchState::Pending => {}
    }
    let project = receipt
        .project
        .clone()
        .context("the board admitted the run without a project")?;
    let host = receipt.host.clone();
    let here = nebula_core::host::hostname();
    let project = match run_target(projects, &here, &host, &project) {
        Target::Here(project) => project,
        Target::Missing => {
            let missing = Refusal::Conflict.message(format!("{host} has no project \"{project}\""));
            return end_run(conn, &id, None, Some(missing), json).await;
        }
        Target::Over if forwarded => {
            bail!(Refusal::Conflict.message(format!("{host} is not this host ({here})")))
        }
        Target::Over => {
            let card = receipt.card.as_str();
            let worktree_word = format!("--worktree={worktree}");
            let mut words = vec![
                "nebula",
                "todo",
                "run",
                card,
                "--role",
                role.as_str(),
                "--kind",
                kind.as_str(),
                "--host",
                &host,
                &worktree_word,
                "--dispatch",
                id.as_str(),
                "--forwarded",
            ];
            words.extend(forwarded_scope.iter().map(String::as_str));
            let origin = parent.or_else(|| {
                crate::ipc::env_agent_id().map(|id| SessionAt {
                    host: here.clone(),
                    id: AgentId(id),
                })
            });
            let parent_words = origin.map(|p| {
                [
                    format!("--parent-host={}", p.host),
                    format!("--parent-id={}", p.id.as_str()),
                ]
            });
            words.extend(parent_words.iter().flatten().map(String::as_str));
            if json {
                words.push("--json");
            }
            return on_host(&host, &words, &rerun_with);
        }
    };
    let worktree = match worktree.as_str() {
        "new" => fresh_worktree(conn, &project, &receipt.card, &id).await?,
        _ => worktree,
    };
    let spawned = crate::ipc::spawn_agent(crate::ipc::NewAgentOpts {
        worktree: Some(worktree),
        project: Some(project),
        kind: kind.as_str().to_string(),
        model: None,
        effort: None,
        name: None,
        prompt: None,
        writer: match role {
            Role::Worker | Role::Fix => nebula_core::WriterClaim::Exclusive,
            _ => nebula_core::WriterClaim::Off,
        },
        writer_wait: std::time::Duration::ZERO,
        code_mode_off: false,
        card: Some(receipt.card.0.clone()),
        no_card: false,
        role: Some(role),
        dispatch: Some(id.clone()),
        parent,
    })
    .await;
    let (agent, error) = match spawned {
        Ok(created)
            if created["role"] != serde_json::to_value(role)?
                || created["kind"] != kind.as_str() =>
        {
            bail!(Refusal::Conflict.message(format!(
                "dispatch {} started \"{}\" as {} {}, not {} {}",
                id.as_str(),
                created["name"].as_str().unwrap_or_default(),
                created["role"].as_str().unwrap_or("no role"),
                created["kind"].as_str().unwrap_or_default(),
                role.as_str(),
                kind.as_str()
            )))
        }
        Ok(created) => (
            Some(AgentId(
                created["id"]
                    .as_str()
                    .context("the spawn named no session")?
                    .to_string(),
            )),
            None,
        ),
        Err(why) if Refusal::of(&why.to_string()) == Some(Refusal::WriterHeld) => {
            (None, Some(why.to_string()))
        }
        Err(why) if outcome_unknown(&why) => return Err(rerun(why)),
        Err(why) => bail!(
            "{why:#}; dispatch {} stays pending, rerun with --dispatch {}",
            id.as_str(),
            id.as_str()
        ),
    };
    end_run(conn, &id, agent, error, json).await
}

enum Target {
    Here(String),
    Over,
    Missing,
}

/// Where a run's project lives: this daemon's own checkout when the target
/// is this host, else a mirror of the target, else ssh to the target, which
/// resolves it there. A mirror never stands in for this host's project.
fn run_target(projects: &[Project], here: &str, host: &str, project: &str) -> Target {
    if host.eq_ignore_ascii_case(here) {
        return match projects
            .iter()
            .any(|p| p.name == project && p.host.is_none())
        {
            true => Target::Here(project.to_string()),
            false => Target::Missing,
        };
    }
    match projects.iter().find_map(|p| {
        p.host
            .as_deref()
            .filter(|h| p.name == project && h.eq_ignore_ascii_case(host))
    }) {
        Some(mirror) => Target::Here(format!("{project}@{mirror}")),
        None => Target::Over,
    }
}

fn on_host(host: &str, words: &[&str], rerun: &str) -> Result<()> {
    match crate::run_effect_on_host(host, crate::session_open::ssh_argv(host, words)?, rerun)? {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

async fn end_run(
    conn: &mut Connection,
    id: &DispatchId,
    agent: Option<AgentId>,
    error: Option<String>,
    json: bool,
) -> Result<()> {
    let ended = request(
        conn,
        BoardOp::EndRun {
            dispatch: id.clone(),
            agent,
            error: error.clone(),
        },
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "{} with --dispatch {}",
            Refusal::OutcomeUnknown.message(format!(
                "the board did not record how dispatch {} ended",
                id.as_str()
            )),
            id.as_str()
        )
    })?;
    let receipt = run_receipt(ended)?;
    print_receipt(&receipt, json)?;
    match (receipt.state, error) {
        (DispatchState::Failed, Some(error)) => bail!("{error}"),
        _ => Ok(()),
    }
}

async fn fresh_worktree(
    conn: &mut Connection,
    project: &str,
    card: &TodoId,
    dispatch: &DispatchId,
) -> Result<String> {
    let card = read_card(conn, ScopeFlags::default().scope(None), card.as_str())
        .await?
        .card;
    let branch = format!("card-{}-{}", card.number, dispatch.as_str().to_lowercase());
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(conn).await?;
    let project = ipc::resolve_project(&projects, &worktrees, &agents, Some(project))?;
    let exists = |worktrees: &[Worktree]| {
        worktrees
            .iter()
            .any(|w| w.project_id == project.id && w.branch == branch)
    };
    if !exists(&worktrees) {
        if let Err(why) = ipc::create_worktree_path(conn, &project.id, &branch, None).await {
            let (_, now, _) = ipc::subscribe_snapshot(conn).await?;
            if !exists(&now) {
                return Err(why);
            }
        }
    }
    Ok(branch)
}

fn run_receipt(result: BoardResult) -> Result<RunReceipt> {
    match result {
        BoardResult::Run(receipt) => Ok(receipt),
        other => bail!("the board answered a run with {other:?}"),
    }
}

fn print_receipt(receipt: &RunReceipt, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_value(receipt)?);
        return Ok(());
    }
    let state = serde_json::to_value(receipt.state)?;
    match &receipt.session {
        Some(session) => println!(
            "dispatch {} {}: session {} on {}",
            receipt.dispatch.as_str(),
            state.as_str().unwrap_or_default(),
            session.id,
            session.host
        ),
        None => println!(
            "dispatch {} {}",
            receipt.dispatch.as_str(),
            state.as_str().unwrap_or_default()
        ),
    }
    Ok(())
}

async fn approve(
    conn: &mut Connection,
    card: &str,
    session: &str,
    expected_status_at: i64,
    scope: ScopeFlags,
    agents: &[Agent],
    json: bool,
) -> Result<()> {
    let forwarded = scope.args();
    let read_scope = scope
        .write_scope()
        .unwrap_or_else(|| ScopeFlags::default().scope(None));
    let show = read_card(conn, read_scope, card).await?;
    let found = show
        .sessions
        .iter()
        .find(|s| s.id.as_str() == session)
        .with_context(|| {
            Refusal::NotFound.message(format!("{} has no session {session}", show.card.label))
        })?;
    let rerun = format!(
        "for session {} with --expected-status-at {expected_status_at}",
        found.id.as_str()
    );
    if !found
        .host
        .eq_ignore_ascii_case(&nebula_core::host::hostname())
        && !agents.iter().any(|a| a.id == found.id)
    {
        let at = expected_status_at.to_string();
        let mut words = vec![
            "nebula",
            "todo",
            "approve",
            show.card.id.as_str(),
            "--session",
            found.id.as_str(),
            "--expected-status-at",
            &at,
        ];
        words.extend(forwarded.iter().map(String::as_str));
        if json {
            words.push("--json");
        }
        return on_host(&found.host, &words, &rerun);
    }
    exchange(
        conn,
        ClientRequest::ApproveSession {
            req_id: REQ,
            id: found.id.clone(),
            expected_status_at,
        },
    )
    .await
    .map_err(|why| with_rerun(why, &rerun))?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "card": show.card.id,
                "session": {"host": found.host, "id": found.id},
                "approved": true,
            })
        );
    } else {
        println!("approved {} on {}", found.name, found.host);
    }
    Ok(())
}

/// The board said a write's outcome is unknown, or the connection failed
/// once the request could have gone out.
pub(crate) fn outcome_unknown(why: &anyhow::Error) -> bool {
    why.downcast_ref::<std::io::Error>().is_some()
        || why.downcast_ref::<ipc::ReplyLost>().is_some()
        || Refusal::of(&why.to_string()) == Some(Refusal::OutcomeUnknown)
}

/// A write whose outcome is unknown names what reruns it.
fn with_rerun(why: anyhow::Error, rerun: &str) -> anyhow::Error {
    if !outcome_unknown(&why) {
        return why;
    }
    match Refusal::of(&why.to_string()) {
        Some(Refusal::OutcomeUnknown) => anyhow::anyhow!("{why} {rerun}"),
        _ => anyhow::anyhow!(
            "{} {rerun}",
            Refusal::OutcomeUnknown.message(format!("{why:#}"))
        ),
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
            if let Some(checkout) = &show.card.checkout {
                let kind = if checkout.worktree {
                    "worktree"
                } else {
                    "primary"
                };
                println!("  on {} {} ({kind})", checkout.host, checkout.branch);
            }
            let now = crate::hosts::now_ms();
            for session in &show.sessions {
                let role = session.role.map_or("-", |r| r.as_str());
                let mut line = format!(
                    "  {role} {} on {}: {} {}",
                    session.name,
                    session.host,
                    session.kind.as_str(),
                    session.status.as_str()
                );
                let ago = crate::hosts::ago_label(now - session.status_changed_at);
                if !ago.is_empty() {
                    line.push_str(&format!(" {ago}"));
                }
                if session.writer {
                    line.push_str(", writer");
                }
                line.push_str(if session.alive { ", live" } else { ", stopped" });
                if session.stale {
                    line.push_str(", stale");
                }
                println!("{line}");
            }
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
            for host in &board.hosts {
                let slots = host
                    .writer_slots
                    .map(|s| format!("/{s}"))
                    .unwrap_or_default();
                println!(
                    "{}: {}{slots} writers, {} checkouts, {} need you",
                    host.host,
                    host.writers,
                    host.checkouts.len(),
                    host.needs_you
                );
            }
            let away = &board.away;
            if !away.finished.is_empty() || !away.needs_you.is_empty() {
                println!(
                    "while you were away: {} cards finished, {} sessions need you",
                    away.finished.len(),
                    away.needs_you.len()
                );
            }
            if !away.finished.is_empty() {
                let refs: Vec<&str> = away
                    .finished
                    .iter()
                    .map(|id| {
                        board
                            .cards
                            .iter()
                            .find(|c| &c.id == id)
                            .map_or(id.as_str(), |c| c.label.as_str())
                    })
                    .collect();
                println!("  finished: {}", refs.join(", "));
            }
            for session in &away.needs_you {
                println!("  needs you: {} on {}", session.id, session.host);
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
    use nebula_core::codec::read_frame;
    use nebula_core::{ProjectId, WorktreeId};
    use std::path::Path;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    #[test]
    fn only_an_answer_settles_a_write() {
        assert!(!outcome_unknown(&anyhow::anyhow!(
            Refusal::Conflict.message("taken")
        )));
        assert!(!outcome_unknown(&anyhow::anyhow!("no board configured")));
        let lost = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        assert!(outcome_unknown(&anyhow::Error::from(lost)));
        assert!(outcome_unknown(&anyhow::Error::from(ipc::CLOSED)));
        assert!(outcome_unknown(&anyhow::anyhow!(
            Refusal::OutcomeUnknown.message("link dropped")
        )));
    }

    async fn approve_against(
        daemon: impl FnOnce(UnixStream) -> tokio::task::JoinHandle<()>,
    ) -> anyhow::Error {
        let (cli, end) = UnixStream::pair().unwrap();
        let _daemon = daemon(end);
        let mut conn = Connection {
            stream: cli,
            daemon_pid: 0,
        };
        let req = ClientRequest::ApproveSession {
            req_id: REQ,
            id: AgentId("s1".into()),
            expected_status_at: 7,
        };
        let why = tokio::time::timeout(
            std::time::Duration::from_secs(3600),
            exchange(&mut conn, req),
        )
        .await
        .expect("the approve still waits an hour on")
        .unwrap_err();
        with_rerun(why, "for session s1 with --expected-status-at 7")
    }

    fn names_the_rerun(why: &anyhow::Error) {
        let text = why.to_string();
        assert_eq!(Refusal::of(&text), Some(Refusal::OutcomeUnknown), "{text}");
        assert!(text.contains("--expected-status-at 7"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_approve_the_daemon_never_answers_ends_outcome_unknown() {
        names_the_rerun(
            &approve_against(|mut end| {
                tokio::spawn(async move {
                    let _ = read_frame::<ClientRequest, _>(&mut end).await;
                    std::future::pending::<()>().await;
                })
            })
            .await,
        );
    }

    #[tokio::test]
    async fn an_approve_reply_cut_mid_frame_ends_outcome_unknown() {
        names_the_rerun(
            &approve_against(|mut end| {
                tokio::spawn(async move {
                    let _ = read_frame::<ClientRequest, _>(&mut end).await;
                    end.write_all(&100u32.to_le_bytes()).await.unwrap();
                    end.write_all(&[0x81; 10]).await.unwrap();
                })
            })
            .await,
        );
    }

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

    #[test]
    fn run_target_never_takes_a_mirror_for_this_host() {
        let mirror = Project {
            workspace_id: Default::default(),
            id: ProjectId("twin".into()),
            name: "app".into(),
            repo_path: "/src/app".into(),
            sort_order: 0,
            divider_after: false,
            divider_before: false,
            divider_label: None,
            divider_before_label: None,
            host: Some("findl".into()),
        };
        let projects = [mirror];
        assert!(matches!(
            run_target(&projects, "mbp", "mbp", "app"),
            Target::Missing
        ));
        assert!(matches!(
            run_target(&projects, "mbp", "findl", "app"),
            Target::Here(flag) if flag == "app@findl"
        ));
        assert!(matches!(
            run_target(&projects, "mbp", "laptop", "app"),
            Target::Over
        ));
    }
}
