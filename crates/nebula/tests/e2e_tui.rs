//! True end-to-end TUI test: runs the real `nebula` binary inside a PTY,
//! sends literal keystrokes, and parses the rendered frames with vt100 —
//! asserting what a user would actually see on screen, including which row
//! is highlighted and which panel has focus.
//!
//! Flow under test:
//!   add two projects → Tab-cycle focus → create two worktrees →
//!   j/k selection between worktrees → Enter into the sessions panel →
//!   create an agent (auto-attach) → per-worktree session isolation →
//!   j/k toggling between projects updates the worktree panel.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COLS: u16 = 120;
const ROWS: u16 = 36;
const WAIT: Duration = Duration::from_secs(20);

// Distinct footer hints identify the focused panel on screen.
const FOOTER_PROJECTS: &str = "n/o: add";
const FOOTER_WORKTREES: &str = "n: new worktree";
const FOOTER_SESSIONS: &str = "n: agent";
/// Terminal pane focused but NOT input-locked (attached session).
const FOOTER_TERMINAL_FOCUSED: &str = "Enter: type into terminal";
/// Terminal pane input-locked: keys forward to the PTY.
// The footer abbreviates the unlock chord: "^q", not "Ctrl+q" (87d2b24
// shipped this test red against the new spelling; fixed 2026-08-24).
const FOOTER_TERMINAL_LOCKED: &str = "^q: panels";

struct TuiHarness {
    writer: Box<dyn Write + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    output: Arc<Mutex<Vec<u8>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    runtime_dir: PathBuf,
    data_dir: PathBuf,
    _repos: tempfile::TempDir,
}

impl TuiHarness {
    fn spawn() -> Self {
        Self::spawn_with_env(&[])
    }

    /// `spawn`, plus environment overrides for the TUI process — used to put
    /// a stub `gh` on PATH so the pull-request row can be driven without a
    /// GitHub account.
    fn spawn_with_env(extra_env: &[(&str, String)]) -> Self {
        // Socket paths must stay under SUN_LEN (~104 bytes) — keep the
        // runtime dir short. Tests share one process, so a per-harness
        // sequence keeps each test on its own daemon.
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        // Honor TMPDIR so verification can keep all daemon artifacts inside
        // its isolated scratch directory. Short leaves keep Unix sockets valid.
        let runtime_dir = std::env::temp_dir().join(format!("nr{pid}-{seq}"));
        let data_dir = std::env::temp_dir().join(format!("nd{pid}-{seq}"));
        let _ = std::fs::remove_dir_all(&runtime_dir);
        let _ = std::fs::remove_dir_all(&data_dir);
        let repos = tempfile::tempdir().unwrap();
        let (writer, parser, output, child) =
            launch(&runtime_dir, &data_dir, repos.path(), &[], extra_env);
        Self {
            writer,
            parser,
            output,
            child,
            runtime_dir,
            data_dir,
            _repos: repos,
        }
    }

    /// Replace the TUI with a fresh one launched with `args`, on the same
    /// daemon and repos.
    fn relaunch(&mut self, args: &[&str]) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let (writer, parser, output, child) = launch(
            &self.runtime_dir,
            &self.data_dir,
            self._repos.path(),
            args,
            &[],
        );
        self.writer = writer;
        self.parser = parser;
        self.output = output;
        self.child = child;
    }

    /// A committed git repo named `name` (fresh `git init` + one commit —
    /// worktrees need a HEAD to branch from).
    fn make_repo(&self, name: &str) -> PathBuf {
        let repo = self._repos.path().join(name);
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed in {}", repo.display());
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@nebula.dev"]);
        git(&["config", "user.name", "nebula-test"]);
        std::fs::write(repo.join(".keep"), "").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "init"]);
        repo
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    fn type_str(&mut self, s: &str) {
        self.send(s.as_bytes());
    }

    fn screen_text(&self) -> String {
        let parser = self.parser.lock().unwrap();
        screen_to_text(parser.screen())
    }

    /// Poll the rendered screen until `pred` holds; panic with a full screen
    /// dump on timeout.
    fn wait_for(&self, what: &str, pred: impl Fn(&vt100::Screen) -> bool) {
        if poll_until(WAIT, || {
            pred(self.parser.lock().unwrap().screen()).then_some(())
        })
        .is_some()
        {
            return;
        }
        let tui_log =
            std::fs::read_to_string(self.data_dir.join("state/tui.log")).unwrap_or_default();
        let tail: String = tui_log
            .lines()
            .rev()
            .take(60)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "timed out waiting for: {what}\n--- screen ---\n{}\n--- tui.log tail ---\n{tail}",
            self.screen_text()
        );
    }

    /// Wait until the raw PTY output, every byte since launch, contains
    /// `needle` — for text the screen shows only for an instant.
    fn wait_for_output(&self, needle: &str) {
        let written = poll_until(WAIT, || {
            String::from_utf8_lossy(&self.output.lock().unwrap())
                .contains(needle)
                .then_some(())
        });
        assert!(written.is_some(), "the TUI never wrote {needle:?}");
    }

    fn wait_for_text(&self, needle: &str) {
        self.wait_for(&format!("text {needle:?}"), |s| {
            screen_to_text(s).contains(needle)
        });
    }

    fn wait_for_gone(&self, needle: &str) {
        self.wait_for(&format!("text {needle:?} to disappear"), |s| {
            !screen_to_text(s).contains(needle)
        });
    }

    /// Wait until `needle`'s own cells render with the selection fill (the
    /// raised `sel_bg` / `sel_bg_dim` background bar); fill elsewhere on the
    /// row does not count.
    fn wait_for_selected(&self, needle: &str) {
        self.wait_for(&format!("row {needle:?} selected (filled)"), |s| {
            row_is_selected(s, needle)
        });
    }

    fn wait_for_dot(&self, needle: &str, color: vt100::Color) {
        self.wait_for(&format!("sessions dot of {needle:?} = {color:?}"), |s| {
            sessions_dot(s, needle) == Some(color)
        });
    }

    /// Wait until `needle` no longer appears inside the Sessions panel's
    /// column band. Screen-wide checks would false-positive on the terminal
    /// pane title, which keeps naming the attached session while browsing
    /// other worktrees/projects.
    fn wait_for_sessions_row_gone(&self, needle: &str) {
        self.wait_for(&format!("sessions row {needle:?} to disappear"), |s| {
            !sessions_panel_contains(s, needle)
        });
    }
}

#[allow(clippy::type_complexity)]
fn launch(
    runtime_dir: &Path,
    data_dir: &Path,
    cwd: &Path,
    args: &[&str],
    extra_env: &[(&str, String)],
) -> (
    Box<dyn Write + Send>,
    Arc<Mutex<vt100::Parser>>,
    Arc<Mutex<Vec<u8>>>,
    Box<dyn portable_pty::Child + Send + Sync>,
) {
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_nebula"));
    cmd.args(args);
    cmd.env("NEBULA_RUNTIME_DIR", runtime_dir);
    cmd.env("NEBULA_DATA_DIR", data_dir);
    cmd.env("NEBULA_AGENT_CMD", "/bin/sh"); // stand-in for claude
    cmd.env("NEBULA_WORKTREE_SYNC_MS", "100"); // fast external-change pickup
    cmd.env("NEBULA_LOG", "debug");
    cmd.env("SHELL", "/bin/sh");
    cmd.env("TERM", "xterm-256color");
    // Agent/CI shells often export NO_COLOR; crossterm then strips the
    // reverse/bold attrs wait_for_selected relies on.
    cmd.env_remove("NO_COLOR");
    cmd.env_remove("FORCE_COLOR");
    for k in LIVE_SESSION_ENV {
        cmd.env_remove(k);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.cwd(cwd);
    let child = pty.slave.spawn_command(cmd).unwrap();
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().unwrap();
    let writer = pty.master.take_writer().unwrap();
    // Keep the master alive for the whole test (dropping it hangs up the
    // TUI's tty); leak is fine in a test process.
    std::mem::forget(pty.master);

    let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
    let output = Arc::new(Mutex::new(Vec::new()));
    {
        let parser = parser.clone();
        let output = output.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                parser.lock().unwrap().process(&buf[..n]);
                output.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }

    (writer, parser, output, child)
}

/// Polls `probe` every 50ms until it yields a value; `None` once `timeout` passes.
#[expect(
    clippy::disallowed_methods,
    reason = "the one poll loop: every wait in this file goes through it"
)]
fn poll_until<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

impl Drop for TuiHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // Stop the auto-spawned daemon and clean the short-lived dirs.
        let _ = nebula_cmd()
            .arg("kill")
            .env("NEBULA_RUNTIME_DIR", &self.runtime_dir)
            .env("NEBULA_DATA_DIR", &self.data_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
        let _ = std::fs::remove_dir_all(&self.data_dir);
    }
}

fn screen_to_text(screen: &vt100::Screen) -> String {
    let (rows, cols) = screen.size();
    let mut out = String::new();
    for row in 0..rows {
        for col in 0..cols {
            match screen.cell(row, col) {
                Some(cell) => {
                    let contents = cell.contents();
                    if contents.is_empty() {
                        out.push(' ');
                    } else {
                        out.push_str(contents);
                    }
                }
                None => out.push(' '),
            }
        }
        out.push('\n');
    }
    out
}

/// True when `needle` appears within the Sessions panel's columns
/// (mirrors DEFAULT_PANEL_WIDTHS in nebula-tui/src/app.rs; the harness
/// starts with a fresh DB, so the panels are at their default widths).
fn sessions_panel_contains(screen: &vt100::Screen, needle: &str) -> bool {
    const SESSIONS_X: u16 = 20 + 22;
    const SESSIONS_W: u16 = 32;
    let (rows, cols) = screen.size();
    let right = SESSIONS_X.saturating_add(SESSIONS_W).min(cols);
    for row in 0..rows {
        let mut line = String::new();
        for col in SESSIONS_X..right {
            let contents = screen
                .cell(row, col)
                .map(|c| c.contents())
                .unwrap_or_default();
            if contents.is_empty() {
                line.push(' ');
            } else {
                line.push_str(contents);
            }
        }
        if line.contains(needle) {
            return true;
        }
    }
    false
}

/// The colour of the status dot on the Sessions row that names `needle`.
fn sessions_dot(screen: &vt100::Screen, needle: &str) -> Option<vt100::Color> {
    const SESSIONS_X: u16 = 20 + 22;
    const SESSIONS_W: u16 = 32;
    let (rows, cols) = screen.size();
    let right = SESSIONS_X.saturating_add(SESSIONS_W).min(cols);
    (0..rows).find_map(|row| {
        let cells: Vec<_> = (SESSIONS_X..right)
            .filter_map(|col| screen.cell(row, col))
            .collect();
        let line: String = cells.iter().map(|c| c.contents()).collect();
        if !line.contains(needle) {
            return None;
        }
        let dot = cells.iter().find(|c| matches!(c.contents(), "●" | "○"))?;
        Some(dot.fgcolor())
    })
}

fn row_is_selected(screen: &vt100::Screen, needle: &str) -> bool {
    let (rows, cols) = screen.size();
    for row in 0..rows {
        let mut line = String::new();
        for col in 0..cols {
            let contents = screen
                .cell(row, col)
                .map(|c| c.contents())
                .unwrap_or_default();
            if contents.is_empty() {
                line.push(' ');
            } else {
                line.push_str(contents);
            }
        }
        // Selection paints the row with the raised fill: indexed 237 in
        // the focused panel, 235 in unfocused ones (theme sel_bg /
        // sel_bg_dim). Only the needle's own cells count: a popup row
        // shares its screen line with whatever panel row lies beneath.
        let filled = line.match_indices(needle).any(|(at, _)| {
            let start = line[..at].chars().count() as u16;
            let end = start + needle.chars().count() as u16;
            (start..end).any(|col| {
                matches!(
                    screen.cell(row, col).map(|c| c.bgcolor()),
                    Some(vt100::Color::Idx(237)) | Some(vt100::Color::Idx(235))
                )
            })
        });
        if filled {
            return true;
        }
    }
    false
}

#[test]
fn row_is_selected_accepts_a_filled_needle_after_an_unfilled_copy() {
    let mut parser = vt100::Parser::new(2, 40, 0);
    parser.process(b"Hide \x1b[48;5;237mHide\x1b[0m");
    assert!(row_is_selected(parser.screen(), "Hide"));
    parser.process(b"\r\x1b[48;5;237m\x1b[2K\x1b[0mHide");
    assert!(!row_is_selected(parser.screen(), "Hide"));
}

fn add_project(tui: &mut TuiHarness, path: &Path, expect_name: &str) {
    tui.send(b"n");
    tui.wait_for_text("Add project");
    tui.type_str(&path.to_string_lossy());
    tui.send(b"\r");
    // The prompt must close before asserting panel content — otherwise the
    // overlay's own text can satisfy the wait (stale-frame race).
    tui.wait_for_gone("Add project");
    tui.wait_for_text(expect_name);
}

fn repo_git(repo: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed in {}", repo.display());
}

fn create_worktree(tui: &mut TuiHarness, branch: &str) {
    tui.send(b"n");
    // `n` on the Worktrees panel asks worktree-or-branch first; Enter on
    // the hovered default (Worktree) chains into the name prompt.
    tui.wait_for_text("New worktree or branch");
    tui.send(b"\r");
    tui.wait_for_gone("or branch");
    tui.wait_for_text("New worktree");
    tui.type_str(branch);
    tui.send(b"\r");
    // The name step chains into the base-branch picker; Enter on the
    // hovered default base keeps the old one-step behavior.
    tui.wait_for_text("Base branch");
    tui.send(b"\r");
    tui.wait_for_gone("Base branch");
    tui.wait_for_text(branch);
    // A fresh worktree auto-focuses the sessions panel (so `n` starts an
    // agent); hop back to Worktrees so callers stay panel-stable.
    tui.wait_for_text(FOOTER_SESSIONS);
    tui.send(b"\x1b[D"); // ← (h is the hosts picker)
    tui.wait_for_text(FOOTER_WORKTREES);
}

#[test]
fn tui_projects_worktrees_agents_navigation() {
    let mut tui = TuiHarness::spawn();
    let alpha = tui.make_repo("alpha-proj");
    let beta = tui.make_repo("beta-proj");

    // ---- boot: empty state, Projects focused ----
    tui.wait_for_text("no projects yet");
    tui.wait_for_text(FOOTER_PROJECTS);

    // ---- add the first project via bash-style Tab completion ----
    // Type the repos dir + "al", press Tab: unique match completes to
    // "alpha-proj/" on screen.
    tui.send(b"n");
    tui.wait_for_text("Add project");
    tui.type_str(&format!("{}/al", alpha.parent().unwrap().display()));
    tui.send(b"\t");
    tui.wait_for_text("alpha-proj/");
    tui.send(b"\r");
    tui.wait_for_gone("Add project");
    tui.wait_for_text("alpha-proj");
    tui.wait_for_text("main ⌂ primary"); // the primary checkout, named by its branch

    // The live directory browser: typing "…/T/.tmpX/" lists both repos as
    // rows (no Tab needed), then Esc cancels.
    tui.send(b"n");
    tui.wait_for_text("Add project");
    tui.type_str(&format!("{}/", alpha.parent().unwrap().display()));
    tui.wait_for_text("alpha-proj/");
    tui.wait_for_text("beta-proj/");
    tui.send(&[0x1b]); // Esc
    tui.wait_for_gone("Add project");

    // ---- second project typed the plain way ----
    add_project(&mut tui, &beta, "beta-proj");
    // First project stays selected; its rows render reversed in the focused panel.
    tui.wait_for_selected("alpha-proj");

    // ---- Tab cycles focus across the panes and back ----
    tui.send(b"\t");
    tui.wait_for_text(FOOTER_WORKTREES);
    tui.send(b"\t");
    tui.wait_for_text(FOOTER_SESSIONS);
    tui.send(b"\t");
    // Terminal pane focused with nothing attached: no panel footer, no lock.
    tui.wait_for_gone(FOOTER_SESSIONS);
    tui.send(b"\t");
    tui.wait_for_text(FOOTER_PROJECTS); // wrapped around

    // ---- Enter drills from Projects into Worktrees ----
    tui.send(b"\r");
    tui.wait_for_text(FOOTER_WORKTREES);

    // ---- create two worktrees on alpha-proj ----
    create_worktree(&mut tui, "feat-a");
    create_worktree(&mut tui, "feat-b");

    // The worktree dirs exist on disk, as siblings of the repo.
    let wt_root = alpha.parent().unwrap().join("alpha-proj-worktrees");
    assert!(wt_root.join("feat-a").exists(), "feat-a worktree on disk");
    assert!(wt_root.join("feat-b").exists(), "feat-b worktree on disk");

    // ---- a fresh worktree is auto-selected: feat-b was created last ----
    tui.wait_for_selected("feat-b");

    // ---- j/k still walks the selection: feat-b → feat-a → main → feat-a ----
    tui.send(b"k");
    tui.wait_for_selected("feat-a");
    tui.send(b"k");
    tui.wait_for_selected("⌂ primary");
    tui.send(b"j");
    tui.wait_for_selected("feat-a");

    // ---- Enter shows the sessions (agents) panel for feat-a ----
    tui.send(b"\r");
    tui.wait_for_text(FOOTER_SESSIONS);

    // ---- create an agent: kind picker → name prompt, auto-attaches ----
    tui.send(b"n");
    tui.wait_for_text("New session"); // Claude/Codex/Cursor/Terminal picker
    tui.send(b"\r"); // pick the default (Claude)
    tui.wait_for_gone("New session");
    tui.wait_for_text("New agent");
    tui.send(b"\r"); // empty input falls back to "agent-1"
    tui.wait_for_gone("New agent");
    tui.wait_for_text("agent-1"); // now provably the sessions-panel row
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED); // auto-attach locks input

    // ---- Ctrl+q (raw byte 0x11, what every emulator sends) escapes back ----
    tui.send(&[0x11]);
    tui.wait_for_text(FOOTER_SESSIONS);

    // ---- Tab merely focuses the live pane (no lock); Enter locks it ----
    tui.send(b"\t");
    tui.wait_for_text(FOOTER_TERMINAL_FOCUSED);
    tui.send(b"\r");
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED);
    tui.send(&[0x1d]); // Ctrl+] fallback (legacy byte, what Terminal.app sends)
    tui.wait_for_text(FOOTER_SESSIONS);

    // ---- Shift+T: a shell terminal in the worktree dir, auto-attached ----
    tui.send(b"T");
    tui.wait_for_text("❯ term-1");
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED);
    tui.send(&[0x11]); // Ctrl+q back to panels
    tui.wait_for_text(FOOTER_SESSIONS);

    // ---- sessions are per-worktree: feat-b has no agent-1 ----
    tui.send(b"\x1b[D"); // ← back to Worktrees (feat-a still selected)
    tui.wait_for_text(FOOTER_WORKTREES);
    tui.send(b"j"); // feat-b
    tui.wait_for_selected("feat-b");
    tui.wait_for_sessions_row_gone("agent-1");
    tui.send(b"k"); // back to feat-a
    tui.wait_for_selected("feat-a");
    tui.wait_for_text("agent-1");

    // ---- toggling projects swaps the whole worktree panel ----
    tui.send(b"\x1b[D"); // ← focus Projects
    tui.wait_for_text(FOOTER_PROJECTS);
    tui.send(b"j"); // select beta-proj
    tui.wait_for_selected("beta-proj");
    tui.wait_for_gone("feat-a"); // beta has only its main checkout
    tui.wait_for_text("⌂ primary");
    tui.wait_for_sessions_row_gone("agent-1");

    // ---- the root row tracks live branch switches, no restart needed ----
    repo_git(&beta, &["checkout", "-b", "hotfix"]);
    // The row is named by its branch, nested under the branch it was cut
    // from (`main` stays as a plain branch row above it).
    tui.wait_for_text("hotf"); // the name may truncate in the narrow panel
    tui.wait_for_text("○ main (branch)");
    tui.send(b"k"); // back to alpha-proj
    tui.wait_for_selected("alpha-proj");
    tui.wait_for_text("feat-a");
    tui.wait_for_text("feat-b");

    // Switching back restores the remembered context: feat-a is the
    // selected worktree again and its agent is back without re-drilling.
    tui.wait_for_text("agent-1");
    tui.send(b"\r"); // Projects → Worktrees
    tui.wait_for_text(FOOTER_WORKTREES);
    tui.wait_for_selected("feat-a");

    // ---- clean quit ----
    tui.send(b"q");
    let exited = poll_until(Duration::from_secs(5), || tui.child.try_wait().unwrap());
    assert!(
        exited.is_some(),
        "TUI did not exit after q\n--- screen ---\n{}",
        tui.screen_text()
    );
}

#[test]
fn tui_slow_agent_shows_starting_until_first_output() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let gate = fixture.path().join("ready");
    let script = fixture.path().join("slow-agent");
    std::fs::write(&script, format!(
        "#!/bin/sh\nwhile [ ! -f '{}' ]; do sleep 0.05; done\nprintf 'ATTACH_READY_MARKER\\r\\n'\nexec /bin/cat\n", gate.display()
    )).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut tui = TuiHarness::spawn_with_env(&[("NEBULA_AGENT_CMD", script.display().to_string())]);
    let repo = tui.make_repo("slow-start");
    tui.wait_for_text("no projects yet");
    add_project(&mut tui, &repo, "slow-start");
    tui.send(b"\t");
    tui.wait_for_text(FOOTER_WORKTREES);
    tui.send(b"\r");
    tui.wait_for_text(FOOTER_SESSIONS);
    tui.send(b"n");
    tui.wait_for_text("New session");
    tui.send(b"\r");
    tui.wait_for_text("New agent");
    tui.send(b"\r");
    tui.wait_for_gone("New agent");
    tui.wait_for_text("Starting claude…");
    assert!(!tui.screen_text().contains("ATTACH_READY_MARKER"));
    std::fs::write(gate, "ready").unwrap();
    tui.wait_for_text("ATTACH_READY_MARKER");
    tui.wait_for_gone("Starting claude…");
}

/// The replay agent plays a Claude turn that stops on a question through the
/// hooks a real session fires; the Sessions dot follows it the whole way.
#[test]
fn tui_sessions_dot_follows_a_replayed_turn_through_a_question() {
    const YELLOW: vt100::Color = vt100::Color::Idx(3);
    const RED: vt100::Color = vt100::Color::Idx(1);
    const GREEN: vt100::Color = vt100::Color::Idx(2);
    let replay = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/replay");
    let agent = format!(
        "{} {}",
        replay.join("agent").display(),
        replay.join("ask.replay").display()
    );
    let mut tui = TuiHarness::spawn_with_env(&[("NEBULA_AGENT_CMD", agent)]);
    let repo = tui.make_repo("replay");
    tui.wait_for_text("no projects yet");
    add_project(&mut tui, &repo, "replay");
    tui.send(b"\t");
    tui.wait_for_text(FOOTER_WORKTREES);
    tui.send(b"\r");
    tui.wait_for_text(FOOTER_SESSIONS);
    tui.send(b"n");
    tui.wait_for_text("New session");
    tui.send(b"\r");
    tui.wait_for_text("New agent");
    tui.send(b"\r");
    tui.wait_for_gone("New agent");
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED);

    tui.type_str("tidy the readme\r");
    tui.wait_for_dot("agent-1", YELLOW);
    tui.type_str("\r");
    tui.wait_for_dot("agent-1", RED);
    tui.wait_for_text("Apply the change?");
    tui.type_str("yes\r");
    tui.wait_for_dot("agent-1", YELLOW);
    tui.type_str("\r");
    tui.wait_for_dot("agent-1", GREEN);
    tui.wait_for_text("Done.");
}

/// A silent preview service must never prevent the real attach. The only
/// substitute is the daemon socket; input, event loop, timer and renderer are real.
#[test]
fn tui_remote_preview_deadline_attaches_when_the_reply_never_arrives() {
    use nebula_core::{
        codec::{read_frame, write_frame},
        ClientRequest, ServerEvent, PROTOCOL_VERSION,
    };
    let runtime = tempfile::tempdir().unwrap();
    let listener =
        std::os::unix::net::UnixListener::bind(runtime.path().join("daemon.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
            tokio::time::timeout(WAIT, async move {
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut requested = None;
                let mut attached = false;
                while let Some(req) = read_frame::<ClientRequest, _>(&mut stream).await.unwrap() {
                    match req {
                        ClientRequest::Hello { .. } => write_frame(&mut stream, &ServerEvent::HelloOk {
                            protocol_version: PROTOCOL_VERSION, daemon_pid: std::process::id(),
                        }).await.unwrap(),
                        ClientRequest::Subscribe => {
                            let snapshot: ServerEvent = serde_json::from_value(serde_json::json!({"Snapshot": {
                                "workspaces": [{"id":"default", "name":"Default"}], "active_workspace":"default",
                                "projects": [{"id":"p", "name":"remote", "repo_path":"/remote", "sort_order":0,
                                    "divider_after":false, "divider_before":false, "host":"findl"}],
                                "worktrees": [{"id":"w", "project_id":"p", "path":"/remote", "branch":"main", "is_main":true, "sort_order":0}],
                                "agents": [{"id":"a", "worktree_id":"w", "name":"remote-agent", "status":"finished", "archived":false, "sort_order":0, "alive":true}],
                                "terminals":[], "pr_seen":[],
                                "ui_state":"{\"project\":\"p\",\"worktree\":\"w\",\"session_agent\":\"a\"}"
                            }})).unwrap();
                            write_frame(&mut stream, &snapshot).await.unwrap();
                        }
                        ClientRequest::ReadSession {req_id, ..} => {
                            #[expect(clippy::disallowed_methods, reason = "the elapsed time to the fallback attach is the asserted preview deadline")]
                            let asked_at = Instant::now();
                            requested = Some((req_id, asked_at)); // deliberately no reply
                        }
                        ClientRequest::Attach {session, ..} => {
                            assert!(!attached, "preview deadline must issue only one attach");
                            attached = true;
                            let (req_id, start) = requested.expect("compact preview is requested first");
                            result_tx.send(start.elapsed()).unwrap();
                            // A delayed error must not flash once the fallback has won.
                            write_frame(&mut stream, &ServerEvent::Error {req_id: Some(req_id), message: "late preview failure".into()}).await.unwrap();
                            write_frame(&mut stream, &ServerEvent::Scrollback {session, base_seq: 0, data: b"DEADLINE_READY".to_vec()}).await.unwrap();
                        }
                        _ => {}
                    }
                }
            }).await.unwrap();
        });
    });
    let mut tui =
        TuiHarness::spawn_with_env(&[("NEBULA_RUNTIME_DIR", runtime.path().display().to_string())]);
    tui.wait_for_text("remote");
    tui.send(b"\t");
    tui.wait_for_text(FOOTER_WORKTREES);
    tui.send(b"\r");
    tui.wait_for_text(FOOTER_SESSIONS);
    tui.send(b"\r");
    let elapsed = result_rx
        .recv_timeout(WAIT)
        .expect("event loop must send Attach without a preview reply");
    assert!(
        elapsed >= Duration::from_millis(100) && elapsed < Duration::from_secs(2),
        "bounded preview deadline: {elapsed:?}"
    );
    tui.wait_for_text("DEADLINE_READY");
    assert!(!tui.screen_text().contains("late preview failure"));
    drop(tui);
    peer.join().unwrap();
}

#[test]
fn tui_help_modal_grouped_keymap() {
    let mut tui = TuiHarness::spawn();
    tui.wait_for_text("no projects yet");

    // The grouped two-column keymap: every section header on screen at
    // once (the old single list clipped its tail on short terminals).
    tui.send(b"?");
    tui.wait_for_text("NAVIGATE & SEARCH");
    tui.wait_for_text("PROJECTS");
    tui.wait_for_text("WORKTREES");
    tui.wait_for_text("SESSIONS");
    tui.wait_for_text("TERMINAL & MOUSE");
    tui.wait_for_text("GENERAL");

    tui.send(&[0x1b]); // Esc closes
    tui.wait_for_gone("NAVIGATE & SEARCH");
}

/// Notes, todos and saved links are retired: their old keys open nothing in
/// any panel, so `?` right after them still reaches the help overlay.
#[test]
fn tui_retired_note_todo_and_link_shortcuts_do_nothing() {
    let mut tui = TuiHarness::spawn();
    let repo = tui.make_repo("retired-proj");
    tui.wait_for_text("no projects yet");
    add_project(&mut tui, &repo, "retired-proj");
    tui.wait_for_text("⌂ primary");

    for panel in ["projects", "worktrees", "sessions"] {
        tui.send(b"eEl?");
        tui.wait_for_text("NAVIGATE & SEARCH");
        let screen = tui.screen_text();
        for overlay in ["Notes —", "Todos —", "Add link"] {
            assert!(
                !screen.contains(overlay),
                "{overlay:?} opened in the {panel} panel:\n{screen}"
            );
        }
        tui.send(&[0x1b]);
        tui.wait_for_gone("NAVIGATE & SEARCH");
        tui.send(b"\r");
    }
}

/// The pull request nebula finds on the branch leads the LINKS group. A
/// stub `gh` on PATH stands in for GitHub: the real one is asked for exactly
/// this JSON (`gh pr view --json number,url,title,state,isDraft`).
#[test]
fn tui_pull_request_row_leads_the_links_group() {
    let stub_bin = tempfile::tempdir().unwrap();
    let gh = stub_bin.path().join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nprintf '%s' '{\"isDraft\":false,\"number\":7,\"state\":\"OPEN\",\"title\":\"Attach links\",\"url\":\"https://github.com/o/r/pull/7\"}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        stub_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut tui = TuiHarness::spawn_with_env(&[("PATH", path)]);
    let repo = tui.make_repo("pr-proj");
    tui.wait_for_text("no projects yet");
    add_project(&mut tui, &repo, "pr-proj");
    tui.wait_for_text("⌂ primary");

    // The lookup rides the git poll, so the tab shows up on its own (it
    // is row 0, so it is selected and carries its full label).
    tui.wait_for_text("#7 Attach links");

    // It is not a stored row: d and r say so instead of opening a dialog.
    tui.send(b"\r"); // Projects → Worktrees
    tui.wait_for_text(FOOTER_WORKTREES);
    tui.send(b"\r"); // Worktrees → Sessions
    tui.wait_for_selected("#7 Attach links");
    tui.send(b"d");
    tui.wait_for_text("can't be deleted");
    tui.send(b"r");
    tui.wait_for_text("can't be renamed");
}

#[test]
fn tui_git_diff_modal() {
    let mut tui = TuiHarness::spawn();
    let repo = tui.make_repo("diff-proj");

    tui.wait_for_text("no projects yet");
    add_project(&mut tui, &repo, "diff-proj");
    // The root worktree row must exist before g has anything to diff.
    tui.wait_for_text("⌂ primary");

    // Dirty the checkout: one tracked modification, one untracked file.
    std::fs::write(repo.join(".keep"), "tracked change\n").unwrap();
    std::fs::write(repo.join("hello.txt"), "hello world\n").unwrap();

    // The worktree panel's bottom badge picks the changes up on its own
    // poll — no keypress in between.
    tui.wait_for_text("+2 files");

    // ---- open the modal; the selected file's diff renders ----
    tui.send(b"g");
    tui.wait_for_text("Files (2)");
    // Status is path-ordered, so .keep (modified) is selected first.
    tui.wait_for_selected(".keep");
    tui.wait_for_text("+tracked change");

    // ---- Ctrl+r marks .keep reviewed: it sinks below hello.txt and the
    // selection auto-advances to the next file, loading its diff ----
    tui.send(&[0x12]);
    tui.wait_for_text("· 1✓"); // files-panel title counts the mark
    tui.wait_for_selected("hello.txt");
    tui.wait_for_text("+hello world");

    // ---- Down reaches the reviewed zone; Ctrl+r unmarks .keep, which
    // pops back to the top of the list and stays selected ----
    tui.send(b"\x1b[B"); // Down
    tui.wait_for_selected(".keep");
    tui.wait_for_text("+tracked change");
    tui.send(&[0x12]);
    tui.wait_for_gone("· 1✓");

    // ---- arrow to the untracked file ----
    tui.send(b"\x1b[B"); // Down
    tui.wait_for_selected("hello.txt");
    tui.wait_for_text("+hello world");

    // ---- type-to-filter narrows the list and reselects the top match ----
    tui.type_str("kee");
    tui.wait_for_text("Files (1/2)");
    tui.wait_for_selected(".keep");
    tui.wait_for_text("+tracked change");
    tui.send(&[0x1b]); // first Esc clears the filter, not the modal
    tui.wait_for_text("Files (2)");

    // ---- the modal blocks other interaction ----
    // n would open "Add project" from the Projects panel; inside the modal it
    // feeds the filter instead (verified after close — stale-frame convention).
    tui.send(b"n");
    tui.wait_for_text("no matches");
    tui.send(&[0x1b]); // Esc clears the filter…
    tui.wait_for_text("Files (2)"); // (also keeps the two Escs from coalescing)
    tui.send(&[0x1b]); // …and the second closes the modal
    tui.wait_for_gone("Files (2)");
    tui.wait_for_text(FOOTER_PROJECTS);
    assert!(
        !tui.screen_text().contains("Add project"),
        "modal swallowed n\n--- screen ---\n{}",
        tui.screen_text()
    );

    // ---- clean tree opens an empty viewer; ^g picks the commit ----
    repo_git(&repo, &["add", "."]);
    repo_git(&repo, &["commit", "-m", "wip"]);
    // The commit empties the badge on the next poll.
    tui.wait_for_gone("+2 files");
    tui.send(b"g");
    tui.wait_for_text("working tree clean");
    tui.send(b"\x07"); // ^g
    tui.wait_for_text("Compare with");
    tui.send(b"\x1b[B"); // ↓ onto the "wip" commit
    tui.send(b"\r");
    tui.wait_for_text("Files (2)");
    tui.send(b"\x1b");
    tui.wait_for_gone("Files (2)");
}

/// Killing the daemon out from under a running TUI (what `nebula kill`
/// from another shell does) must not strand it on the "✗ disconnected"
/// footer forever: the TUI re-dials, boots a fresh daemon, re-subscribes,
/// and the panels come back live with the persisted tree.
#[test]
fn tui_reconnects_after_the_daemon_is_killed() {
    let mut tui = TuiHarness::spawn();
    let repo = tui.make_repo("proj");
    add_project(&mut tui, &repo, "proj");

    let ok = nebula_cmd()
        .arg("kill")
        .env("NEBULA_RUNTIME_DIR", &tui.runtime_dir)
        .env("NEBULA_DATA_DIR", &tui.data_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(ok, "nebula kill failed");
    tui.wait_for_text("✗ disconnected");

    // Auto-reconnect: a fresh daemon comes up, the Snapshot restores the
    // tree, and the footer stops flagging the connection.
    tui.wait_for_gone("✗ disconnected");
    tui.wait_for_text("proj");
}

/// `⌘K r` (typed here as ⇧P + the alias): the TUI quits and re-execs the
/// binary on disk in the SAME window — the process survives, the tree
/// comes back, and the reloaded UI still answers input.
#[test]
fn tui_reloads_in_place_from_the_palette() {
    let mut tui = TuiHarness::spawn();
    let repo = tui.make_repo("proj");
    add_project(&mut tui, &repo, "proj");

    tui.send(b"P");
    tui.wait_for_text("Commands");
    tui.type_str("r");
    tui.send(b"\r");

    // exec() keeps the pid; the old image prints this on its way out, so the
    // tree drawn after it is the fresh image's. Its help modal proves it is alive.
    tui.wait_for_output("nebula: reloading…");
    tui.wait_for_text("proj");
    tui.send(b"?");
    tui.wait_for_text("NAVIGATE & SEARCH");
}

/// Session identity a test run inherits when started from inside a live
/// nebula session; the isolated TUI, its daemon and CLI calls must not carry it.
const LIVE_SESSION_ENV: [&str; 4] = [
    "NEBULA_AGENT_ID",
    "NEBULA_API_URL",
    "NEBULA_API_TOKEN",
    "CLAUDE_CODE_EXPERIMENTAL_CODE_MODE",
];

fn nebula_cmd() -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"));
    for k in LIVE_SESSION_ENV {
        cmd.env_remove(k);
    }
    cmd
}

fn nebula_cli(tui: &TuiHarness, args: &[&str]) -> std::process::Output {
    nebula_cmd()
        .args(args)
        .env("NEBULA_RUNTIME_DIR", &tui.runtime_dir)
        .env("NEBULA_DATA_DIR", &tui.data_dir)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// Type into the attached pane and wait for the session's own agent id —
/// proof that the pane holds that session's live PTY. `tag` keeps an echo
/// replayed from an earlier visit from satisfying the wait.
fn assert_typing_reaches(tui: &mut TuiHarness, tag: &str, id: &str) {
    tui.type_str(&format!("echo {tag}-$NEBULA_AGENT_ID\r"));
    tui.wait_for_text(&format!("{tag}-{id}"));
}

/// `nebula open --session` from a launcher (no terminal): a running TUI
/// takes the request and attaches the session; with none running the
/// command says so with exit 3 instead of starting a TUI it has no
/// terminal for. From a terminal it starts the TUI landed on the session.
#[test]
fn open_session_attaches_it_in_the_running_tui() {
    let mut tui = TuiHarness::spawn();
    let repo = tui.make_repo("alpha-proj");
    add_project(&mut tui, &repo, "alpha-proj");
    let mut ids = Vec::new();
    for name in ["first", "second"] {
        let out = nebula_cli(
            &tui,
            &[
                "agent",
                "new",
                "--project",
                "alpha-proj",
                "--worktree",
                "main",
                "--name",
                name,
            ],
        );
        assert!(out.status.success(), "agent new {name}: {out:?}");
        let row: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        ids.push(row["id"].as_str().unwrap().to_owned());
    }
    tui.wait_for_text("second");

    let unknown = nebula_cli(&tui, &["open", "--session", "nope"]);
    assert_eq!(unknown.status.code(), Some(1), "{unknown:?}");
    assert_eq!(
        String::from_utf8_lossy(&unknown.stderr).trim(),
        "Error: no session \"nope\" (pass a name from `nebula agent list --all` or its id)"
    );

    let by_name = nebula_cli(
        &tui,
        &[
            "open",
            "--session",
            "second",
            "--project",
            "alpha-proj",
            "--no-focus",
        ],
    );
    assert_eq!(by_name.status.code(), Some(0), "{by_name:?}");
    let landed: serde_json::Value = serde_json::from_slice(&by_name.stdout).unwrap();
    assert_eq!(
        landed,
        serde_json::json!({
            "landed": "tui",
            "tui_pid": tui.child.process_id().unwrap(),
            "id": ids[1],
            "name": "second",
            "project": "alpha-proj",
            "host": null,
            "focused": false,
        })
    );
    tui.wait_for_selected("second");
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED);
    assert_typing_reaches(&mut tui, "t1", &ids[1]);

    // A shell split beside it takes the input lock; reopening the session
    // must hand the lock back to the session's own pane.
    tui.send(&[0x11]);
    tui.wait_for_text(FOOTER_SESSIONS);
    tui.send(b"|");
    tui.wait_for_text("Terminal (shell)");
    tui.send(b"\x1b[B\x1b[B\x1b[B\x1b[B\x1b[B\r");
    tui.wait_for_gone("Terminal (shell)");
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED);
    tui.type_str("echo split-$((6*7))-[$NEBULA_AGENT_ID]\r");
    tui.wait_for_text("split-42-[term:");
    let reopen = nebula_cli(&tui, &["open", "--session", &ids[1], "--no-focus"]);
    assert_eq!(reopen.status.code(), Some(0), "{reopen:?}");
    assert_typing_reaches(&mut tui, "t1b", &ids[1]);

    tui.send(&[0x11]);
    tui.wait_for_text(FOOTER_SESSIONS);
    let by_id = nebula_cli(&tui, &["open", "--session", &ids[0], "--no-focus"]);
    assert_eq!(by_id.status.code(), Some(0), "{by_id:?}");
    tui.wait_for_selected("first");
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED);
    assert_typing_reaches(&mut tui, "t2", &ids[0]);

    for args in [
        &["workspace", "add", "other"][..],
        &["workspace", "open", "other"],
    ] {
        let out = nebula_cli(&tui, args);
        assert!(out.status.success(), "{args:?}: {out:?}");
    }
    tui.wait_for_gone("alpha-proj");
    tui.send(&[0x11]);
    tui.wait_for_text(FOOTER_PROJECTS);
    let out_of_scope = nebula_cli(&tui, &["open", "--session", "second", "--no-focus"]);
    assert_eq!(out_of_scope.status.code(), Some(0), "{out_of_scope:?}");
    tui.wait_for_text("alpha-proj");
    tui.wait_for_selected("second");
    assert_typing_reaches(&mut tui, "t2b", &ids[1]);

    tui.send(&[0x11]);
    tui.wait_for_text(FOOTER_SESSIONS);
    tui.send(b"\x1b[D\x1b[D");
    tui.wait_for_text(FOOTER_PROJECTS);
    tui.send(b"m");
    tui.wait_for_text("Remove from list");
    tui.send(&b"\x1b[B".repeat(20));
    tui.wait_for_selected("Remove from list");
    tui.send(b"\x1b[A");
    tui.wait_for_selected("Hide");
    tui.send(b"\r");
    tui.wait_for_text("PROJECTS    ▾ 1");
    let hidden = nebula_cli(&tui, &["open", "--session", "first", "--no-focus"]);
    assert_eq!(hidden.status.code(), Some(0), "{hidden:?}");
    tui.wait_for_gone("▾ 1");
    assert_typing_reaches(&mut tui, "t2c", &ids[0]);
    let out = nebula_cli(&tui, &["workspace", "open", "other"]);
    assert!(out.status.success(), "{out:?}");

    tui.child.kill().unwrap();
    tui.child.wait().unwrap();
    let no_tui = nebula_cli(&tui, &["open", "--session", "first"]);
    assert_eq!(no_tui.status.code(), Some(3), "{no_tui:?}");
    assert!(no_tui.stdout.is_empty(), "{no_tui:?}");
    assert_eq!(
        String::from_utf8_lossy(&no_tui.stderr).trim(),
        "nebula: no nebula TUI is running — start one in a terminal with `nebula open --session "
            .to_owned()
            + &ids[0]
            + "`"
    );

    tui.relaunch(&["open", "--session", "second"]);
    tui.wait_for_selected("second");
    tui.wait_for_text(FOOTER_TERMINAL_LOCKED);
    assert_typing_reaches(&mut tui, "t3", &ids[1]);
}

/// A HOME whose `~/.local/bin` holds `nebula` (`remote`, else this build)
/// and a stub ssh that records its argv one word per line in `ssh-argv`,
/// then joins the words after the host with spaces and runs them as ssh
/// would. It exits 255 like an unreachable host with SSH_STUB_UNREACHABLE
/// set, and runs nothing with SSH_STUB_DRY set.
fn ssh_stub_home(remote: Option<&str>) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    match remote {
        Some(script) => {
            std::fs::write(bin.join("nebula"), script).unwrap();
            std::fs::set_permissions(bin.join("nebula"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        None => {
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_nebula"), bin.join("nebula")).unwrap()
        }
    }
    std::fs::write(
        bin.join("ssh"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n[ -n \"$SSH_STUB_UNREACHABLE\" ] && {{ echo 'ssh: connect to host laptop-b: Connection refused' >&2; exit 255; }}\n[ -n \"$SSH_STUB_DRY\" ] && exit 0\nwhile [ \"$1\" != -- ]; do shift; done\nshift 2\nexec /bin/sh -c \"$*\"\n",
            home.path().join("ssh-argv").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    home
}

fn open_on(
    home: &Path,
    runtime_dir: &Path,
    data_dir: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> std::process::Output {
    nebula_cmd()
        .arg("open")
        .args(args)
        .env("NEBULA_RUNTIME_DIR", runtime_dir)
        .env("NEBULA_DATA_DIR", data_dir)
        .env("HOME", home)
        .env(
            "PATH",
            format!(
                "{}:{}",
                home.join(".local/bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// The words the stub ssh was asked to run on HOST, after its PATH prefix.
fn remote_words(home: &Path, host: &str) -> Vec<String> {
    let argv = std::fs::read_to_string(home.join("ssh-argv")).unwrap_or_default();
    let lines: Vec<&str> = argv.lines().collect();
    assert!(lines.contains(&"BatchMode=yes"), "ssh argv: {argv:?}");
    let at = lines
        .iter()
        .position(|l| *l == host)
        .unwrap_or_else(|| panic!("no {host} in ssh argv: {argv:?}"));
    lines[at + 2..].iter().map(|l| l.to_string()).collect()
}

/// `--on HOST` runs the open on HOST over ssh: a stub ssh (the relay tests'
/// PATH override) runs the remote command line on this machine, where the
/// remote `~/.local/bin/nebula` is this build talking to the same TUI.
#[test]
fn open_on_host_returns_landed_focused() {
    let mut tui = TuiHarness::spawn();
    let repo = tui.make_repo("alpha proj");
    add_project(&mut tui, &repo, "alpha proj");
    let new = nebula_cli(
        &tui,
        &[
            "agent",
            "new",
            "--project",
            "alpha proj",
            "--worktree",
            "main",
            "--name",
            "far away",
        ],
    );
    assert!(new.status.success(), "{new:?}");
    let id = serde_json::from_slice::<serde_json::Value>(&new.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    tui.wait_for_text("far away");

    let home = ssh_stub_home(None);
    let (rt, data) = (tui.runtime_dir.clone(), tui.data_dir.clone());
    let session = ["--session", "far away", "--project", "alpha proj"];

    let bad = open_on(
        home.path(),
        &rt,
        &data,
        &[&session[..], &["--on", "bad host@localhost", "--no-focus"]].concat(),
        &[],
    );
    assert_eq!(bad.status.code(), Some(1), "{bad:?}");
    assert!(
        bad.stdout.is_empty(),
        "an invalid host never lands: {bad:?}"
    );

    let default = open_on(
        home.path(),
        &rt,
        &data,
        &[&session[..], &["--on", "laptop-b"]].concat(),
        &[("SSH_STUB_DRY", "1")],
    );
    assert_eq!(default.status.code(), Some(0), "{default:?}");
    assert_eq!(
        remote_words(home.path(), "laptop-b"),
        [
            "'nebula'",
            "'open'",
            "'--session=far away'",
            "'--project=alpha proj'"
        ]
    );

    let out = open_on(
        home.path(),
        &rt,
        &data,
        &[&session[..], &["--on", "laptop-b", "--no-focus", "--json"]].concat(),
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        remote_words(home.path(), "laptop-b"),
        [
            "'nebula'",
            "'open'",
            "'--session=far away'",
            "'--project=alpha proj'",
            "'--no-focus'"
        ]
    );
    let landed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        landed,
        serde_json::json!({
            "landed": "tui",
            "tui_pid": tui.child.process_id().unwrap(),
            "id": id,
            "name": "far away",
            "project": "alpha proj",
            "host": null,
            "focused": false,
        })
    );
    tui.wait_for_selected("far away");
    assert_typing_reaches(&mut tui, "on1", &id);
}

/// A caller of `open --on` tells apart an unreachable HOST (3), a HOST with
/// no TUI running (5) and a HOST whose nebula predates `open --session`
/// (1, with an upgrade hint instead of clap's 2).
#[test]
fn open_on_host_exit_codes_name_the_failure() {
    let dirs = tempfile::tempdir().unwrap();
    let (rt, data) = (dirs.path().join("rt"), dirs.path().join("data"));
    let args = ["--session", "far", "--on", "laptop-b", "--no-focus"];

    let up = ssh_stub_home(None);
    let unreachable = open_on(
        up.path(),
        &rt,
        &data,
        &args,
        &[("SSH_STUB_UNREACHABLE", "1")],
    );
    assert_eq!(unreachable.status.code(), Some(3), "{unreachable:?}");
    assert!(unreachable.stdout.is_empty(), "{unreachable:?}");

    let no_tui = ssh_stub_home(Some(
        "#!/bin/sh\necho 'nebula: no nebula TUI is running' >&2\nexit 3\n",
    ));
    let out = open_on(no_tui.path(), &rt, &data, &args, &[]);
    assert_eq!(out.status.code(), Some(5), "{out:?}");

    let old = ssh_stub_home(Some(
        "#!/bin/sh\necho \"error: unexpected argument '--session' found\" >&2\nexit 2\n",
    ));
    let out = open_on(old.path(), &rt, &data, &args, &[]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("upgrade"),
        "{out:?}"
    );
}

/// An attach the daemon cannot complete (the agent CLI vanished, so the
/// respawn fails) is a failure, not a landing: the command waits for the
/// attach itself.
#[test]
fn open_session_fails_when_the_attach_fails() {
    let bin = tempfile::tempdir().unwrap();
    let agent_cmd = bin.path().join("agent");
    std::fs::write(
        &agent_cmd,
        "#!/bin/sh\necho stand-in started\nexec /bin/sh \"$@\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&agent_cmd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut tui =
        TuiHarness::spawn_with_env(&[("NEBULA_AGENT_CMD", agent_cmd.display().to_string())]);
    let repo = tui.make_repo("alpha-proj");
    add_project(&mut tui, &repo, "alpha-proj");
    let new = nebula_cli(
        &tui,
        &[
            "agent",
            "new",
            "--project",
            "alpha-proj",
            "--worktree",
            "main",
            "--name",
            "broken",
        ],
    );
    assert!(new.status.success(), "{new:?}");
    tui.wait_for_text("broken");
    let mut read = None;
    let started = poll_until(WAIT, || {
        let out = nebula_cli(
            &tui,
            &["agent", "read", "broken", "--project", "alpha-proj"],
        );
        let started = String::from_utf8_lossy(&out.stdout).contains("stand-in started");
        read = Some(out);
        started.then_some(())
    });
    assert!(started.is_some(), "stand-in never started: {read:?}");
    std::fs::remove_file(&agent_cmd).unwrap();
    let exit = nebula_cli(
        &tui,
        &["agent", "send", "broken", "exit", "--project", "alpha-proj"],
    );
    assert!(exit.status.success(), "{exit:?}");
    let mut row = serde_json::Value::Null;
    let exited = poll_until(WAIT, || {
        let show = nebula_cli(
            &tui,
            &["agent", "show", "broken", "--project", "alpha-proj"],
        );
        row = serde_json::from_slice(&show.stdout).unwrap();
        (row["alive"] == false).then_some(())
    });
    assert!(exited.is_some(), "PTY never exited: {row}");

    let out = nebula_cli(&tui, &["open", "--session", "broken", "--no-focus"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("Error: nebula could not attach \"broken\": attach: "),
        "{stderr}"
    );
}
