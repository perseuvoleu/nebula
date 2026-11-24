//! `nebula artifact` verbs: a path is copied into the local daemon's
//! artifact store, then published to the board with the caller's card,
//! session and checkout; a URL is published as kind link.

use crate::board_cli::{card_ref, check_card, connect, request, Caller, ScopeFlags};
use crate::ipc::{self, Connection};
use anyhow::{bail, Context, Result};
use nebula_core::board::{
    parse_card_ref, Artifact, ArtifactContent, BoardOp, BoardRead, BoardResult, CardRef,
    SessionLink,
};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{paths, ArtifactId, ClientRequest, ServerEvent};
use std::path::PathBuf;

const FILE_KINDS: [&str; 5] = ["recap", "e2e-run", "diagram", "ticket", "prototype"];

pub enum CardChoice {
    Caller,
    Card(String),
    None,
}

pub enum ArtifactVerb {
    Add {
        source: String,
        kind: Option<String>,
        title: Option<String>,
        project: Option<String>,
        card: CardChoice,
    },
    List {
        scope: ScopeFlags,
        card: Option<String>,
        kind: Option<String>,
        page: Option<String>,
    },
    Show {
        id: String,
    },
    Open {
        id: String,
    },
    Rm {
        id: String,
    },
}

fn artifact_id(text: &str) -> Result<ArtifactId> {
    match parse_card_ref(text) {
        Some(CardRef::Id(id)) => Ok(ArtifactId(id.0)),
        _ => bail!("{text:?} is not an artifact id (a ULID)"),
    }
}

fn url(text: &str) -> Result<Option<String>> {
    let Some((scheme, rest)) = text.split_once("://") else {
        return Ok(None);
    };
    if !["http", "https"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
    {
        return Ok(None);
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    if host.is_empty() {
        bail!("{text} has no host");
    }
    Ok(Some(text.to_string()))
}

async fn store(
    conn: &mut Connection,
    id: &ArtifactId,
    kind: &str,
    source: PathBuf,
) -> Result<ArtifactContent> {
    let req_id = 1;
    let req = ClientRequest::StoreArtifact {
        req_id,
        id: id.clone(),
        kind: kind.to_string(),
        source,
    };
    write_frame(&mut conn.stream, &req).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::ArtifactStored { req_id: r, content }) if r == req_id => {
                return Ok(content)
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

async fn add(
    conn: &mut Connection,
    caller: Caller,
    source: String,
    kind: Option<String>,
    title: Option<String>,
    project: Option<String>,
    card: CardChoice,
) -> Result<BoardOp> {
    let card = match card {
        CardChoice::Caller => caller.card.clone(),
        CardChoice::Card(text) => Some(check_card(conn, &text).await?),
        CardChoice::None => None,
    };
    let id = ArtifactId::generate();
    let (kind, content, default_title) = if let Some(url) = url(&source)? {
        let content = ArtifactContent::Url { url };
        ("link".to_string(), content, source)
    } else {
        let kind = kind.context("--kind is required for a path")?;
        if !FILE_KINDS.contains(&kind.as_str()) {
            bail!("{kind:?} is not a file kind ({})", FILE_KINDS.join(", "));
        }
        let path = std::path::absolute(&source)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.clone());
        let content = store(conn, &id, &kind, path).await?;
        (kind, content, name)
    };
    let session = caller.author.agent.clone().map(|id| SessionLink {
        host: caller.author.host.clone(),
        id,
        name: caller.author.name.clone(),
    });
    Ok(BoardOp::Publish(Artifact {
        id,
        kind,
        title: title.unwrap_or(default_title),
        card,
        project: project.or(caller.project),
        session,
        checkout: caller.checkout,
        content,
        meta: Default::default(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as i64,
        removed_at: None,
    }))
}

pub async fn run(verb: ArtifactVerb, json: bool) -> Result<()> {
    let mut conn = connect().await?;
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(&mut conn).await?;
    let caller = Caller::resolve(&projects, &worktrees, &agents)?;
    let mut opening = false;
    let mut op = match verb {
        ArtifactVerb::Add {
            source,
            kind,
            title,
            project,
            card,
        } => add(&mut conn, caller, source, kind, title, project, card).await?,
        ArtifactVerb::List {
            scope,
            card,
            kind,
            page,
        } => BoardOp::Read(BoardRead::Artifacts {
            scope: scope.scope(caller.project.as_deref()),
            card: card.as_deref().map(card_ref).transpose()?,
            kind,
            query: None,
            page,
        }),
        ArtifactVerb::Show { id } => show_op(&id)?,
        ArtifactVerb::Open { id } => {
            opening = true;
            show_op(&id)?
        }
        ArtifactVerb::Rm { id } => BoardOp::RemoveArtifact {
            id: artifact_id(&id)?,
        },
    };
    loop {
        let result = request(&mut conn, op.clone()).await?;
        if opening {
            let BoardResult::Artifact(artifact) = result else {
                bail!("the board answered an artifact read with {result:?}");
            };
            return open(&artifact);
        }
        if json {
            let value = match &result {
                BoardResult::Artifact(a) => serde_json::to_value(a)?,
                BoardResult::Artifacts(page) => serde_json::to_value(page)?,
                other => bail!("the board answered with {other:?}"),
            };
            println!("{value}");
            return Ok(());
        }
        let next = match &result {
            BoardResult::Artifact(a) => {
                println!("{}", line(a));
                None
            }
            BoardResult::Artifacts(page) => {
                if page.items.is_empty() {
                    println!("no artifacts — publish one with `nebula artifact add <path> --kind <kind>`");
                }
                for a in &page.items {
                    println!("{}", line(a));
                }
                page.next.clone()
            }
            other => bail!("the board answered with {other:?}"),
        };
        match (&mut op, next) {
            (BoardOp::Read(BoardRead::Artifacts { page, .. }), Some(next)) => *page = Some(next),
            _ => return Ok(()),
        }
    }
}

fn show_op(id: &str) -> Result<BoardOp> {
    Ok(BoardOp::Read(BoardRead::Artifact {
        scope: ScopeFlags::default().scope(None),
        id: artifact_id(id)?,
    }))
}

fn line(a: &Artifact) -> String {
    let place = match &a.content {
        ArtifactContent::Files { host, entry } => format!("{entry} on {host}"),
        ArtifactContent::Url { url } => url.clone(),
        ArtifactContent::Body { .. } => "text".into(),
    };
    let mut line = format!("{} [{}] {}  ({place})", a.id, a.kind, a.title);
    if a.removed_at.is_some() {
        line.push_str("  (removed)");
    }
    line
}

fn open(a: &Artifact) -> Result<()> {
    let target = match &a.content {
        ArtifactContent::Files { host, .. } if *host != nebula_core::host::hostname() => {
            println!("on {host}: ssh {host} nebula artifact open {}", a.id);
            return Ok(());
        }
        ArtifactContent::Files { entry, .. } => paths::data_dir()
            .join("artifacts")
            .join(a.id.as_str())
            .join(entry)
            .display()
            .to_string(),
        ArtifactContent::Url { url } => url.clone(),
        ArtifactContent::Body { body } => {
            println!("{body}");
            return Ok(());
        }
    };
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = std::process::Command::new(opener).arg(&target).status();
    if !status.is_ok_and(|s| s.success()) {
        bail!("{opener} could not open {target}");
    }
    println!("{target}");
    Ok(())
}
