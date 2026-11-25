#![allow(
    clippy::disallowed_methods,
    reason = "clippy.toml bans sleeps and wall clock in integration tests only; product code waits and times by design"
)]

mod preview;
mod ssh;
mod upgrade;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nebula_core::board::{Role, Stage};

#[derive(Parser)]
#[command(
    name = "nebula",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("NEBULA_BUILD_ID"), ")"),
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
    /// Manage private previews on this host without contacting the daemon.
    Preview(preview::Args),
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
    ///
    /// With --session it lands attached to that session instead, opening
    /// its workspace and unhiding its project when needed. A running TUI
    /// takes the attach, its window comes to the front, and once the pane
    /// holds the session's terminal the command prints
    /// {"landed":"tui","tui_pid",id,name,project,host,focused} (host null
    /// when local). With no TUI running it starts one here when run in a
    /// terminal, else exits 3. An unknown, archived or ambiguous session,
    /// or an attach that fails, exits 1.
    Open {
        /// Directory to land on (default: the current directory).
        #[arg(default_value = ".", conflicts_with = "session")]
        path: String,
        /// Session to attach: its id or its name, as `nebula agent list
        /// --all` prints them (remote @host sessions included).
        #[arg(long, value_name = "ID|NAME")]
        session: Option<String>,
        /// Look the --session name up in this project only, `name@host`
        /// for a remote one (default: every project).
        #[arg(long, requires = "session")]
        project: Option<String>,
        /// Leave the window where it is (the attach still happens).
        /// Focusing goes through System Events, so the calling app needs
        /// macOS Automation permission for it.
        #[arg(long, requires = "session")]
        no_focus: bool,
        /// Run this open on HOST over ssh (BatchMode) and print its object;
        /// an unreachable HOST exits 3. This machine's own name opens here.
        #[arg(long, value_name = "HOST", requires = "session")]
        on: Option<String>,
        /// Print the landed object as JSON (what --session prints anyway).
        #[arg(long, requires = "session")]
        json: bool,
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
        /// Stop the agents and shells on the primary checkout instead of
        /// refusing; warm agent PTYs are always stopped.
        #[arg(long)]
        stop_busy: bool,
    },
    /// Spawn and inspect agent sessions from the command line — how a
    /// session delegates work to and manages its workers.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Diagrams for this checkout: self-contained HTML pages in
    /// `.scratch/diagrams/`, shown inside the TUI with ⌘X. Agents create
    /// them with `new`, edit the file, then `open` it for the user.
    Diagram {
        #[command(subcommand)]
        command: DiagramCommand,
    },
    /// Read or edit the notes for the current project/worktree (agents can
    /// too — inside a session the target is the session's own project).
    Notes {
        #[command(subcommand)]
        command: Option<NotesCommand>,
    },
    /// Cards on the board: add, move and work them (--json for agents).
    /// A daemon without a `board` key refuses every card verb.
    Todo {
        #[command(subcommand)]
        command: Option<TodoCommand>,
    },
    /// Publish what a session produced (recap, e2e run, diagram, ticket,
    /// prototype, link): copied into this daemon's data dir, so it outlives
    /// the worktree, and linked to the caller's card, session and checkout.
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Record why a choice was made, on the caller's card: an artifact of
    /// kind decision that the board host also copies to RAG.
    Decision {
        #[command(subcommand)]
        command: DecisionCommand,
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
    /// Knowledge base for agents: notes, shared know-how and every app's
    /// markdown docs, searched semantically. Lives on findl; elsewhere this
    /// runs `ssh findl nebula rag …`.
    Rag(nebula_rag::cli::RagArgs),
    /// Collect a settled remote checkout through origin, preserving both
    /// checkouts and all sessions. Prints JSON on success, dry-run, and refusal.
    Collect {
        /// Remote branch, checkout id, directory name, absolute path, or primary.
        selector: String,
        /// Collect into the local primary checkout (refuses shared/dirty primary).
        #[arg(long, conflicts_with = "worktree")]
        branch: bool,
        /// Collect into a separate local worktree. Without a mode flag, infer
        /// primary vs worktree from the remote source checkout.
        #[arg(long)]
        worktree: bool,
        /// Local project name or id (default: calling session, then cwd).
        #[arg(long)]
        project: Option<String>,
        /// Configured twin host; inferred when exactly one twin exists.
        #[arg(long)]
        host: Option<String>,
        /// Sole owning remote agent id or name. Multiple live owners refuse.
        #[arg(long)]
        agent: Option<String>,
        /// Inspect without prompts, fetches, or filesystem changes.
        #[arg(long)]
        dry_run: bool,
        /// Seconds to wait for a new handoff turn (busy agents refuse immediately).
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Bring this project's checkout and its twin on a host in step: report
    /// both sides, move commits the safe way through origin, carry `.env*`
    /// (laptop → host, existing kept), `.scratch/` (both ways, newer wins,
    /// nothing deleted) and skills. Refuses, naming the reason and the fix,
    /// whenever moving would lose work. Inside a session the project is the
    /// caller's; outside, the checkout holding the cwd, else --project.
    Sync {
        /// Only send: this checkout → the host (refuses when the host is
        /// ahead).
        #[arg(long, value_name = "HOST", conflicts_with = "from")]
        to: Option<String>,
        /// Only collect: the host → this checkout (refuses when this side
        /// is ahead; sends no `.env*` or skills).
        #[arg(long, value_name = "HOST")]
        from: Option<String>,
        /// Print both sides' state and the plan as JSON, change nothing
        /// (exit 0 even with the host unreachable).
        #[arg(long)]
        status: bool,
        /// Print the plan, change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Diverged branches: rebase this checkout onto the host's commits
        /// (never the reverse). Never the default.
        #[arg(long)]
        rebase: bool,
        /// Overwrite `.env` files the host already has with the local ones.
        #[arg(long)]
        force_env: bool,
        /// Skip the skills mirror (nebula-sync-skills).
        #[arg(long)]
        no_skills: bool,
        /// Project name when run outside any checkout.
        #[arg(long)]
        project: Option<String>,
        /// Which local checkout of the project: a branch name, a directory
        /// name, or "root"/"primary" (default: the calling session's, else
        /// the one holding the cwd). Only its twin checkout of the same
        /// branch is touched on the host.
        #[arg(long)]
        worktree: Option<String>,
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
    /// Print the running daemon's build id (`nebula --version`'s sha); exits
    /// 1 when no daemon runs or it predates build ids. `remote status` reads it.
    #[command(hide = true, name = "_daemon-build")]
    DaemonBuild,
    /// Pipe stdin/stdout to this machine's daemon socket — what a remote
    /// nebula runs over ssh to reach the daemon here.
    #[command(hide = true)]
    Proxy {
        /// Fail when no daemon is running instead of starting one.
        #[arg(long)]
        connect_only: bool,
    },
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
    /// Remove only a clean, merged, session-free worktree by ID. Keeps its
    /// branch. Prints a verdict per criterion: files (ignored links, env
    /// copies and setup output such as node_modules/dist pass; other
    /// ignored files are user data), base (created_from, else git config
    /// `nebula.base`, else the remote default branch, else main/master;
    /// never fetches), merged (ancestor, patch-id or squash tree) and
    /// sessions.
    DeleteSafe {
        id: String,
        branch: String,
        path: std::path::PathBuf,
        /// Print the verdicts and change nothing.
        #[arg(long)]
        check: bool,
        /// Delete archived sessions and exited terminals with the checkout;
        /// live or unarchived sessions still refuse.
        #[arg(long)]
        with_archived: bool,
        #[arg(long)]
        project: Option<String>,
    },
    /// Drop rows whose checkout directory and git worktree entry are both
    /// gone. A row with a live or unarchived session is never dropped.
    Prune {
        /// List the ghost rows and change nothing.
        #[arg(long)]
        check: bool,
        /// Delete archived sessions and exited terminals with their row.
        #[arg(long)]
        with_archived: bool,
        /// Project name (default: the calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Deliberately switch primary. Refused while other sessions run there
    /// (unless `--stop-busy`), primary has uncommitted changes, an untracked
    /// file the branch would overwrite, or the branch already has a checkout;
    /// use that checkout instead.
    Checkout {
        /// Local branch name.
        branch: String,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
        /// Stop the agents and shells on the primary checkout instead of
        /// refusing; warm agent PTYs are always stopped.
        #[arg(long)]
        stop_busy: bool,
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
        /// claude | codex | cursor | pi | antigravity.
        #[arg(long, default_value = "claude")]
        kind: String,
        /// Model the CLI launches with (default: the CLI's own; antigravity:
        /// gemini-4-neon-medium).
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
        /// Mark the session as the checkout's one writer. While another
        /// unarchived session holds that mark (live PTY, or running,
        /// needs_feedback, finished or disconnected), wait for it to exit or
        /// be archived, then spawn; exit 2 if it still holds the mark after
        /// --writer-wait.
        #[arg(long)]
        writer: bool,
        /// Seconds --writer waits for the current writer; 0 refuses at once.
        #[arg(long, default_value_t = 600)]
        writer_wait: u64,
        /// Spawn as the writer alongside a live one, after the user approved
        /// two writers on the checkout; implies --writer.
        #[arg(long)]
        force_writer: bool,
        /// Launch claude with Code Mode off (`CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=0`)
        /// on every spawn and resume, e.g. for Claude in Chrome or computer
        /// use; other kinds ignore it.
        #[arg(long)]
        no_code_mode: bool,
        /// The board card the session works on (#12, 12 or a ULID); a card
        /// the board does not have refuses before anything spawns.
        #[arg(long, value_name = "REF", conflicts_with = "no_card")]
        card: Option<String>,
        /// Link the session to no card.
        #[arg(long)]
        no_card: bool,
        /// worker, review, fix, verify or research.
        #[arg(long, value_parser = parse_role)]
        role: Option<Role>,
        /// The `todo run` dispatch this spawn fulfils; a repeat answers with
        /// the session that dispatch already started.
        #[arg(long, value_name = "ID")]
        dispatch: Option<String>,
        /// Accepted for symmetry with the board verbs; the output is JSON anyway.
        #[arg(long)]
        json: bool,
    },
    /// Block until workers settle out of running (finished, needs_feedback,
    /// terminated…), then print the settled ones as JSON — `agent list`'s
    /// row shape. No names means every unarchived worker of the project.
    ///
    /// Exit codes: 0 the workers settled (with --any, at least one);
    /// 1 timed out or failed; 3 a submitted Enter reached the calling
    /// session (`agent send` from any host or version, the TUI, the web app),
    /// which ends the wait early so the session reads the queued message —
    /// stdout then lists only the workers already settled. Exit 3 needs the daemon on the
    /// session's host to be this build or newer; an older one never ends a
    /// wait early.
    Wait {
        /// Session names to wait on (as shown in the panels).
        names: Vec<String>,
        /// Return as soon as the first named worker settles.
        #[arg(long, requires = "names")]
        any: bool,
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
        /// The prompt; multiple words need no quotes. Up to 1,000 bytes on
        /// one line: longer or multi-line text is refused (exit 2), since a
        /// worker reads it as a pasted quote — write it to a file and use `--file`.
        #[arg(required_unless_present = "file", num_args = 1..)]
        text: Vec<String>,
        /// Send `Read <absolute path> and act on it.` instead of text; the
        /// file must exist and be a regular file. With a `name@host`
        /// project, give the file's absolute path on that host; it is sent
        /// as given.
        #[arg(long, conflicts_with = "text")]
        file: Option<std::path::PathBuf>,
        /// Send long or multi-line text as one bracketed paste anyway — for a
        /// shell tab or a CLI that treats a paste as input.
        #[arg(long)]
        paste: bool,
        /// Project name, or `name@host` for a remote project (default: the
        /// calling session's project).
        #[arg(long)]
        project: Option<String>,
    },
    /// Send one terminal key by stable agent ID, without submitting a prompt.
    Key {
        id: String,
        #[arg(value_parser = ["Up", "Down", "Left", "Right", "Enter", "Escape", "Tab"])]
        key: String,
        #[arg(long)]
        project: Option<String>,
    },
    /// Delete exactly this agent ID, never resolve a mutable session name.
    DeleteId {
        id: String,
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
enum DiagramCommand {
    /// Write a new diagram from the template and print its absolute path.
    /// Inside a nebula session the diagram records that session as its
    /// owner (decision pages report back to it).
    New {
        /// File name without `.html`: lowercase letters, digits, hyphens.
        slug: String,
        /// Page title — what the viewer lists and searches (default: the slug).
        #[arg(long)]
        title: Option<String>,
        /// Tag for search; repeatable.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Decision page: option cards whose "choose" button reports the
        /// choice back to the owning session.
        #[arg(long)]
        decision: bool,
        /// Overwrite an existing diagram of the same slug.
        #[arg(long)]
        force: bool,
    },
    /// Validate an existing HTML file (is HTML, has a <title>; warns on
    /// network references) and copy it into the folder.
    Add {
        file: std::path::PathBuf,
        /// Slug to store it under (default: derived from the file name).
        #[arg(long)]
        slug: Option<String>,
        /// Overwrite an existing diagram of the same slug.
        #[arg(long)]
        force: bool,
    },
    /// List diagrams, newest first; a query fuzzy-matches slug, title, tags.
    List {
        query: Option<String>,
        /// Print a JSON array (slug, title, tags, session, path, mtime_ms).
        #[arg(long)]
        json: bool,
    },
    /// Ask the running TUI to show a diagram (default: the newest) — the
    /// same viewer ⌘X opens.
    Open { slug: Option<String> },
    /// Delete a diagram (and its decisions log).
    Rm { slug: String },
    /// Print the diagrams folder of this checkout.
    Path,
    /// Print the choices the user made on a decision page, oldest first.
    Decisions {
        slug: String,
        /// Print a JSON array of `{ts, slug, id, choice, label, note}`.
        #[arg(long)]
        json: bool,
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
    /// Add a card to the board (default project: the caller's own).
    Add {
        /// The card's title; multiple words need no quotes.
        #[arg(required = true, num_args = 1..)]
        title: Vec<String>,
        /// Project name.
        #[arg(long, conflicts_with = "home")]
        project: Option<String>,
        /// A home card, in no project.
        #[arg(long)]
        home: bool,
        #[arg(long, default_value = "backlog", value_parser = parse_stage)]
        stage: Stage,
        /// Propose it: it waits in the proposal tray until accepted.
        #[arg(long)]
        propose: bool,
        /// The card's ULID, so a rerun of the same add creates nothing twice.
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change a card's title or project.
    Edit {
        card: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, conflicts_with = "home")]
        project: Option<String>,
        /// Make it a home card.
        #[arg(long)]
        home: bool,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Move a card: backlog, next, doing, review or done. Exits 6 when
    /// --expect is set and the card has left that stage.
    Move {
        card: String,
        #[arg(value_parser = parse_stage)]
        stage: Stage,
        #[arg(long, value_parser = parse_stage)]
        expect: Option<Stage>,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Archive a card; its number stays reserved.
    Archive {
        card: String,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Accept a proposed card onto the board.
    Accept {
        card: String,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Dismiss a proposed card (archives it).
    Dismiss {
        card: String,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Append to a card's log, as the caller.
    Note {
        card: String,
        /// The note; multiple words need no quotes.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// The note's ULID, so a rerun adds nothing twice.
        #[arg(long)]
        id: Option<String>,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Mark a card blocked by another; self-blocks and cycles exit 6.
    Block {
        card: String,
        #[arg(long)]
        by: String,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Remove a block.
    Unblock {
        card: String,
        #[arg(long)]
        by: String,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// List cards (default: the caller's project, else every card).
    List {
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long, value_parser = parse_stage)]
        stage: Option<Stage>,
        /// Archived cards too.
        #[arg(long)]
        archived: bool,
        /// Continue from a previous reply's `next`.
        #[arg(long)]
        page: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// One card with its sessions, artifacts and log. Exits 5 when no card
    /// in scope has that ref.
    Show {
        card: String,
        #[command(flatten)]
        scope: ScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// The board: cards, hosts and the away digest. With --since it answers
    /// {"version","unchanged":true} when nothing changed.
    Board {
        #[command(flatten)]
        scope: ScopeArgs,
        /// A `version` from an earlier reply.
        #[arg(long)]
        since: Option<String>,
        /// Epoch ms of the viewer's last visit, for the away digest.
        #[arg(long)]
        away_since: Option<i64>,
        /// Continue from a previous reply's `next`; exits 6 once the board
        /// has changed since that page.
        #[arg(long)]
        page: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Start a session on a card: refused (exit 6) for an archived, blocked
    /// or home card, or when the checkout's writer is taken. A rerun with
    /// the same --dispatch answers with the session it already started.
    Run {
        card: String,
        #[arg(long, value_parser = parse_role)]
        role: Role,
        /// claude | codex | cursor | pi | antigravity.
        #[arg(long)]
        kind: String,
        /// The host to run on.
        #[arg(long, default_value = "leaseweb")]
        host: String,
        /// `new` for a fresh worktree, or a branch.
        #[arg(long)]
        worktree: String,
        /// The dispatch's ULID, so a rerun spawns nothing twice.
        #[arg(long)]
        dispatch: Option<String>,
        /// Set on the hop to the target host, which then never forwards again.
        #[arg(long, hide = true)]
        forwarded: bool,
        /// With --parent-id, the spawning session on the host that forwarded the run.
        #[arg(long, hide = true, requires = "parent_id")]
        parent_host: Option<String>,
        #[arg(long, hide = true, requires = "parent_host")]
        parent_id: Option<String>,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Answer a card's waiting session with its approval key, only while its
    /// status is still the one stamped --expected-status-at (else exit 6).
    Approve {
        card: String,
        #[arg(long)]
        session: String,
        /// The session's status_changed_at, as `todo show` printed it.
        #[arg(long)]
        expected_status_at: i64,
        #[command(flatten)]
        scope: WriteScopeArgs,
        #[arg(long)]
        json: bool,
    },
    /// Check a legacy todo off, by its number in the old todo list.
    Done { index: usize },
    /// Bring a checked-off legacy todo back.
    Reopen { index: usize },
    /// Check a legacy todo's note off.
    NoteDone {
        /// The todo's number in the old todo list.
        index: usize,
        /// The note's number under it.
        note: usize,
    },
}

#[derive(Subcommand)]
enum ArtifactCommand {
    /// Publish a file, a directory or a URL. A directory of a recap,
    /// diagram or prototype opens at its index.html; an e2e-run needs a
    /// valid result.json naming only files it holds. Symlinks, special
    /// files and trees over 64 MiB are refused, and nothing is stored.
    Add {
        /// A path, or an http(s) URL (published as kind link).
        source: String,
        /// recap, e2e-run, diagram, ticket or prototype (required for a path).
        #[arg(long)]
        kind: Option<String>,
        /// Default: the file or directory name, or the URL.
        #[arg(long)]
        title: Option<String>,
        /// Link this card instead of the caller's (#12, 12 or a ULID).
        #[arg(long, conflicts_with = "no_card")]
        card: Option<String>,
        /// Link no card, even inside a carded session.
        #[arg(long)]
        no_card: bool,
        /// Its project when no card is linked (default: the caller's).
        #[arg(long, conflicts_with = "card")]
        project: Option<String>,
        /// The artifact's ULID, so a rerun stores and publishes nothing twice.
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List published artifacts (default: the caller's project, else all).
    List {
        #[command(flatten)]
        scope: ScopeArgs,
        /// Only this card's.
        #[arg(long)]
        card: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        /// Continue from a previous reply's `next`.
        #[arg(long)]
        page: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// One artifact, removed ones included (with removed_at). Exits 5 when
    /// there is none.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Open it here, or print the command that opens it on the host that
    /// stores it.
    Open { id: String },
    /// Hide it from lists; `show` still reports it, and the bytes stay.
    Rm {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DecisionCommand {
    /// Record a decision, linked to the caller's card, session and checkout.
    Add {
        /// What was decided.
        what: String,
        /// Why.
        #[arg(long)]
        why: Option<String>,
        /// Link this card instead of the caller's (#12, 12 or a ULID).
        #[arg(long, conflicts_with = "project")]
        card: Option<String>,
        /// Record it on this project with no card.
        #[arg(long)]
        project: Option<String>,
        /// Record it in no project and on no card, wherever it runs.
        #[arg(long, conflicts_with_all = ["card", "project"])]
        home: bool,
        /// The decision's ULID, so a rerun records nothing twice.
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List decisions (default: the caller's project, else all).
    List {
        #[command(flatten)]
        scope: ScopeArgs,
        /// Only those whose what or why holds this text.
        #[arg(long)]
        query: Option<String>,
        /// Continue from a previous reply's `next`.
        #[arg(long)]
        page: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// One decision, with meta.rag_note once RAG holds it. Exits 5 when
    /// there is none.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Args)]
struct ScopeArgs {
    /// Only these projects (repeat for more).
    #[arg(long)]
    project: Vec<String>,
    /// Every project.
    #[arg(long, conflicts_with = "project")]
    all: bool,
    /// Home cards (no project) too.
    #[arg(long)]
    include_home: bool,
}

impl From<ScopeArgs> for nebula_tui::board_cli::ScopeFlags {
    fn from(args: ScopeArgs) -> Self {
        Self {
            projects: args.project,
            all: args.all,
            include_home: args.include_home,
        }
    }
}

#[derive(clap::Args)]
struct WriteScopeArgs {
    /// Refuse (exit 5) a card outside this project; repeat for more, `*`
    /// for every project. Unlike --project, it never moves a card.
    #[arg(long = "scope", value_name = "PROJECT")]
    scope: Vec<String>,
    /// Home cards (no project) are in scope too.
    #[arg(long)]
    include_home: bool,
}

impl From<WriteScopeArgs> for nebula_tui::board_cli::ScopeFlags {
    fn from(args: WriteScopeArgs) -> Self {
        Self {
            all: args.scope.iter().any(|p| p == "*"),
            projects: args.scope.into_iter().filter(|p| p != "*").collect(),
            include_home: args.include_home,
        }
    }
}

fn parse_stage(text: &str) -> Result<Stage, String> {
    Stage::parse(text)
        .ok_or_else(|| format!("{text:?} is not a stage (backlog, next, doing, review, done)"))
}

fn parse_role(text: &str) -> Result<Role, String> {
    Role::parse(text)
        .ok_or_else(|| format!("{text:?} is not a role (worker, review, fix, verify, research)"))
}

#[derive(Subcommand)]
enum RemoteCommand {
    /// nebula version and daemon on the host, this daemon's sessions there,
    /// leftover agent processes, and whether its build and skills mirror
    /// match this machine's (reporting only; `sync`/`upgrade` change them).
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
    /// Install `kind`'s hooks (claude, codex, cursor, pi, antigravity) for `dir`.
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
    nebula_core::build_id::set(env!("NEBULA_BUILD_ID"));
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Preview(args)) => preview::run(args),
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
            nebula_daemon::hooks::installer::install_for_kind(kind, &dir)?;
            nebula_daemon::hooks::installer::trust_for_kind(kind, &dir)
        }
        Some(Command::Open {
            session: Some(reference),
            project,
            no_focus,
            on,
            ..
        }) => {
            use std::io::IsTerminal;
            if let Some(host) = on.filter(|h| !nebula_core::host::names_this_machine(h)) {
                let code =
                    nebula_tui::run_open_on_host(&host, &reference, project.as_deref(), !no_focus)?;
                if code != 0 {
                    std::process::exit(code);
                }
                return Ok(());
            }
            let Some(id) = nebula_tui::run_open_session(reference, project, !no_focus)? else {
                return Ok(());
            };
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                return run_tui_and_handoff(Some(nebula_tui::app::OpenAt::Session(id)));
            }
            eprintln!(
                "nebula: no nebula TUI is running — start one in a terminal with `nebula open --session {id}`"
            );
            std::process::exit(3);
        }
        Some(Command::Open { path, .. }) => {
            // Absolute before the TUI starts: the daemon and the panels
            // both need a path that survives any later cwd changes.
            let dir = std::fs::canonicalize(&path)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(&path));
            run_tui_and_handoff(Some(nebula_tui::app::OpenAt::Dir(dir)))
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
            WorktreeCommand::DeleteSafe {
                id,
                branch,
                path,
                check,
                with_archived,
                project,
            } => nebula_tui::run_worktree_delete_safe(
                id,
                branch,
                path,
                check,
                with_archived,
                project,
            ),
            WorktreeCommand::Prune {
                check,
                with_archived,
                project,
            } => nebula_tui::run_worktree_prune(check, with_archived, project),
            WorktreeCommand::Delete {
                name,
                force,
                project,
            } => nebula_tui::run_worktree_delete(name, force, project),
            WorktreeCommand::Checkout {
                branch,
                project,
                stop_busy,
            } => nebula_tui::run_worktree_checkout(branch, project, stop_busy),
        },
        Some(Command::Switch {
            branch,
            from,
            worktree,
            project,
            stop_busy,
        }) => nebula_tui::run_switch(branch, from, worktree, project, stop_busy),
        Some(Command::Agent { command }) => match command {
            AgentCommand::New {
                worktree,
                project,
                kind,
                model,
                effort,
                name,
                prompt,
                writer,
                writer_wait,
                force_writer,
                no_code_mode,
                card,
                no_card,
                role,
                dispatch,
                ..
            } => nebula_tui::run_agent_new(nebula_tui::NewAgentOpts {
                worktree,
                project,
                kind,
                model,
                effort,
                name: name.map(|words| words.join(" ")),
                prompt,
                writer: match (writer, force_writer) {
                    (_, true) => nebula_core::WriterClaim::Forced,
                    (true, false) => nebula_core::WriterClaim::Exclusive,
                    (false, false) => nebula_core::WriterClaim::Off,
                },
                writer_wait: std::time::Duration::from_secs(writer_wait),
                code_mode_off: no_code_mode,
                card,
                no_card,
                role,
                dispatch: dispatch
                    .as_deref()
                    .map(nebula_tui::board_cli::dispatch_id)
                    .transpose()?,
                parent: None,
            }),
            AgentCommand::Wait {
                names,
                any,
                timeout,
                project,
            } => nebula_tui::run_agent_wait(names, any, timeout, project),
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
                file,
                paste,
                project,
            } => nebula_tui::run_agent_send(name, text.join(" "), file, paste, project),
            AgentCommand::Archive { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Archive, name, project)
            }
            AgentCommand::Unarchive { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Unarchive, name, project)
            }
            AgentCommand::Key { id, key, project } => {
                nebula_tui::run_agent_action_id(id, Some(key), project)
            }
            AgentCommand::DeleteId { id, project } => {
                nebula_tui::run_agent_action_id(id, None, project)
            }
            AgentCommand::Delete { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Delete, name, project)
            }
            AgentCommand::Restart { name, project } => {
                nebula_tui::run_agent_ctl(nebula_tui::AgentCtl::Restart, name, project)
            }
        },
        Some(Command::Diagram { command }) => {
            use nebula_tui::DiagramOp;
            nebula_tui::run_diagram(match command {
                DiagramCommand::New {
                    slug,
                    title,
                    tags,
                    decision,
                    force,
                } => DiagramOp::New {
                    slug,
                    title,
                    tags,
                    decision,
                    force,
                },
                DiagramCommand::Add { file, slug, force } => DiagramOp::Add { file, slug, force },
                DiagramCommand::List { query, json } => DiagramOp::List { query, json },
                DiagramCommand::Open { slug } => DiagramOp::Open { slug },
                DiagramCommand::Rm { slug } => DiagramOp::Rm { slug },
                DiagramCommand::Path => DiagramOp::Path,
                DiagramCommand::Decisions { slug, json } => DiagramOp::Decisions { slug, json },
            })
        }
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
            use nebula_tui::board_cli::TodoVerb;
            use nebula_tui::TodoOp;
            let legacy = |op| nebula_tui::run_todo(op);
            let (verb, json) = match command {
                Some(TodoCommand::Done { index }) => return legacy(TodoOp::Done { index }),
                Some(TodoCommand::Reopen { index }) => return legacy(TodoOp::Reopen { index }),
                Some(TodoCommand::NoteDone { index, note }) => {
                    return legacy(TodoOp::NoteDone { index, note })
                }
                None => (
                    TodoVerb::List {
                        scope: Default::default(),
                        stage: None,
                        archived: false,
                        page: None,
                    },
                    false,
                ),
                Some(TodoCommand::Add {
                    title,
                    project,
                    home,
                    stage,
                    propose,
                    id,
                    json,
                }) => (
                    TodoVerb::Add {
                        title: title.join(" "),
                        project,
                        home,
                        stage,
                        propose,
                        id,
                    },
                    json,
                ),
                Some(TodoCommand::Edit {
                    card,
                    title,
                    project,
                    home,
                    scope,
                    json,
                }) => (
                    TodoVerb::Edit {
                        card,
                        title,
                        project,
                        home,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Move {
                    card,
                    stage,
                    expect,
                    scope,
                    json,
                }) => (
                    TodoVerb::Move {
                        card,
                        stage,
                        expect,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Archive { card, scope, json }) => (
                    TodoVerb::Archive {
                        card,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Accept { card, scope, json }) => (
                    TodoVerb::Accept {
                        card,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Dismiss { card, scope, json }) => (
                    TodoVerb::Dismiss {
                        card,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Note {
                    card,
                    text,
                    id,
                    scope,
                    json,
                }) => (
                    TodoVerb::Note {
                        card,
                        text: text.join(" "),
                        id,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Block {
                    card,
                    by,
                    scope,
                    json,
                }) => (
                    TodoVerb::Block {
                        card,
                        by,
                        on: true,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Unblock {
                    card,
                    by,
                    scope,
                    json,
                }) => (
                    TodoVerb::Block {
                        card,
                        by,
                        on: false,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::List {
                    scope,
                    stage,
                    archived,
                    page,
                    json,
                }) => (
                    TodoVerb::List {
                        scope: scope.into(),
                        stage,
                        archived,
                        page,
                    },
                    json,
                ),
                Some(TodoCommand::Run {
                    card,
                    role,
                    kind,
                    host,
                    worktree,
                    dispatch,
                    forwarded,
                    parent_host,
                    parent_id,
                    scope,
                    json,
                }) => (
                    TodoVerb::Run {
                        card,
                        role,
                        kind,
                        host,
                        worktree,
                        dispatch,
                        scope: scope.into(),
                        forwarded,
                        parent: parent_host.zip(parent_id).map(|(host, id)| {
                            nebula_core::board::SessionAt {
                                host,
                                id: nebula_core::AgentId(id),
                            }
                        }),
                    },
                    json,
                ),
                Some(TodoCommand::Approve {
                    card,
                    session,
                    expected_status_at,
                    scope,
                    json,
                }) => (
                    TodoVerb::Approve {
                        card,
                        session,
                        expected_status_at,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Show { card, scope, json }) => (
                    TodoVerb::Show {
                        card,
                        scope: scope.into(),
                    },
                    json,
                ),
                Some(TodoCommand::Board {
                    scope,
                    since,
                    away_since,
                    page,
                    json,
                }) => (
                    TodoVerb::Board {
                        scope: scope.into(),
                        since,
                        away_since,
                        page,
                    },
                    json,
                ),
            };
            nebula_tui::run_board(verb, json)
        }
        Some(Command::Artifact { command }) => {
            use nebula_tui::artifact_cli::{AddArgs, ArtifactVerb, CardChoice};
            let (verb, json) = match command {
                ArtifactCommand::Add {
                    source,
                    kind,
                    title,
                    card,
                    no_card,
                    project,
                    id,
                    json,
                } => (
                    ArtifactVerb::Add(AddArgs {
                        source,
                        kind,
                        title,
                        project,
                        id,
                        card: match (card, no_card) {
                            (Some(card), _) => CardChoice::Card(card),
                            (None, true) => CardChoice::None,
                            (None, false) => CardChoice::Caller,
                        },
                    }),
                    json,
                ),
                ArtifactCommand::List {
                    scope,
                    card,
                    kind,
                    page,
                    json,
                } => (
                    ArtifactVerb::List {
                        scope: scope.into(),
                        card,
                        kind,
                        page,
                    },
                    json,
                ),
                ArtifactCommand::Show { id, json } => (ArtifactVerb::Show { id }, json),
                ArtifactCommand::Open { id } => (ArtifactVerb::Open { id }, false),
                ArtifactCommand::Rm { id, json } => (ArtifactVerb::Rm { id }, json),
            };
            nebula_tui::run_artifact(verb, json)
        }
        Some(Command::Decision { command }) => {
            use nebula_tui::decision_cli::DecisionVerb;
            let (verb, json) = match command {
                DecisionCommand::Add {
                    what,
                    why,
                    card,
                    project,
                    home,
                    id,
                    json,
                } => (
                    DecisionVerb::Add {
                        what,
                        why,
                        card,
                        project,
                        home,
                        id,
                    },
                    json,
                ),
                DecisionCommand::List {
                    scope,
                    query,
                    page,
                    json,
                } => (
                    DecisionVerb::List {
                        scope: scope.into(),
                        query,
                        page,
                    },
                    json,
                ),
                DecisionCommand::Show { id, json } => (DecisionVerb::Show { id }, json),
            };
            nebula_tui::run_decision(verb, json)
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
        Some(Command::Rag(args)) => nebula_rag::cli::run(args),
        Some(Command::Collect {
            selector,
            branch,
            worktree,
            project,
            host,
            agent,
            dry_run,
            timeout,
        }) => {
            use nebula_tui::collect_cli::{CollectOpts, Mode};
            nebula_tui::run_collect(CollectOpts {
                selector,
                mode: if branch {
                    Some(Mode::Branch)
                } else if worktree {
                    Some(Mode::Worktree)
                } else {
                    None
                },
                project,
                host,
                agent,
                dry_run,
                timeout: std::time::Duration::from_secs(timeout),
            })
        }
        Some(Command::Sync {
            to,
            from,
            status,
            dry_run,
            rebase,
            force_env,
            no_skills,
            project,
            worktree,
        }) => {
            // `--to`/`--from` name the host for the reader; the twin decides
            // which host it is, so a mismatch is an error, not a redirect.
            let direction = match (&to, &from) {
                (Some(_), _) => nebula_tui::SyncDirection::To,
                (_, Some(_)) => nebula_tui::SyncDirection::From,
                _ => nebula_tui::SyncDirection::Both,
            };
            nebula_tui::run_sync(nebula_tui::SyncOpts {
                direction,
                named_host: to.or(from),
                status,
                dry_run,
                rebase,
                force_env,
                skills: !no_skills,
                project,
                worktree,
            })
        }
        Some(Command::Ssh { host, path }) => ssh::run_ssh(&host, path.as_deref()),
        Some(Command::Upgrade { force }) => upgrade::run_upgrade(force),
        Some(Command::Proxy { connect_only }) => nebula_tui::run_proxy(connect_only),
        Some(Command::StaleDaemonNote) => {
            if nebula_daemon::lifecycle::daemon_is_stale() {
                println!("note: the running daemon was built from older code.");
                println!(
                    "      run 'nebula kill' to restart onto the new binary (stops ALL sessions)."
                );
            }
            Ok(())
        }
        Some(Command::DaemonBuild) => match nebula_daemon::lifecycle::daemon_build_id() {
            Some(id) => {
                println!("{id}");
                Ok(())
            }
            None => std::process::exit(1),
        },
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
fn run_tui_and_handoff(open_at: Option<nebula_tui::app::OpenAt>) -> Result<()> {
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
