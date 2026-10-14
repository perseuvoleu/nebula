//! `nebula open --session <id|name>`: resolve one session against the
//! daemon, then hand the attach to a running TUI through a request file in
//! the runtime dir (the `nebula diagram open` mechanism, no daemon protocol
//! involved). The TUI that takes the request answers with an ack naming its
//! pid and whether it could show the session, which is how the command
//! knows a TUI landed it.

use crate::ipc;
use anyhow::{bail, Context, Result};
use nebula_core::{paths, AgentId};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a request stays valid — the same reasoning as diagram requests:
/// a request left behind with no TUI running never attaches later.
const REQUEST_TTL_MS: i64 = 10_000;

/// Longer than the TUI's 500 ms request poll, with room for a busy frame:
/// a request still unclaimed by then means no TUI is running.
const CLAIM_WAIT: Duration = Duration::from_millis(1_500);

/// How long a TUI gets to confirm the attach once it has claimed the
/// request — past its own [`ATTACH_TIMEOUT`], so its answer arrives first.
const ANSWER_WAIT: Duration = Duration::from_secs(20);

/// How long the TUI waits for the daemon's first screen of the session
/// (a remote one comes over ssh) before answering that the attach failed.
pub const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Resolved {
    pub id: AgentId,
    pub name: String,
    pub project: String,
    pub host: Option<String>,
}

/// What a TUI that took a request must answer through [`ack`].
pub struct Request {
    pub agent: AgentId,
    nonce: serde_json::Value,
}

pub fn request_path() -> PathBuf {
    paths::runtime_dir().join("session-open.json")
}

fn ack_path() -> PathBuf {
    paths::runtime_dir().join("session-open-ack.json")
}

/// Resolve `reference` — an agent id, else an unarchived session name — in
/// `project_flag` (`name@host` for a remote twin) or across every project.
pub async fn resolve(reference: &str, project_flag: Option<&str>) -> Result<Resolved> {
    let Ok(stream) = ipc::try_connect(&paths::socket_path()).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = ipc::handshake(stream).await?;
    let (projects, worktrees, agents) = ipc::subscribe_snapshot(&mut conn).await?;
    let scope = match project_flag {
        Some(flag) => Some(ipc::resolve_project(
            &projects,
            &worktrees,
            &agents,
            Some(flag),
        )?),
        None => None,
    };
    let owner = |a: &nebula_core::Agent| {
        let w = worktrees.iter().find(|w| w.id == a.worktree_id)?;
        projects.iter().find(|p| p.id == w.project_id)
    };
    let in_scope: Vec<(&nebula_core::Agent, &nebula_core::Project)> = agents
        .iter()
        .filter_map(|a| owner(a).map(|p| (a, p)))
        .filter(|(_, p)| scope.as_ref().is_none_or(|s| s.id == p.id))
        .collect();
    let by_id: Vec<_> = in_scope
        .iter()
        .filter(|(a, _)| a.id.as_str() == reference)
        .collect();
    let hits: Vec<_> = if by_id.is_empty() {
        in_scope
            .iter()
            .filter(|(a, _)| a.name == reference)
            .collect()
    } else {
        by_id
    };
    let live: Vec<_> = hits.iter().filter(|(a, _)| !a.archived).collect();
    let label = |p: &nebula_core::Project| match &p.host {
        Some(h) => format!("{}@{h}", p.name),
        None => p.name.clone(),
    };
    match live.as_slice() {
        [(a, p)] => Ok(Resolved {
            id: a.id.clone(),
            name: a.name.clone(),
            project: p.name.clone(),
            host: p.host.clone(),
        }),
        [] if !hits.is_empty() => bail!(
            "session \"{reference}\" is archived — `nebula agent unarchive {} --project {}` brings it back",
            hits[0].0.name,
            label(hits[0].1)
        ),
        [] => {
            let place = scope.as_ref().map(|p| format!(" in {}", label(p))).unwrap_or_default();
            bail!("no session \"{reference}\"{place} (pass a name from `nebula agent list --all` or its id)")
        }
        many => bail!(
            "{} sessions are named \"{reference}\" — pass --project or the id: {}",
            many.len(),
            many.iter()
                .map(|(a, p)| format!("{} ({})", label(p), a.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Ask a running TUI to attach `id`; `Some(pid)` of the TUI once its pane
/// holds the session's terminal, `None` when no TUI claimed the request
/// (it is withdrawn), an error when the TUI that claimed it could not show
/// or attach the session.
pub fn hand_to_running_tui(id: &AgentId) -> Result<Option<u32>> {
    let nonce = format!("{}-{}", std::process::id(), crate::app::now_ms());
    let request = request_path();
    if let Some(parent) = request.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomically(
        &request,
        &json!({ "agent": id.as_str(), "nonce": nonce, "ts": crate::app::now_ms() }).to_string(),
    )?;
    let claim_deadline = Instant::now() + CLAIM_WAIT;
    while request.exists() && Instant::now() < claim_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let ours = std::fs::read_to_string(&request)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|v| v["nonce"] == nonce);
    // A failed remove means a TUI claimed it after all.
    if ours && std::fs::remove_file(&request).is_ok() {
        return Ok(None);
    }
    let deadline = Instant::now() + ANSWER_WAIT;
    while Instant::now() < deadline {
        if let Some(answer) = take_ack(&nonce) {
            return answer.map(Some);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the running nebula TUI took the request but never answered")
}

fn take_ack(nonce: &str) -> Option<Result<u32>> {
    let path = ack_path();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    if value["nonce"] != nonce {
        return None;
    }
    let _ = std::fs::remove_file(&path);
    if let Some(error) = value["error"].as_str() {
        return Some(Err(anyhow::anyhow!("{error}")));
    }
    value["pid"].as_u64().map(|pid| Ok(pid as u32))
}

/// The TUI side: claim a pending request — the rename lets exactly one of
/// several running TUIs have it. The caller answers it with [`ack`].
pub fn take_request(path: &Path, now_ms: i64) -> Option<Request> {
    let claimed = path.with_extension(format!("claimed-{}", std::process::id()));
    std::fs::rename(path, &claimed).ok()?;
    let text = std::fs::read_to_string(&claimed).ok()?;
    let _ = std::fs::remove_file(&claimed);
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let age = now_ms - value["ts"].as_i64()?;
    if !(0..=REQUEST_TTL_MS).contains(&age) {
        return None;
    }
    Some(Request {
        agent: AgentId::from(value["agent"].as_str()?.to_owned()),
        nonce: value["nonce"].clone(),
    })
}

/// Answer `request`: landed, or the reason the session could not be shown.
pub fn ack(request: &Request, error: Option<String>) {
    let ack = match error {
        Some(error) => json!({ "nonce": request.nonce, "error": error }),
        None => json!({ "nonce": request.nonce, "pid": std::process::id() }),
    };
    let _ = write_atomically(&ack_path(), &ack.to_string());
}

/// Bring the window of the app running the TUI `tui_pid` — the Ghostty
/// instance `ng` started — to the front, un-minimized. It goes by the app
/// through System Events, never by posting keys; the calling app needs the
/// Automation permission for System Events (macOS asks once). Restoring a
/// minimized window also needs Accessibility, and is skipped without it.
pub fn focus_tui_window(tui_pid: u32) -> Result<()> {
    let app = app_ancestor(tui_pid, parent_of)
        .with_context(|| format!("no app owns the nebula TUI (pid {tui_pid})"))?;
    // Every statement filters by unix id again: a stored process reference
    // resolves by name, which is the first instance of the same app.
    let script = format!(
        r#"tell application "System Events"
    try
        set value of attribute "AXMinimized" of every window of (first process whose unix id is {app}) to false
    end try
    set frontmost of (first process whose unix id is {app}) to true
end tell"#
    );
    let out = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .context("run osascript")?;
    if !out.status.success() {
        bail!("osascript: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// `pid`'s parent and executable path, from `ps`.
fn parent_of(pid: u32) -> Option<(u32, String)> {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (ppid, exe) = text.trim().split_once(char::is_whitespace)?;
    Some((ppid.parse().ok()?, exe.trim().to_owned()))
}

/// The nearest ancestor of `pid` that runs from an app bundle.
fn app_ancestor(pid: u32, parent_of: impl Fn(u32) -> Option<(u32, String)>) -> Option<u32> {
    let mut current = pid;
    for _ in 0..32 {
        let (parent, _) = parent_of(current)?;
        if parent <= 1 {
            return None;
        }
        let (_, exe) = parent_of(parent)?;
        if exe.contains(".app/Contents/MacOS/") {
            return Some(parent);
        }
        current = parent;
    }
    None
}

/// Write-then-rename: the reader never sees half a file.
fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_to_focus_is_the_tuis_nearest_app_ancestor() {
        let tree = |pid: u32| match pid {
            29810 => Some((29808, "nebula".to_owned())),
            29808 => Some((29805, "/usr/bin/login".to_owned())),
            29805 => Some((
                1,
                "/Applications/Ghostty.app/Contents/MacOS/ghostty".to_owned(),
            )),
            1 => Some((0, "/sbin/launchd".to_owned())),
            _ => None,
        };
        assert_eq!(app_ancestor(29810, tree), Some(29805));
        assert_eq!(app_ancestor(29808, tree), Some(29805));
        assert_eq!(app_ancestor(1, tree), None);
        assert_eq!(app_ancestor(4242, tree), None);
    }
}
