//! The board host copies each decision to RAG in the background through
//! `nebula rag`, which picks the local store or the ssh hop itself, and keeps
//! the note id in meta.rag_note. A decision without one is retried every tick,
//! so one recorded while RAG is down reaches it once RAG answers.

use crate::artifacts::{artifact_from_row, ARTIFACT_COLUMNS};
use crate::store::Store;
use anyhow::{bail, Context, Result};
use nebula_core::board::{Artifact, ArtifactContent};
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use rusqlite::{params, Transaction};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

static KICK: Notify = Notify::const_new();

/// A decision was just recorded: copy it now rather than at the next tick.
pub fn kick() {
    KICK.notify_one();
}

pub fn start(store: Arc<Store>, shutdown: CancellationToken) -> JoinHandle<()> {
    let period = std::env::var("NEBULA_RAG_COPY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60_000)
        .max(50);
    let bound = Duration::from_millis(
        std::env::var("NEBULA_RAG_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120_000),
    );
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(period));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = interval.tick() => {}
                _ = KICK.notified() => {}
            }
            if crate::config::Config::load().board.as_deref() != Some("local") {
                continue;
            }
            copy_pending(&store, bound, &shutdown).await;
        }
    })
}

async fn copy_pending(store: &Store, bound: Duration, shutdown: &CancellationToken) {
    let pending = match store.board_transaction(pending) {
        Ok(pending) => pending,
        Err(e) => {
            tracing::warn!(error = %e, "reading decisions to copy to RAG failed");
            return;
        }
    };
    for decision in pending {
        if shutdown.is_cancelled() {
            return;
        }
        let copied = match copy(&decision, bound, shutdown).await {
            Ok(note) => store.board_transaction(|tx| {
                tx.execute(
                    "UPDATE artifacts SET meta = json_set(meta, '$.rag_note', ?2)
                     WHERE id = ?1 AND json_extract(meta, '$.rag_note') IS NULL",
                    params![decision.id.as_str(), note],
                )?;
                Ok(())
            }),
            Err(e) => Err(e),
        };
        if let Err(e) = copied {
            tracing::warn!(decision = %decision.id, error = %format!("{e:#}"), "copying a decision to RAG failed; it is retried");
            if e.is::<Unreachable>() {
                return;
            }
        }
    }
}

fn pending(tx: &Transaction) -> Result<Vec<Artifact>> {
    Ok(tx
        .prepare(&format!(
            "SELECT {ARTIFACT_COLUMNS} FROM artifacts
             WHERE kind = ?1 AND removed_at IS NULL
               AND json_extract(meta, '$.rag_note') IS NULL
             ORDER BY created_at"
        ))?
        .query_map([nebula_core::board::DECISION_KIND], artifact_from_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// On Linux a binary replaced under a running daemon reads as
/// "<path> (deleted)"; the new binary sits at the same path.
fn nebula() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let text = exe.to_string_lossy();
    Ok(match text.strip_suffix(" (deleted)") {
        Some(path) => PathBuf::from(path),
        None => exe,
    })
}

async fn copy(decision: &Artifact, bound: Duration, shutdown: &CancellationToken) -> Result<i64> {
    let ws = match &decision.project {
        Some(project) => format!("apps/{}/notes", workspace_segment(project)),
        None => "shared".to_string(),
    };
    let why = match &decision.content {
        ArtifactContent::Body { body } => body.trim(),
        _ => "",
    };
    let text = match why {
        "" => decision.title.clone(),
        why => format!("{}\n\n{why}", decision.title),
    };
    if decision.project.is_some() {
        rag(&["ws", "create", &ws, "--force"], None, bound, shutdown).await?;
    }
    let source = format!("decision:{}", decision.id);
    let title = format!("--title={}", decision.title);
    let saved = rag(
        &[
            "--source",
            &source,
            "add",
            "--no-dedup",
            "--ws",
            &ws,
            &title,
            "-",
        ],
        Some(&text),
        bound,
        shutdown,
    )
    .await?;
    note_id(&saved).with_context(|| format!("nebula rag add printed no note id: {saved:?}"))
}

/// RAG gave no answer (a timeout, or ssh's exit 255): the rest of the
/// tick's decisions would fail the same way, so they wait for the next one.
#[derive(Debug)]
struct Unreachable(String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// The child runs in its own process group, so a deadline or shutdown
/// kills an ssh hop and whatever it started too. The group is signalled
/// while the pending wait still holds the leader unreaped, so its id cannot
/// belong to a recycled process.
async fn rag(
    args: &[&str],
    stdin: Option<&str>,
    bound: Duration,
    shutdown: &CancellationToken,
) -> Result<String> {
    let mut child = Command::new(nebula()?)
        .arg("rag")
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .context("run nebula rag")?;
    let group = Pid::from_raw(child.id().context("nebula rag exited before it ran")? as i32);
    let output = async {
        if let Some(text) = stdin {
            let mut pipe = child.stdin.take().context("nebula rag stdin")?;
            pipe.write_all(text.as_bytes()).await?;
        }
        Ok::<_, anyhow::Error>(child.wait_with_output().await?)
    };
    tokio::pin!(output);
    let stopped = tokio::select! {
        out = &mut output => Ok(out?),
        _ = tokio::time::sleep(bound) => Err("gave no answer in time"),
        _ = shutdown.cancelled() => Err("was stopped by daemon shutdown"),
    };
    let out = match stopped {
        Ok(out) => out,
        Err(why) => {
            let _ = killpg(group, Signal::SIGKILL);
            let _ = tokio::time::timeout(Duration::from_secs(1), output).await;
            return Err(Unreachable(format!("nebula rag {} {why}", args.join(" "))).into());
        }
    };
    if out.status.code() == Some(255) {
        return Err(Unreachable(format!(
            "nebula rag {} could not reach RAG: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
        .into());
    }
    if !out.status.success() {
        bail!(
            "nebula rag {} exited {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A project name RAG accepts as a workspace segment is used as it is.
/// Any other gets a slug plus a hash of the exact name, so "Client Portal"
/// and "client-portal" stay in distinct workspaces.
fn workspace_segment(project: &str) -> String {
    let valid = !matches!(project, "." | "..")
        && !project.is_empty()
        && project
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if valid {
        return project.to_string();
    }
    let slug: String = project
        .chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c.to_ascii_lowercase(),
            false => '-',
        })
        .collect();
    let hash = project.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{}-{:08x}", slug.trim_matches('-'), hash as u32)
}

/// `saved note:12 → …`.
fn note_id(saved: &str) -> Option<i64> {
    let (_, rest) = saved.split_once("note:")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}
