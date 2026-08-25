pub mod app;
pub mod branch_name;
pub mod branches;
pub mod completion;
pub mod config;
pub mod event_loop;
pub mod fuzzy;
pub mod git_diff;
pub mod grep_search;
pub mod hosts;
pub mod ipc;
pub mod keymap;
pub mod keys;
pub mod links;
pub mod pull_request;
pub mod raw_attach;
pub mod remote;
pub mod review;
pub mod splash;
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
pub fn run_tui(open_at: Option<std::path::PathBuf>) -> Result<event_loop::Handoff> {
    runtime()?.block_on(event_loop::run_app(open_at))
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

/// `nebula todo [list|add|done|reopen|show|note|note-done]` —
/// project/worktree todos and their child notes from the CLI, agents
/// included (see `ipc::run_todo`).
pub fn run_todo(op: TodoOp) -> Result<()> {
    runtime()?.block_on(ipc::run_todo(op))
}

/// `nebula worktree new` — agent-facing orchestration verb.
pub fn run_worktree_new(name: String, from: Option<String>, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::worktree_new(name, from, project))
}

/// `nebula worktree delete` — the daemon's delete flow (checkout + row).
pub fn run_worktree_delete(name: String, force: bool, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::worktree_delete(name, force, project))
}

/// `nebula agent new` — spawn a session (orchestrators included).
pub fn run_agent_new(opts: NewAgentOpts) -> Result<()> {
    runtime()?.block_on(ipc::agent_new(opts))
}

/// `nebula agent list` — the orchestrator's JSON view of its workers.
pub fn run_agent_list(project: Option<String>, all: bool) -> Result<()> {
    runtime()?.block_on(ipc::agent_list(project, all))
}

/// `nebula agent wait` — block until workers settle out of running.
pub fn run_agent_wait(names: Vec<String>, timeout: u64, project: Option<String>) -> Result<()> {
    runtime()?.block_on(ipc::agent_wait(names, timeout, project))
}

/// `nebula agent promote|demote <name>` — flip a session's orchestrator role.
pub fn run_agent_set_orchestrator(
    name: String,
    project: Option<String>,
    orchestrator: bool,
) -> Result<()> {
    runtime()?.block_on(ipc::agent_set_orchestrator(name, project, orchestrator))
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
