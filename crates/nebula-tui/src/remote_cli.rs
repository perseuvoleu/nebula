//! `nebula remote <host> …`: the host-side view of remote projects.
//!
//! Sessions on a remote host are owned by the host's own daemon and
//! mirrored here by the relay (see the daemon's `relay.rs`), so the local
//! snapshot already says what runs on findl. What it can't say — nebula's
//! version there, whether the host daemon is up — this module asks over
//! ssh. Every ssh hop is BatchMode: a host that needs a password fails fast
//! instead of hanging a CLI.

use crate::ipc::{handshake, subscribe_snapshot, try_connect};
use anyhow::{bail, Context, Result};
use nebula_core::{paths, Agent, Project, Worktree};
use std::process::Command;

pub enum RemoteOp {
    Status,
    Sessions,
    /// Every ~2s until Ctrl-C.
    Watch,
    Sync,
    Upgrade,
    /// `nebula kill` on the host: restarts its daemon, ending every
    /// session there.
    Restart,
}

pub async fn run(host: String, op: RemoteOp) -> Result<()> {
    match op {
        RemoteOp::Status => status(&host).await,
        RemoteOp::Sessions => {
            let snap = snapshot().await?;
            print_sessions(&host, &snap, true).await
        }
        RemoteOp::Watch => watch(&host).await,
        RemoteOp::Sync => sync(&host).await,
        RemoteOp::Upgrade => upgrade(&host),
        RemoteOp::Restart => restart(&host),
    }
}

type Snapshot = (Vec<Project>, Vec<Worktree>, Vec<Agent>);

async fn snapshot() -> Result<Snapshot> {
    let Ok(stream) = try_connect(&paths::socket_path()).await else {
        bail!("no nebula daemon is running");
    };
    let mut conn = handshake(stream).await?;
    subscribe_snapshot(&mut conn).await
}

/// Projects of the snapshot that live on `host`.
fn projects_on<'a>(snap: &'a Snapshot, host: &str) -> Vec<&'a Project> {
    snap.0
        .iter()
        .filter(|p| p.host.as_deref() == Some(host))
        .collect()
}

/// Local-daemon sessions whose checkout is on `host`, with their worktree.
fn sessions_on<'a>(snap: &'a Snapshot, host: &str) -> Vec<(&'a Agent, &'a Worktree, &'a Project)> {
    let projects = projects_on(snap, host);
    snap.2
        .iter()
        .filter_map(|a| {
            let w = snap.1.iter().find(|w| w.id == a.worktree_id)?;
            let p = projects.iter().find(|p| p.id == w.project_id)?;
            Some((a, w, *p))
        })
        .collect()
}

/// `ssh host <script>` with the remote `~/.local/bin` on PATH (where
/// install.sh puts nebula and where the CLIs usually live). Stdout on
/// success; the host's stderr as the error otherwise.
fn ssh(host: &str, script: &str, tty: bool) -> Result<String> {
    let mut cmd = Command::new("ssh");
    if tty {
        cmd.arg("-t");
    }
    cmd.args(nebula_core::remote::SSH_BATCH_OPTS);
    cmd.args(["--", host]);
    cmd.arg(format!(
        "export PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:$PATH\"; {script}"
    ));
    if tty {
        // The remote command draws on our terminal (nebula upgrade's
        // progress); nothing to capture.
        let status = cmd.status().context("run ssh")?;
        if !status.success() {
            bail!("ssh {host} exited with {status}");
        }
        return Ok(String::new());
    }
    let out = cmd.output().context("run ssh")?;
    if !out.status.success() {
        bail!(
            "ssh {host}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn status(host: &str) -> Result<()> {
    let snap = snapshot().await?;
    let projects = projects_on(&snap, host);
    if projects.is_empty() {
        println!("no remote projects on {host} (add one: nebula add {host}:/path)");
    }
    for p in &projects {
        let n = snap.1.iter().filter(|w| w.project_id == p.id).count();
        println!("project {} @{host}  {}  ({n} checkout{})", p.name, p.repo_path.display(), if n == 1 { "" } else { "s" });
    }
    // The host itself.
    match ssh(host, "nebula --version 2>/dev/null || echo 'nebula: not installed'; nebula agent list --all >/dev/null 2>&1 && echo 'daemon: running' || echo 'daemon: not running'", false) {
        Ok(out) => {
            for l in out.lines() {
                println!("{host}: {l}");
            }
        }
        Err(e) => println!("{host}: unreachable — {e}"),
    }
    print_sessions(host, &snap, false).await?;
    Ok(())
}

async fn print_sessions(host: &str, snap: &Snapshot, everything: bool) -> Result<()> {
    let all = sessions_on(snap, host);
    // `status` is the glance: live rows only, archived as a count.
    // `sessions` is the ledger and shows everything.
    let archived = all.iter().filter(|(a, ..)| a.archived).count();
    let ours: Vec<_> = all
        .iter()
        .filter(|(a, ..)| everything || !a.archived)
        .collect();
    println!(
        "sessions on {host}: {}{}",
        ours.len(),
        if !everything && archived > 0 {
            format!("  (+{archived} archived)")
        } else {
            String::new()
        }
    );
    for (a, w, p) in ours {
        println!(
            "  {:<12} {:<7} {:<28} {}/{}{}",
            a.status.as_str(),
            a.kind.as_str(),
            truncate(&a.name, 28),
            p.name,
            w.branch,
            if a.archived { "  (archived)" } else if !a.alive { "  (idle)" } else { "" }
        );
    }
    Ok(())
}

async fn watch(host: &str) -> Result<()> {
    loop {
        let snap = snapshot().await?;
        print!("\x1b[2J\x1b[H");
        println!("nebula remote {host} watch — {}   (Ctrl-C to stop)", chrono_now());
        print_sessions(host, &snap, false).await?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

fn chrono_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}:{:02} UTC", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
}

/// Skills (the same mirror `nss` runs) and a fast-forward pull on every
/// remote checkout of the host's projects, so the server never trails the
/// laptop by a stale branch or a missing skill.
async fn sync(host: &str) -> Result<()> {
    match which("nebula-sync-skills") {
        Some(script) => {
            println!("skills → {host}");
            let status = Command::new(script).arg(host).status().context("run nebula-sync-skills")?;
            if !status.success() {
                println!("  skills sync failed ({status})");
            }
        }
        None => println!("skills: `nebula-sync-skills` not on PATH — see the sync-skills skill; skipped"),
    }
    let snap = snapshot().await?;
    let checkouts: Vec<&Worktree> = {
        let projects = projects_on(&snap, host);
        snap.1
            .iter()
            .filter(|w| projects.iter().any(|p| p.id == w.project_id))
            .collect()
    };
    if checkouts.is_empty() {
        println!("git: no remote checkouts on {host}");
    }
    for w in checkouts {
        let path = w.path.to_string_lossy();
        let script = format!(
            "git -C {p} fetch -q origin 2>&1 && git -C {p} pull -q --ff-only 2>&1 && git -C {p} log --oneline -1",
            p = nebula_core::remote::ssh_quote(&path)
        );
        match ssh(host, &script, false) {
            Ok(out) => println!("git {}: {}", path, out.trim()),
            Err(e) => println!("git {}: {}", path, e),
        }
    }
    Ok(())
}

fn upgrade(host: &str) -> Result<()> {
    // Interactive on purpose: nebula upgrade prints progress. The host's
    // daemon keeps running the old build (and its sessions) until
    // `nebula remote <host> restart`.
    ssh(host, "nebula upgrade && nebula --version", true)?;
    Ok(())
}

/// `nebula kill` on the host. Its daemon owns every session there, so
/// this ends them all — the one remote action with that blast radius.
fn restart(host: &str) -> Result<()> {
    ssh(host, "nebula kill; sleep 1; nebula agent list --all >/dev/null 2>&1 && echo 'daemon: running' || echo 'daemon: stopped (starts on the next relay connect)'", true)?;
    Ok(())
}

fn which(bin: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join(bin))
            .find(|f| f.is_file())
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).collect::<String>() + "…"
    }
}

