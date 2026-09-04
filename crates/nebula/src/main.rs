mod ssh;
mod upgrade;

use anyhow::{Context, Result};
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
    /// Move this shell onto a branch. A branch lives where the project
    /// is: it is checked out in the primary checkout (created first like
    /// `git checkout -b`, from the branch this shell is on) and the shell
    /// `cd`s there. `--worktree` gives it its own directory instead, for
    /// parallel work. A branch that already has a checkout is entered
    /// as-is. Inside a nebula terminal the `cd` is typed into the tab.
    Switch {
        /// Local branch name (created when missing).
        branch: String,
        /// Base for a new branch (default: the branch checked out here).
        #[arg(long)]
        from: Option<String>,
        /// Give the branch its own worktree directory (`(wt)` row) instead
        /// of checking it out in the primary.
        #[arg(long)]
        worktree: bool,
        /// Project name (default: the project owning this shell or cwd).
        #[arg(long)]
        project: Option<String>,
    },
    /// Spawn and inspect agent sessions from the command line — how a
    /// session delegates work to and manages its workers.
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
    /// Install an agent CLI's nebula status hooks into a checkout — what
    /// the daemon does before every spawn; run on the far host by a remote
    /// session's ssh spawn, where this daemon's installer can't reach.
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// The host-side view of remote projects: what runs on a server, its
    /// nebula, orphaned agent processes; sync skills and checkouts; upgrade.
    Remote {
        /// ssh destination (an alias from ~/.ssh/config works).
        host: String,
        #[command(subcommand)]
        command: RemoteCommand,
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
    /// Pipe stdin/stdout to this machine's daemon socket — what a remote
    /// nebula runs over ssh to reach the daemon here.
    #[command(hide = true)]
    Proxy,
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// List worktrees as JSON (branch, path, sessions living on each).
    List {
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
        /// Every project, not just one.
        #[arg(long)]
        all: bool,
    },
    /// Create a worktree in a project ("fix login flow" → fix-login-flow).
    /// Inside a nebula session the project is the caller's own; outside,
    /// pass --project.
    New {
        /// Worktree/branch name; spaces become hyphens.
        #[arg(required = true, num_args = 1..)]
        name: Vec<String>,
        /// Base branch or commit to branch from (default: the primary
        /// checkout's HEAD).
        #[arg(long)]
        from: Option<String>,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Delete a worktree through the daemon so the checkout and nebula's
    /// row go together (raw `git worktree remove` leaves a ghost row).
    /// Sessions living on it are killed.
    Delete {
        /// Branch or directory name of the worktree.
        name: String,
        /// Remove even with uncommitted changes.
        #[arg(long)]
        force: bool,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Check a branch out in the primary checkout. A nebula worktree
    /// holding that branch is removed first (the branch is kept); refused
    /// while sessions still run on it. Never detach a worktree by hand to
    /// free a branch — this is the supported route.
    Checkout {
        /// Local branch name.
        branch: String,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Spawn an agent session on a worktree (--worktree <branch>).
    /// `--prompt` hands it its first task.
    New {
        /// Worktree to spawn in: a branch name, a directory name, or
        /// "root"/"primary" for the primary checkout.
        #[arg(long)]
        worktree: Option<String>,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
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
        /// Initial task, submitted as the CLI's first prompt.
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Block until workers settle out of running (finished, needs_feedback,
    /// terminated…), then print them as JSON — `agent list`'s row shape.
    /// No names means every unarchived worker of the project.
    Wait {
        /// Session names to wait on (as shown in the panels).
        names: Vec<String>,
        /// Give up after this many seconds (nonzero exit).
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// List agent sessions as JSON (name, kind, status, worktree, path).
    List {
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
        /// Every project, not just one.
        #[arg(long)]
        all: bool,
        /// Only sessions on this worktree (branch, directory name, or
        /// "root"/"primary" for the primary checkout).
        #[arg(long)]
        worktree: Option<String>,
    },
    /// One session's full JSON row (status, model, path, timestamps).
    Show {
        /// Session name (as shown in the panels).
        name: String,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Print a worker's screen — scrollback tail included — as plain text.
    /// Rendered by the daemon; nothing is attached, resized, or respawned.
    Read {
        /// Session name (as shown in the panels).
        name: String,
        /// Keep only the last N lines (default: everything retained).
        #[arg(long)]
        lines: Option<usize>,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Type a follow-up prompt into a running worker and submit it — the
    /// steering half of delegation (`new --prompt` only covers the first task).
    Send {
        /// Session name (as shown in the panels).
        name: String,
        /// The prompt; multiple words need no quotes.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Kill a worker's PTY but keep its row (the TUI's ARCHIVED group).
    Archive {
        /// Session name (as shown in the panels).
        name: String,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Bring an archived session back (its PTY respawns on next attach).
    Unarchive {
        /// Session name (as shown in the panels).
        name: String,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Kill a worker's PTY and remove its row entirely.
    Delete {
        /// Session name (as shown in the panels).
        name: String,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Respawn a worker's CLI, resuming its stored session when one exists.
    Restart {
        /// Session name (as shown in the panels).
        name: String,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
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
enum RemoteCommand {
    /// nebula version and daemon on the host, this daemon's sessions there,
    /// leftover agent processes.
    Status,
    /// Sessions on the host: this daemon's, plus the host daemon's own.
    Sessions,
    /// Live session list, refreshed every 2s.
    Watch,
    /// Mirror skills (nebula-sync-skills), fast-forward every remote
    /// checkout, and send the local `.env` files of each project to its
    /// checkout on the host (ones the host already has are kept).
    Sync {
        /// Overwrite `.env` files the host already has with the local ones.
        #[arg(long)]
        force_env: bool,
    },
    /// `nebula upgrade` on the host (its daemon keeps running the old
    /// build until `restart`).
    Upgrade,
    /// `nebula kill` on the host: restart its daemon. Ends every session
    /// there — they are the host daemon's.
    Restart,
}

#[derive(Subcommand)]
enum HooksCommand {
    /// Install `kind`'s hooks (claude, codex, cursor, pi) for `dir`.
    Install {
        kind: String,
        /// The checkout the CLI will run in (default: the current directory).
        #[arg(default_value = ".")]
        dir: String,
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
        Some(Command::Hooks {
            command: HooksCommand::Install { kind, dir },
        }) => {
            let kind = nebula_core::AgentKind::parse(&kind)
                .ok_or_else(|| anyhow::anyhow!("unknown agent kind: {kind}"))?;
            let dir =
                std::fs::canonicalize(&dir).with_context(|| format!("{dir} does not exist"))?;
            nebula_daemon::hooks::installer::install_for_kind(kind, &dir)
        }
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
            WorktreeCommand::List { project, all } => nebula_tui::run_worktree_list(project, all),
            WorktreeCommand::New {
                name,
                from,
                project,
            } => nebula_tui::run_worktree_new(name.join(" "), from, project),
            WorktreeCommand::Delete {
                name,
                force,
                project,
            } => nebula_tui::run_worktree_delete(name, force, project),
            WorktreeCommand::Checkout { branch, project } => {
                nebula_tui::run_worktree_checkout(branch, project)
            }
        },
        Some(Command::Switch {
            branch,
            from,
            worktree,
            project,
        }) => nebula_tui::run_switch(branch, from, worktree, project),
        Some(Command::Agent { command }) => match command {
            AgentCommand::New {
                worktree,
                project,
                kind,
                model,
                effort,
                name,
                prompt,
            } => nebula_tui::run_agent_new(nebula_tui::NewAgentOpts {
                worktree,
                project,
                kind,
                model,
                effort,
                name: name.map(|words| words.join(" ")),
                prompt,
            }),
            AgentCommand::Wait {
                names,
                timeout,
                project,
            } => nebula_tui::run_agent_wait(names, timeout, project),
            AgentCommand::List {
                project,
                all,
                worktree,
            } => nebula_tui::run_agent_list(project, all, worktree),
            AgentCommand::Show { name, project } => nebula_tui::run_agent_show(name, project),
            AgentCommand::Read {
                name,
                lines,
                project,
            } => nebula_tui::run_agent_read(name, lines, project),
            AgentCommand::Send {
                name,
                text,
                project,
            } => nebula_tui::run_agent_send(name, text.join(" "), project),
            AgentCommand::Archive { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Archive, name, project)
            }
            AgentCommand::Unarchive { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Unarchive, name, project)
            }
            AgentCommand::Delete { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Delete, name, project)
            }
            AgentCommand::Restart { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Restart, name, project)
            }
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
        Some(Command::Remote { host, command }) => nebula_tui::run_remote(
            host,
            match command {
                RemoteCommand::Status => nebula_tui::RemoteOp::Status,
                RemoteCommand::Sessions => nebula_tui::RemoteOp::Sessions,
                RemoteCommand::Watch => nebula_tui::RemoteOp::Watch,
                RemoteCommand::Sync { force_env } => nebula_tui::RemoteOp::Sync { force_env },
                RemoteCommand::Upgrade => nebula_tui::RemoteOp::Upgrade,
                RemoteCommand::Restart => nebula_tui::RemoteOp::Restart,
            },
        ),
        Some(Command::Ssh { host, path }) => ssh::run_ssh(&host, path.as_deref()),
        Some(Command::Upgrade { force }) => upgrade::run_upgrade(force),
        Some(Command::Proxy) => nebula_tui::run_proxy(),
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
/// handoff: the TUI quit and restored the terminal so a fresh process can
/// exec over us — `nebula ssh` for the hosts picker, or the binary on
/// disk for `⌘K r` reload (the local daemon and its sessions stay up).
fn run_tui_and_handoff(open_at: Option<std::path::PathBuf>) -> Result<()> {
    use nebula_tui::event_loop::Handoff;
    init_tui_logging()?;
    let handoff = log_fatal(
        nebula_tui::run_tui(open_at),
        nebula_core::paths::tui_log_path(),
    )?;
    match handoff {
        Handoff::Ssh(entry) => {
            eprintln!("nebula: connecting to {}…", entry.host);
            ssh::run_ssh(&entry.host, entry.path.as_deref())
        }
        Handoff::Reload => {
            // Same argv against the launch path: after `make install` the
            // path holds the new build (cp + mv replaces the file, so the
            // running image was never touched).
            use anyhow::Context as _;
            use std::os::unix::process::CommandExt;
            let exe = std::env::current_exe().context("resolve current executable")?;
            let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
            eprintln!("nebula: reloading…");
            let err = std::process::Command::new(&exe).args(&args).exec();
            Err(anyhow::anyhow!(
                "re-exec of {} failed: {err}",
                exe.display()
            ))
        }
        Handoff::None => Ok(()),
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
