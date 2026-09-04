//! `nebula remote <host> …`: the host-side view of remote projects.
//!
//! Sessions on a remote host are owned by *this* daemon (their PTY is an
//! ssh client here; the far side only has the CLI process), so the local
//! snapshot is the source of truth for "what runs on findl". What the
//! snapshot can't see — nebula's version there, the daemon a `nebula ssh`
//! visit left running, agent processes that outlived their tunnel — this
//! module asks the host over ssh. Every ssh hop is BatchMode: a host that
//! needs a password fails fast instead of hanging a CLI.
//!
//! Ownership is decided by env, not by process tree: a remote spawn exports
//! `NEBULA_AGENT_ID` and `NEBULA_REMOTE=1` to its CLI, so `/proc/<pid>/environ`
//! says which local session a process belongs to. Processes without that
//! marker were started by the host's own daemon or by hand — `clean` never
//! touches them.

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
    Clean,
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
        RemoteOp::Clean => clean(&host).await,
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

/// Agent CLI processes on the host: (pid, cli, NEBULA_AGENT_ID or "").
/// Linux `/proc` only — which is what a nebula server is.
fn remote_agent_procs(host: &str) -> Result<Vec<(u32, String, String)>> {
    let script = r#"for p in $(pgrep -x -d ' ' claude codex cursor-agent pi 2>/dev/null); do
  c=$(ps -o comm= -p $p 2>/dev/null); id=$(tr '\0' '\n' < /proc/$p/environ 2>/dev/null | sed -n 's/^NEBULA_AGENT_ID=//p');
  echo "$p $c $id"; done"#;
    let out = ssh(host, script, false)?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let cli = it.next()?.to_string();
            let id = it.next().unwrap_or("").to_string();
            Some((pid, cli, id))
        })
        .collect())
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
    // Processes the tunnel left behind.
    let live: Vec<String> = sessions_on(&snap, host)
        .iter()
        .filter(|(a, ..)| a.alive)
        .map(|(a, ..)| a.id.to_string())
        .collect();
    if let Ok(procs) = remote_agent_procs(host) {
        let orphans: Vec<_> = procs
            .iter()
            .filter(|(_, _, id)| !id.is_empty() && !live.contains(id))
            .collect();
        let foreign = procs.iter().filter(|(_, _, id)| id.is_empty()).count();
        println!(
            "processes on {host}: {} agent CLI{} ({} orphaned from here, {} not ours)",
            procs.len(),
            if procs.len() == 1 { "" } else { "s" },
            orphans.len(),
            foreign
        );
        if !orphans.is_empty() {
            println!("  `nebula remote {host} clean` kills the orphans");
        }
    }
    Ok(())
}

async fn print_sessions(host: &str, snap: &Snapshot, include_server_owned: bool) -> Result<()> {
    let all = sessions_on(snap, host);
    // `status` is the glance: live rows only, archived as a count.
    // `sessions` is the ledger and shows everything.
    let archived = all.iter().filter(|(a, ..)| a.archived).count();
    let ours: Vec<_> = all
        .iter()
        .filter(|(a, ..)| include_server_owned || !a.archived)
        .collect();
    println!(
        "sessions on {host} (owned by this daemon): {}{}",
        ours.len(),
        if !include_server_owned && archived > 0 {
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
            if a.archived { "  (archived)" } else if !a.alive { "  (no pty)" } else { "" }
        );
    }
    if include_server_owned {
        // What the host's own daemon runs — sessions started there with
        // `nebula ssh` or by hand. Invisible to our tree, listed for the
        // full picture.
        match ssh(host, "nebula agent list --all 2>/dev/null", false) {
            Ok(out) if !out.trim().is_empty() => {
                let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap_or_default();
                let live: Vec<&serde_json::Value> = rows
                    .iter()
                    .filter(|r| r["archived"].as_bool() != Some(true))
                    .collect();
                println!("sessions owned by {host}'s own daemon: {}", live.len());
                for r in live {
                    println!(
                        "  {:<12} {:<7} {:<28} {}/{}",
                        r["status"].as_str().unwrap_or("?"),
                        r["kind"].as_str().unwrap_or("?"),
                        truncate(r["name"].as_str().unwrap_or("?"), 28),
                        r["project"].as_str().unwrap_or("?"),
                        r["worktree"].as_str().unwrap_or("?")
                    );
                }
            }
            _ => println!("sessions owned by {host}'s own daemon: none (or its daemon is down)"),
        }
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
    // Interactive on purpose: nebula upgrade prints progress and kills the
    // host's daemon (its own sessions, not ours — ours ride ssh from here).
    ssh(host, "nebula kill 2>/dev/null; nebula upgrade && nebula --version", true)?;
    Ok(())
}

/// Kill agent CLI processes on the host that carry a NEBULA_AGENT_ID this
/// daemon no longer has a live PTY for — a tunnel that dropped without
/// taking its CLI down. Processes without the marker are left alone.
async fn clean(host: &str) -> Result<()> {
    let snap = snapshot().await?;
    let live: Vec<String> = sessions_on(&snap, host)
        .iter()
        .filter(|(a, ..)| a.alive)
        .map(|(a, ..)| a.id.to_string())
        .collect();
    let orphans: Vec<(u32, String, String)> = remote_agent_procs(host)?
        .into_iter()
        .filter(|(_, _, id)| !id.is_empty() && !live.contains(id))
        .collect();
    if orphans.is_empty() {
        println!("nothing to clean on {host}");
        return Ok(());
    }
    let pids: Vec<String> = orphans.iter().map(|(p, ..)| p.to_string()).collect();
    ssh(host, &format!("kill {} 2>/dev/null; sleep 1; kill -9 {} 2>/dev/null; true", pids.join(" "), pids.join(" ")), false)?;
    for (pid, cli, id) in &orphans {
        println!("killed {cli} pid {pid} (session {id})");
    }
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

