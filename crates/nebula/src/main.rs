mod ssh;
mod upgrade;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "nebula",
    version,
    about = "Terminal multiplexer for Claude Code agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Directory to add as a project — shorthand for `nebula add <dir>`.
    /// (A directory whose name collides with a subcommand needs the long
    /// form or a `./` prefix.)
    dir: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Add a directory as a project, named after the repo's root directory
    /// (`nebula add .` for the current one; bare `nebula <dir>` works too).
    Add {
        /// Path to a git repository (default: the current directory).
        #[arg(default_value = ".")]
        path: String,
    },
    /// Open the TUI landed straight on the project owning a directory
    /// (default: the current one) — the repo is added as a project first
    /// when nebula doesn't know it yet. The `ng` launcher uses this.
    Open {
        /// Directory to land on (default: the current directory).
        #[arg(default_value = ".")]
        path: String,
    },
    /// Run the daemon process (normally auto-spawned by the TUI).
    Daemon {
        /// Stay attached to the terminal instead of logging to file.
        #[arg(long)]
        foreground: bool,
    },
    /// Ask a running daemon to shut down cleanly.
    Kill,
    /// Title this session (run from inside a nebula agent session; agents
    /// use it to auto-title on the first prompt).
    Rename {
        /// The new title; multiple words need no quotes.
        #[arg(required = true, num_args = 1..)]
        title: Vec<String>,
        /// Replace an existing title instead of only filling in a missing one.
        #[arg(long)]
        force: bool,
    },
    /// Manage workspaces — named project groups; one is open at a time and
    /// the TUI scopes its project list (and `/` search) to it.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Create worktrees from the command line — the orchestration surface
    /// agents use from inside their sessions (any shell works too).
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    /// Spawn and inspect agent sessions from the command line — how an
    /// orchestrator manages its workers.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Read or edit the notes for the current project/worktree (agents can
    /// too — inside a session the target is the session's own project).
    Notes {
        #[command(subcommand)]
        command: Option<NotesCommand>,
    },
    /// Read or edit the todos for the current project/worktree — a task
    /// list separate from notes, each todo holding its own notes (agents
    /// can too — inside a session the target is the session's own project).
    Todo {
        #[command(subcommand)]
        command: Option<TodoCommand>,
    },
    /// Open nebula on a remote host over ssh (installs it there if missing).
    Ssh {
        /// ssh destination, passed verbatim (e.g. user@server).
        host: String,
        /// Remote directory to start in (default: remote $HOME).
        path: Option<String>,
    },
    /// Install the latest published nebula over this one.
    Upgrade {
        /// Upgrade even when running from a local cargo build.
        #[arg(long)]
        force: bool,
    },
    /// Phase-2 debug client: raw passthrough to a scratch session (Ctrl+\ detaches).
    #[command(hide = true, name = "_raw-attach")]
    RawAttach {
        #[arg(default_value = "0")]
        name: String,
    },
    /// Installer hook: print the cutover note only when a live daemon is on
    /// a different build than this binary (see `make install` / install.sh).
    #[command(hide = true, name = "_stale-daemon-note")]
    StaleDaemonNote,
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Create a worktree in a project ("fix login flow" → fix-login-flow).
    /// Inside a nebula session the project is the caller's own; outside,
    /// pass --project.
    New {
        /// Worktree/branch name; spaces become hyphens.
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
        /// Base branch or commit to branch from (default: the root HEAD).
        #[arg(long)]
        from: Option<String>,
        /// Project name (default: the calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Spawn an agent session. `--orchestrator` puts it on the project's
    /// root checkout, pinned, in the orchestrators group; workers need
    /// --worktree <branch>. `--prompt` hands it its first task.
    New {
        /// Worktree to spawn in: a branch name, a directory name, or
        /// "root" (required unless --orchestrator).
        #[arg(long)]
        worktree: Option<String>,
        /// Project name (default: the calling session's project).
        #[arg(long)]
        project: Option<String>,
        /// claude | codex | cursor | pi.
        #[arg(long, default_value = "claude")]
        kind: String,
        /// Model the CLI launches with (default: the CLI's own).
        #[arg(long)]
        model: Option<String>,
        /// Reasoning effort the CLI launches with (default: the CLI's own).
        #[arg(long)]
        effort: Option<String>,
        /// Session name; multiple words need no quotes (default: derived
        /// from --prompt, else generated; the session then titles itself).
        #[arg(long, num_args = 1..)]
        name: Option<Vec<String>>,
        /// Project-level orchestrator: root checkout, pinned, own group.
        #[arg(long)]
        orchestrator: bool,
        /// Initial task, submitted as the CLI's first prompt.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Promote a root-checkout session to project orchestrator (it moves
    /// into the ORCHESTRATORS section, pinned).
    Promote {
        /// Session name (as shown in the panels).
        name: String,
        /// Project name (default: the calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Demote an orchestrator back to a plain session.
    Demote {
        /// Session name (as shown in the panels).
        name: String,
        /// Project name (default: the calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// List agent sessions as JSON (name, kind, status, worktree, path).
    List {
        /// Project name (default: the calling session's project).
        #[arg(long)]
        project: Option<String>,
        /// Every project, not just one.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Subcommand)]
enum NotesCommand {
    /// List the project's notes, then the current worktree's (the default).
    List,
    /// Add a note to the project's list (--worktree for the checkout's own).
    Add {
        /// The note; multiple words need no quotes.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Add to the current worktree's list instead of the project's.
        #[arg(long)]
        worktree: bool,
    },
    /// Check a note off, by its number in `nebula notes`.
    Done { index: usize },
}

#[derive(Subcommand)]
enum TodoCommand {
    /// List the project's todos, then the current worktree's (the default).
    List,
    /// Add a todo to the project's list (--worktree for the checkout's own).
    Add {
        /// The todo; multiple words need no quotes.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Add to the current worktree's list instead of the project's.
        #[arg(long)]
        worktree: bool,
    },
    /// Check a todo off, by its number in `nebula todo list`.
    Done { index: usize },
    /// Bring a checked-off todo back.
    Reopen { index: usize },
    /// One todo with its notes, by its number in `nebula todo list`.
    Show { index: usize },
    /// Add a note under a todo.
    Note {
        /// The todo's number in `nebula todo list`.
        index: usize,
        /// The note; multiple words need no quotes.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
    },
    /// Check a todo's note off (numbers from `nebula todo show <n>`).
    NoteDone {
        /// The todo's number in `nebula todo list`.
        index: usize,
        /// The note's number in `nebula todo show <index>`.
        note: usize,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    /// Create a workspace (does not open it).
    Add { name: String },
    /// Open a workspace: projects (and the TUI, live) scope to it.
    Open { name: String },
    /// List workspaces; `*` marks the open one.
    List,
    /// Delete an empty workspace.
    Delete { name: String },
    /// Rename a workspace.
    Rename { name: String, new_name: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Daemon { foreground }) => {
            init_daemon_logging(foreground)?;
            log_fatal(
                nebula_daemon::run_daemon(),
                nebula_core::paths::daemon_log_path(),
            )
        }
        Some(Command::Add { path }) => nebula_tui::run_add_project(path),
        Some(Command::Open { path }) => {
            // Absolute before the TUI starts: the daemon and the panels
            // both need a path that survives any later cwd changes.
            let dir = std::fs::canonicalize(&path)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(&path));
            run_tui_and_handoff(Some(dir))
        }
        Some(Command::Workspace { command }) => {
            use nebula_tui::WorkspaceOp;
            let op = match command {
                WorkspaceCommand::Add { name } => WorkspaceOp::Add { name },
                WorkspaceCommand::Open { name } => WorkspaceOp::Open { name },
                WorkspaceCommand::List => WorkspaceOp::List,
                WorkspaceCommand::Delete { name } => WorkspaceOp::Delete { name },
                WorkspaceCommand::Rename { name, new_name } => {
                    WorkspaceOp::Rename { name, new_name }
                }
            };
            nebula_tui::run_workspace(op)
        }
        Some(Command::Worktree { command }) => match command {
            WorktreeCommand::New {
                name,
                from,
                project,
            } => nebula_tui::run_worktree_new(name.join(" "), from, project),
        },
        Some(Command::Agent { command }) => match command {
            AgentCommand::New {
                worktree,
                project,
                kind,
                model,
                effort,
                name,
                orchestrator,
                prompt,
            } => nebula_tui::run_agent_new(nebula_tui::NewAgentOpts {
                worktree,
                project,
                kind,
                model,
                effort,
                name: name.map(|words| words.join(" ")),
                orchestrator,
                prompt,
            }),
            AgentCommand::Promote { name, project } => {
                nebula_tui::run_agent_set_orchestrator(name, project, true)
            }
            AgentCommand::Demote { name, project } => {
                nebula_tui::run_agent_set_orchestrator(name, project, false)
            }
            AgentCommand::List { project, all } => nebula_tui::run_agent_list(project, all),
        },
        Some(Command::Notes { command }) => {
            use nebula_tui::NotesOp;
            let op = match command {
                None | Some(NotesCommand::List) => NotesOp::List,
                Some(NotesCommand::Add { text, worktree }) => NotesOp::Add {
                    text: text.join(" "),
                    worktree,
                },
                Some(NotesCommand::Done { index }) => NotesOp::Done { index },
            };
            nebula_tui::run_notes(op)
        }
        Some(Command::Todo { command }) => {
            use nebula_tui::TodoOp;
            let op = match command {
                None | Some(TodoCommand::List) => TodoOp::List,
                Some(TodoCommand::Add { text, worktree }) => TodoOp::Add {
                    text: text.join(" "),
                    worktree,
                },
                Some(TodoCommand::Done { index }) => TodoOp::Done { index },
                Some(TodoCommand::Reopen { index }) => TodoOp::Reopen { index },
                Some(TodoCommand::Show { index }) => TodoOp::Show { index },
                Some(TodoCommand::Note { index, text }) => TodoOp::NoteAdd {
                    index,
                    text: text.join(" "),
                },
                Some(TodoCommand::NoteDone { index, note }) => TodoOp::NoteDone { index, note },
            };
            nebula_tui::run_todo(op)
        }
        Some(Command::Kill) => nebula_tui::run_kill(),
        Some(Command::Rename { title, force }) => nebula_tui::run_rename(title.join(" "), force),
        Some(Command::Ssh { host, path }) => ssh::run_ssh(&host, path.as_deref()),
        Some(Command::Upgrade { force }) => upgrade::run_upgrade(force),
        Some(Command::StaleDaemonNote) => {
            if nebula_daemon::lifecycle::daemon_is_stale() {
                println!("note: the running daemon was built from older code.");
                println!(
                    "      run 'nebula kill' to restart onto the new binary (stops ALL sessions)."
                );
            }
            Ok(())
        }
        Some(Command::RawAttach { name }) => nebula_tui::run_raw_attach(&name),
        None => match cli.dir {
            Some(dir) => nebula_tui::run_add_project(dir),
            None => run_tui_and_handoff(None),
        },
    }
}

/// Launch the TUI (optionally landing on `open_at`'s project) and honor a
/// hosts-picker handoff: the TUI quit and restored the terminal so a fresh
/// `nebula ssh` can exec over us (the local daemon and its sessions stay up).
fn run_tui_and_handoff(open_at: Option<std::path::PathBuf>) -> Result<()> {
    init_tui_logging()?;
    let handoff = log_fatal(
        nebula_tui::run_tui(open_at),
        nebula_core::paths::tui_log_path(),
    )?;
    match handoff {
        Some(entry) => {
            eprintln!("nebula: connecting to {}…", entry.host);
            ssh::run_ssh(&entry.host, entry.path.as_deref())
        }
        None => Ok(()),
    }
}

/// Record a fatal top-level error in the log file before it goes to stderr —
/// the TUI's stderr disappears with the terminal, the daemon's is /dev/null.
fn log_fatal<T>(result: Result<T>, log_path: std::path::PathBuf) -> Result<T> {
    if let Err(err) = &result {
        nebula_core::crashlog::append(&log_path, &format!("FATAL {err:#}"));
    }
    result
}

fn init_daemon_logging(foreground: bool) -> Result<()> {
    // The daemon runs detached with stderr on /dev/null — without this hook a
    // panic (on any thread, tokio workers included) leaves no trace.
    nebula_core::crashlog::install_panic_hook(nebula_core::paths::daemon_log_path());
    let filter = tracing_subscriber::EnvFilter::try_from_env("NEBULA_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if foreground {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    } else {
        std::fs::create_dir_all(nebula_core::paths::log_dir())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(nebula_core::paths::daemon_log_path())?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(file)
            .with_ansi(false)
            .init();
    }
    Ok(())
}

fn init_tui_logging() -> Result<()> {
    // Panic output to stderr dies with the alternate screen — capture it to
    // the log file. The TUI later wraps this hook with its terminal-restore,
    // so the chain on panic is: restore terminal → log to file → stderr.
    nebula_core::crashlog::install_panic_hook(nebula_core::paths::tui_log_path());
    // stdout belongs to the UI — log to file only.
    std::fs::create_dir_all(nebula_core::paths::log_dir())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(nebula_core::paths::tui_log_path())?;
    let filter = tracing_subscriber::EnvFilter::try_from_env("NEBULA_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();
    Ok(())
}
