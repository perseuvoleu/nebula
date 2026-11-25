//! `nebula artifact` verbs: a path is copied into the local daemon's
//! artifact store, then published to the board with the caller's card,
//! session and checkout; a URL is published as kind link.

use crate::board_cli::{
    card_ref, check_card, client_id, connect, exchange, request, Caller, ScopeFlags, REQ,
};
use crate::ipc::{self, Connection};
use anyhow::{bail, Context, Result};
use nebula_core::board::{
    parse_card_ref, Artifact, ArtifactContent, BoardOp, BoardRead, BoardResult, CardRef, Refusal,
    SessionLink,
};
use nebula_core::{ArtifactId, ClientRequest, ServerEvent, TodoId};
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

async fn store(
    conn: &mut Connection,
    id: &ArtifactId,
    kind: &str,
    source: PathBuf,
) -> Result<(ArtifactContent, bool)> {
    let req = ClientRequest::StoreArtifact {
        req_id: REQ,
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
        && why.downcast_ref::<ipc::ReplyLost>().is_none()
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
            req_id: REQ,
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
    let project = project.or(caller.project.clone());
    let draft = Draft {
        id,
        kind,
        title: title.unwrap_or(default_title),
        card,
        project,
        content,
    };
    Ok((BoardOp::Publish(draft.by(caller)?), created))
}

/// An artifact before the caller's session, checkout and time are added.
pub(crate) struct Draft {
    pub id: ArtifactId,
    pub kind: String,
    pub title: String,
    pub card: Option<TodoId>,
    pub project: Option<String>,
    pub content: ArtifactContent,
}

impl Draft {
    /// The artifact as `caller` publishes it now.
    pub(crate) fn by(self, caller: Caller) -> Result<Artifact> {
        let session = caller.author.agent.map(|id| SessionLink {
            host: caller.author.host,
            id,
            name: caller.author.name,
        });
        Ok(Artifact {
            id: self.id,
            kind: self.kind,
            title: self.title,
            card: self.card,
            project: self.project,
            session,
            checkout: caller.checkout,
            content: self.content,
            meta: Default::default(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis() as i64,
            removed_at: None,
        })
    }
}

/// How a verb family prints the board's artifact answers. With `kind`, an
/// artifact of another kind is not found.
pub(crate) struct Printer {
    pub kind: Option<&'static str>,
    pub line: fn(&Artifact) -> String,
    pub empty: &'static str,
}

const ARTIFACTS: Printer = Printer {
    kind: None,
    line,
    empty: "no artifacts — publish one with `nebula artifact add <path> --kind <kind>`",
};

/// Sends `op` and prints its answer, walking every page of a human list;
/// a publish that fails is undone or named for a rerun by [`unpublished`].
pub(crate) async fn answer(
    conn: &mut Connection,
    mut op: BoardOp,
    created: bool,
    json: bool,
    printer: &Printer,
) -> Result<()> {
    loop {
        let result = match (&op, request(conn, op.clone()).await) {
            (BoardOp::Publish(artifact), Err(why)) => {
                return Err(unpublished(conn, &artifact.id, created, why).await);
            }
            (_, result) => result?,
        };
        let next = match result {
            BoardResult::Artifact(a) => {
                if let Some(kind) = printer.kind.filter(|kind| *kind != a.kind) {
                    bail!(Refusal::NotFound.message(format!("no {kind} {}", a.id)));
                }
                match json {
                    true => println!("{}", serde_json::to_value(&a)?),
                    false => println!("{}", (printer.line)(&a)),
                }
                None
            }
            BoardResult::Artifacts(page) if json => {
                println!("{}", serde_json::to_value(&page)?);
                None
            }
            BoardResult::Artifacts(page) => {
                if page.items.is_empty() {
                    println!("{}", printer.empty);
                }
                for a in &page.items {
                    println!("{}", (printer.line)(a));
                }
                page.next
            }
            other => bail!("the board answered with {other:?}"),
        };
        match (&mut op, next) {
            (BoardOp::Read(BoardRead::Artifacts { page, .. }), Some(next)) => *page = Some(next),
            _ => return Ok(()),
        }
    }
}

pub async fn run(verb: ArtifactVerb, json: bool) -> Result<()> {
    let mut conn = connect().await?;
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(&mut conn).await?;
    let caller = Caller::resolve(&projects, &worktrees, &agents)?;
    let mut opening = false;
    let mut created = false;
    let op = match verb {
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
    if opening {
        let result = request(&mut conn, op).await?;
        let BoardResult::Artifact(artifact) = result else {
            bail!("the board answered an artifact read with {result:?}");
        };
        return open(&mut conn, &artifact).await;
    }
    answer(&mut conn, op, created, json, &ARTIFACTS).await
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
                req_id: REQ,
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
        assert!(!outcome_known(&anyhow::Error::from(ipc::CLOSED)));
        assert!(!outcome_known(&anyhow::anyhow!(
            Refusal::OutcomeUnknown.message("link dropped")
        )));
    }
}
