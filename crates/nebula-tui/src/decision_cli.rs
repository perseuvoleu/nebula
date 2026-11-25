//! `nebula decision` verbs: a decision is an artifact of kind decision
//! whose title is what was decided and whose body is why; the board host
//! copies each one to RAG.

use crate::artifact_cli::{artifact_id, unpublished};
use crate::board_cli::{client_id, connect, read_card, request, Caller, ScopeFlags};
use crate::ipc;
use anyhow::{bail, Result};
use nebula_core::board::{
    Artifact, ArtifactContent, BoardOp, BoardRead, BoardResult, Refusal, SessionLink,
};
use nebula_core::ArtifactId;

const KIND: &str = "decision";

/// A `nebula decision` verb, as the binary parsed it.
pub enum DecisionVerb {
    Add {
        what: String,
        why: Option<String>,
        card: Option<String>,
        project: Option<String>,
        home: bool,
        id: Option<String>,
    },
    List {
        scope: ScopeFlags,
        query: Option<String>,
        page: Option<String>,
    },
    Show {
        id: String,
    },
}

/// Runs `verb` against the local daemon's board and prints its reply (JSON
/// with `json`). A refusal's message carries its kind for the exit code.
pub async fn run(verb: DecisionVerb, json: bool) -> Result<()> {
    let mut conn = connect().await?;
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(&mut conn).await?;
    let caller = Caller::resolve(&projects, &worktrees, &agents)?;
    let mut op = match verb {
        DecisionVerb::Add {
            what,
            why,
            card,
            project,
            home,
            id,
        } => {
            if what.trim().is_empty() {
                bail!("a decision needs its WHAT");
            }
            let (card, project) = match (card, project) {
                (None, None) if home => (None, None),
                (Some(text), _) => {
                    let card = read_card(&mut conn, &text).await?;
                    (Some(card.id), card.project)
                }
                (None, Some(project)) => (None, Some(project)),
                (None, None) => (caller.card.clone(), caller.project),
            };
            let session = caller.author.agent.clone().map(|id| SessionLink {
                host: caller.author.host.clone(),
                id,
                name: caller.author.name.clone(),
            });
            BoardOp::Publish(Artifact {
                id: ArtifactId(client_id(id, &mut None)?),
                kind: KIND.into(),
                title: what,
                card,
                project,
                session,
                checkout: caller.checkout,
                content: ArtifactContent::Body {
                    body: why.unwrap_or_default(),
                },
                meta: Default::default(),
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis() as i64,
                removed_at: None,
            })
        }
        DecisionVerb::List { scope, query, page } => BoardOp::Read(BoardRead::Artifacts {
            scope: scope.scope(caller.project.as_deref()),
            card: None,
            kind: Some(KIND.into()),
            query,
            page,
        }),
        DecisionVerb::Show { id } => BoardOp::Read(BoardRead::Artifact {
            scope: ScopeFlags::default().scope(None),
            id: artifact_id(&id)?,
        }),
    };
    loop {
        let result = match (&op, request(&mut conn, op.clone()).await) {
            (BoardOp::Publish(decision), Err(why)) => {
                return Err(unpublished(&mut conn, &decision.id, false, why).await);
            }
            (_, result) => result?,
        };
        let next = match result {
            BoardResult::Artifact(decision) => {
                if decision.kind != KIND {
                    bail!(Refusal::NotFound.message(format!("no decision {}", decision.id)));
                }
                match json {
                    true => println!("{}", serde_json::to_value(&decision)?),
                    false => println!("{}", line(&decision)),
                }
                None
            }
            BoardResult::Artifacts(page) if json => {
                println!("{}", serde_json::to_value(&page)?);
                None
            }
            BoardResult::Artifacts(page) => {
                if page.items.is_empty() {
                    println!(
                        "no decisions — record one with `nebula decision add <what> --why <why>`"
                    );
                }
                for decision in &page.items {
                    println!("{}", line(decision));
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

fn line(decision: &Artifact) -> String {
    let mut line = format!("{} {}", decision.id, decision.title);
    if let ArtifactContent::Body { body } = &decision.content {
        if !body.is_empty() {
            line.push_str(&format!(" — {body}"));
        }
    }
    line
}
