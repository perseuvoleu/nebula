#![allow(
    clippy::disallowed_methods,
    reason = "clippy.toml bans sleeps and wall clock in integration tests only; product code waits and times by design"
)]

pub mod app;
pub mod artifact_cli;
pub mod board_cli;
pub mod branch_name;
pub mod branches;
pub mod collect_cli;
pub mod completion;
pub mod config;
pub mod decision_cli;
pub mod diagram_cli;
pub mod diagrams;
pub mod event_loop;
pub mod file_view;
pub mod fuzzy;
#[cfg(feature = "ghostty")]
pub mod ghostty_pane;
pub mod git_diff;
pub mod grep_search;
pub mod hosts;
mod html_viewer;
pub mod ipc;
pub mod keymap;
pub mod keys;
pub mod links;
pub mod pull_request;
pub mod raw_attach;
pub mod remote;
pub mod remote_cli;
pub mod review;
pub mod review_queue;
pub mod session_open;
pub mod splash;
pub mod sync_cli;
pub mod syntax;
pub mod text_input;
pub mod theme;
pub mod transcript;
pub mod tree_browser;
pub mod ui;
pub mod vim_term;

use anyhow::Result;

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?)
}

/// Entry point for the TUI client. Terminal setup/teardown lives here so the
/// binary crate stays a thin arg-parser. `Some(entry)` means the user picked
/// a recent ssh host — the terminal is restored and the caller should exec
/// `nebula ssh` at it.
pub fn run_tui(open_at: Option<app::OpenAt>) -> Result<event_loop::Handoff> {
    runtime()?.block_on(event_loop::run_app(open_at))
}

/// `nebula open --session`: resolve the session, land it in a running TUI
/// and (unless `focus` is off) bring that TUI's window to the front,
/// printing what landed as one JSON object. `Some(id)` when no TUI
/// answered — the caller starts one landed on `id` when it has a terminal.
pub fn run_open_session(
    reference: String,
    project: Option<String>,
    focus: bool,
) -> Result<Option<nebula_core::AgentId>> {
    let resolved = runtime()?.block_on(session_open::resolve(&reference, project.as_deref()))?;
    let Some(pid) = session_open::hand_to_running_tui(&resolved.id)? else {
        return Ok(Some(resolved.id));
    };
    // The attach is confirmed by now; a window that won't come forward
    // is reported, not a failure.
    let focused = focus
        && match session_open::focus_tui_window(pid) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("nebula: attached, but could not focus its window: {error:#}");
                false
            }
        };
    println!(
        "{}",
        serde_json::json!({
            "landed": "tui",
            "tui_pid": pid,
            "id": resolved.id.as_str(),
            "name": resolved.name,
            "project": resolved.project,
            "host": resolved.host,
            "focused": focused,
        })
    );
    Ok(None)
}

/// `nebula open --session --on HOST`: the same open run on HOST over ssh,
/// its object passed through. The exit code to leave with: 3 when HOST is
/// unreachable, else the remote command's.
pub fn run_open_on_host(
    host: &str,
    reference: &str,
    project: Option<&str>,
    focus: bool,
) -> Result<i32> {
    run_on_host(
        host,
        session_open::open_on_argv(host, reference, project, focus)?,
    )
}

/// Runs an ssh argv from [`session_open::ssh_argv`] and passes the remote
/// command's output through. The exit code to leave with: 3 when HOST is
/// unreachable, else the remote command's.
pub(crate) fn run_on_host(host: &str, argv: Vec<String>) -> Result<i32> {
    use anyhow::Context;
    use std::io::Write;
    let out = std::process::Command::new("ssh")
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .output()
        .context("run ssh")?;
    // ssh's own failures (connect, auth) exit 255; nebula never does.
    if out.status.code() == Some(255) {
        eprintln!(
            "nebula: host {host} is unreachable: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return Ok(3);
    }
    std::io::stdout().write_all(&out.stdout)?;
    std::io::stdout().flush()?;
    std::io::stderr().write_all(&out.stderr)?;
    Ok(out.status.code().unwrap_or(1))
}

/// Phase-2 throwaway raw-mode client (`nebula _raw-attach`).
pub fn run_raw_attach(name: &str) -> Result<()> {
    runtime()?.block_on(raw_attach::run(name))
}

/// Post-upgrade daemon handoff: shut the daemon down only when it holds no
/// live sessions (see `ipc::shutdown_if_idle`).
pub fn shutdown_daemon_if_idle() -> Result<ipc::IdleShutdown> {
    runtime()?.block_on(ipc::shutdown_if_idle())
}

/// `nebula rename` — agent-side session titling (see `ipc::rename_current_agent`).
pub fn run_rename(title: String, force: bool) -> Result<()> {
    runtime()?.block_on(ipc::rename_current_agent(title, force))
}

/// `nebula add <dir>` / bare `nebula <dir>` — register a directory as a
/// project (see `ipc::add_project`).
pub fn run_add_project(path: String) -> Result<()> {
    runtime()?.block_on(ipc::add_project(path))
}

pub use ipc::{NewAgentOpts, NotesOp, TodoOp, WorkspaceOp};

/// `nebula notes [list|add|done]` — project/worktree notes from the CLI,
/// agents included (see `ipc::run_notes`).
pub fn run_notes(op: NotesOp) -> Result<()> {
    runtime()?.block_on(ipc::run_notes(op))
}

/// `nebula todo done|reopen|note-done` — the legacy positional todo verbs
/// (see `ipc::run_todo`).
pub fn run_todo(op: TodoOp) -> Result<()> {
    runtime()?.block_on(ipc::run_todo(op))
}

/// `nebula todo` card verbs on the board (see `board_cli::run`).
pub fn run_board(verb: board_cli::TodoVerb, json: bool) -> Result<()> {
    board_cli::exit_on_refusal(runtime()?.block_on(board_cli::run(verb, json)))
}

/// `nebula artifact`: exits 5 for an unknown artifact or card.
pub fn run_artifact(verb: artifact_cli::ArtifactVerb, json: bool) -> Result<()> {
    board_cli::exit_on_refusal(runtime()?.block_on(artifact_cli::run(verb, json)))
}

/// `nebula decision`: exits 5 for an unknown decision or card.
pub fn run_decision(verb: decision_cli::DecisionVerb, json: bool) -> Result<()> {
    board_cli::exit_on_refusal(runtime()?.block_on(decision_cli::run(verb, json)))
}

/// `nebula worktree new` — agent-facing orchestration verb.
pub fn run_worktree_new(name: String, from: Option<String>, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::worktree_new(name, from, project))
}

/// Check or remove a clean, merged, session-free checkout by ID while keeping its branch.
pub fn run_worktree_delete_safe(
    id: String,
    branch: String,
    path: std::path::PathBuf,
    check: bool,
    with_archived: bool,
    project: Option<String>,
) -> Result<()> {
    runtime()?.block_on(ipc::worktree_delete_safe(
        id,
        branch,
        path,
        check,
        with_archived,
        project,
    ))
}

/// `nebula worktree prune` — drop rows whose checkout and git entry are gone.
pub fn run_worktree_prune(check: bool, with_archived: bool, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::worktree_prune(check, with_archived, project))
}

/// `nebula worktree delete` — the daemon's delete flow (checkout + row).
pub fn run_worktree_delete(name: String, force: bool, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::worktree_delete(name, force, project))
}

/// `nebula worktree checkout` — a branch onto the primary checkout.
pub fn run_worktree_checkout(
    branch: String,
    project: Option<String>,
    stop_busy: bool,
) -> Result<()> {
    runtime()?.block_on(ipc::worktree_checkout(branch, project, stop_busy))
}

/// `nebula switch` — a branch's checkout (made if missing), and the calling
/// shell tab `cd`-ed into it.
pub fn run_switch(
    branch: String,
    from: Option<String>,
    worktree: bool,
    project: Option<String>,
    stop_busy: bool,
) -> Result<()> {
    runtime()?.block_on(ipc::switch(branch, from, worktree, project, stop_busy))
}

/// `nebula agent new` — spawn a session. Exits 2 when another live session
/// is the checkout's writer and `--writer` was passed without `--force-writer`.
pub fn run_agent_new(opts: NewAgentOpts) -> Result<()> {
    match runtime()?.block_on(ipc::agent_new(opts)) {
        Err(why)
            if nebula_core::board::Refusal::of(&why.to_string())
                == Some(nebula_core::board::Refusal::WriterHeld) =>
        {
            eprintln!("{why:#}");
            std::process::exit(2);
        }
        done => board_cli::exit_on_refusal(done),
    }
}

/// `nebula worktree list` — JSON map of the checkouts and who lives on them.
pub fn run_worktree_list(project: Option<String>, all: bool) -> Result<()> {
    runtime()?.block_on(ipc::worktree_list(project, all))
}

/// Best-effort preview bookmark used after a successful host-local promotion.
pub fn pin_checkout_link(checkout: &std::path::Path, url: &str) -> Result<()> {
    runtime()?.block_on(ipc::pin_checkout_link(checkout, url))
}

/// `nebula agent list` — the delegating session's JSON view of its workers.
/// `nebula remote <host> …` — the host-side view of remote projects.
pub fn run_remote(host: String, op: remote_cli::RemoteOp) -> Result<()> {
    runtime()?.block_on(remote_cli::run(host, op))
}

pub use remote_cli::RemoteOp;

/// `nebula sync` — one project, both machines: report, move commits the
/// safe way through origin, carry `.env*` / `.scratch/` / skills.
pub fn run_sync(opts: sync_cli::SyncOpts) -> Result<()> {
    runtime()?.block_on(sync_cli::run(opts))
}

pub use sync_cli::{Direction as SyncDirection, SyncOpts};

/// `nebula proxy` — stdin/stdout ↔ the local daemon socket (see `ipc::proxy`).
pub fn run_proxy(connect_only: bool) -> Result<()> {
    let rt = runtime()?;
    let done = rt.block_on(ipc::proxy(connect_only));
    rt.shutdown_background();
    done
}

/// `nebula diagram …` — the checkout's `.scratch/diagrams/` folder: create,
/// import, list/search, remove, and ask a running TUI to show one.
pub fn run_diagram(op: diagram_cli::DiagramOp) -> Result<()> {
    // Only `new` records an owner, so only it looks the caller up.
    let owner = match &op {
        diagram_cli::DiagramOp::New { .. } => runtime()?
            .block_on(ipc::calling_session())
            .map(|(id, name)| diagrams::Meta {
                tags: Vec::new(),
                session: name,
                agent: Some(id),
            })
            .unwrap_or_default(),
        _ => Default::default(),
    };
    diagram_cli::run(op, owner)
}

pub use diagram_cli::DiagramOp;

pub fn run_agent_list(project: Option<String>, all: bool, worktree: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::agent_list(project, all, worktree))
}

/// `nebula agent show` — one session's full JSON row.
pub fn run_agent_show(name: String, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::agent_show(name, project))
}

/// `nebula agent read` — a worker's screen (and scrollback tail) as text.
pub fn run_agent_read(name: String, lines: Option<usize>, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::agent_read(name, lines, project))
}

/// `nebula agent send` — submit a follow-up prompt to a running worker, or
/// with `file` a pointer to it. Exits 2, sending nothing, for a missing file
/// or text that would go as a paste without `paste`.
pub fn run_agent_send(
    name: String,
    text: String,
    file: Option<std::path::PathBuf>,
    paste: bool,
    project: Option<String>,
) -> Result<()> {
    let text = match file {
        Some(path) => {
            ipc::file_pointer(&path, ipc::project_flag_host(project.as_deref()).is_some())
        }
        None => Ok(text),
    };
    match text.and_then(|text| ipc::send_payload(&text, paste).map(|payload| (text, payload))) {
        Ok((text, payload)) => runtime()?.block_on(ipc::agent_send(name, text, payload, project)),
        Err(why) => {
            eprintln!("{why}");
            std::process::exit(2);
        }
    }
}

pub use ipc::AgentCtl;

/// Act on an agent ID: Some(key) writes one key; None explicitly deletes it.
pub fn run_agent_action_id(id: String, key: Option<String>, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::agent_action_id(id, key, project))
}

/// `nebula agent archive|unarchive|delete|restart` — lifecycle verbs.
pub fn run_agent_ctl(op: AgentCtl, name: String, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::agent_ctl(op, name, project))
}

/// `nebula agent wait` — block until workers settle out of running. Exits 3
/// when a message sent to the calling session cut the wait short.
pub fn run_agent_wait(
    names: Vec<String>,
    any: bool,
    timeout: u64,
    project: Option<String>,
) -> Result<()> {
    if let ipc::WaitEnd::Message =
        runtime()?.block_on(ipc::agent_wait(names, any, timeout, project))?
    {
        eprintln!("a message is queued for this session; wait ended early");
        std::process::exit(3);
    }
    Ok(())
}

/// `nebula workspace <add|open|list|delete|rename>` (see `ipc::run_workspace_op`).
pub fn run_workspace(op: WorkspaceOp) -> Result<()> {
    runtime()?.block_on(ipc::run_workspace_op(op))
}

/// `nebula kill`.
pub fn run_kill() -> Result<()> {
    runtime()?.block_on(async {
        if ipc::kill_daemon().await? {
            println!("nebula daemon shut down");
        } else {
            println!("no nebula daemon running");
        }
        Ok(())
    })
}

/// Collect a remote branch or worktree into a safe local destination.
pub fn run_collect(opts: collect_cli::CollectOpts) -> Result<()> {
    runtime()?.block_on(collect_cli::run(opts))
}
