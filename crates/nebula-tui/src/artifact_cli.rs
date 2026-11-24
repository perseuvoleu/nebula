//! `nebula artifact` verbs: a path is copied into the local daemon's
//! artifact store, then published to the board with the caller's card,
//! session and checkout; a URL is published as kind link.

use crate::board_cli::{
    card_ref, check_card, client_id, connect, request, Caller, ScopeFlags, CLOSED,
};
use crate::ipc::{self, Connection};
use anyhow::{bail, Context, Result};
use nebula_core::board::{
    parse_card_ref, Artifact, ArtifactContent, BoardOp, BoardRead, BoardResult, CardRef, Refusal,
    SessionLink,
};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{ArtifactId, ClientRequest, ServerEvent};
use std::path::PathBuf;

const FILE_KINDS: [&str; 5] = ["recap", "e2e-run", "diagram", "ticket", "prototype"];

pub enum CardChoice {
    Caller,
    Card(String),
    None,
}

pub struct AddArgs {
    pub source: String,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub project: Option<String>,
    pub card: CardChoice,
    pub id: Option<String>,
}

pub enum ArtifactVerb {
    Add(AddArgs),
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

pub(crate) fn artifact_id(text: &str) -> Result<ArtifactId> {
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

async fn exchange(conn: &mut Connection, req: ClientRequest) -> Result<ServerEvent> {
    write_frame(&mut conn.stream, &req).await?;
    loop {
        match read_frame::<ServerEvent, _>(&mut conn.stream).await? {
            Some(ServerEvent::Error {
                req_id: Some(1),
                message,
            }) => bail!("{message}"),
            Some(
                reply @ (ServerEvent::ArtifactStored { req_id: 1, .. }
                | ServerEvent::ArtifactLocated { req_id: 1, .. }
                | ServerEvent::Ack { req_id: 1, .. }),
            ) => return Ok(reply),
            Some(_) => continue,
            None => bail!(CLOSED),
        }
    }
}

async fn store(
    conn: &mut Connection,
    id: &ArtifactId,
    kind: &str,
    source: PathBuf,
) -> Result<(ArtifactContent, bool)> {
    let req = ClientRequest::StoreArtifact {
        req_id: 1,
        id: id.clone(),
        kind: kind.to_string(),
        source,
    };
    match exchange(conn, req).await? {
        ServerEvent::ArtifactStored {
            content, created, ..
        } => Ok((content, created)),
        other => bail!("the daemon answered a store with {other:?}"),
    }
}

fn outcome_known(why: &anyhow::Error) -> bool {
    why.downcast_ref::<std::io::Error>().is_none()
        && why.to_string() != CLOSED
        && Refusal::of(&why.to_string()) != Some(Refusal::OutcomeUnknown)
}

pub(crate) async fn unpublished(
    conn: &mut Connection,
    id: &ArtifactId,
    created: bool,
    why: anyhow::Error,
) -> anyhow::Error {
    if !outcome_known(&why) {
        return anyhow::anyhow!(Refusal::OutcomeUnknown.message(format!(
            "artifact {id} may have been published ({why:#}); rerun with --id {id}"
        )));
    }
    if created {
        let req = ClientRequest::DiscardArtifact {
            req_id: 1,
            id: id.clone(),
        };
        if let Err(e) = exchange(conn, req).await {
            eprintln!("nebula: the stored copy of artifact {id} stays: {e:#}");
        }
    }
    why
}

async fn add(conn: &mut Connection, caller: Caller, args: AddArgs) -> Result<(BoardOp, bool)> {
    let AddArgs {
        source,
        kind,
        title,
        project,
        card,
        id,
    } = args;
    let card = match card {
        CardChoice::Caller => caller.card.clone(),
        CardChoice::Card(text) => Some(check_card(conn, &text).await?),
        CardChoice::None => None,
    };
    let id = ArtifactId(client_id(id, &mut None)?);
    let (kind, content, default_title, created) = if let Some(url) = url(&source)? {
        let content = ArtifactContent::Url { url };
        ("link".to_string(), content, source, false)
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
        let (content, created) = store(conn, &id, &kind, path).await?;
        (kind, content, name, created)
    };
    let session = caller.author.agent.clone().map(|id| SessionLink {
        host: caller.author.host.clone(),
        id,
        name: caller.author.name.clone(),
    });
    let op = BoardOp::Publish(Artifact {
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
    });
    Ok((op, created))
}

pub async fn run(verb: ArtifactVerb, json: bool) -> Result<()> {
    let mut conn = connect().await?;
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(&mut conn).await?;
    let caller = Caller::resolve(&projects, &worktrees, &agents)?;
    let mut opening = false;
    let mut created = false;
    let mut op = match verb {
        ArtifactVerb::Add(args) => {
            let (op, stored) = add(&mut conn, caller, args).await?;
            created = stored;
            op
        }
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
        let result = match (&op, request(&mut conn, op.clone()).await) {
            (BoardOp::Publish(artifact), Err(why)) => {
                return Err(unpublished(&mut conn, &artifact.id, created, why).await);
            }
            (_, result) => result?,
        };
        if opening {
            let BoardResult::Artifact(artifact) = result else {
                bail!("the board answered an artifact read with {result:?}");
            };
            return open(&mut conn, &artifact).await;
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

async fn open(conn: &mut Connection, a: &Artifact) -> Result<()> {
    let target = match &a.content {
        ArtifactContent::Files { host, .. } if *host != nebula_core::host::hostname() => {
            println!("on {host}: ssh {host} nebula artifact open {}", a.id);
            return Ok(());
        }
        ArtifactContent::Files { entry, .. } => {
            let req = ClientRequest::LocateArtifact {
                req_id: 1,
                id: a.id.clone(),
                entry: entry.clone(),
            };
            match exchange(conn, req).await? {
                ServerEvent::ArtifactLocated { path, .. } => path.display().to_string(),
                other => bail!("the daemon answered a locate with {other:?}"),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_answer_settles_a_publish() {
        assert!(outcome_known(&anyhow::anyhow!(
            Refusal::Conflict.message("taken")
        )));
        assert!(outcome_known(&anyhow::anyhow!("no board configured")));
        let lost = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        assert!(!outcome_known(&anyhow::Error::from(lost)));
        assert!(!outcome_known(&anyhow::anyhow!(CLOSED)));
        assert!(!outcome_known(&anyhow::anyhow!(
            Refusal::OutcomeUnknown.message("link dropped")
        )));
    }
}
