pub mod kitty;
pub mod progress;
pub mod render;
pub mod ring;

use anyhow::{Context, Result};
use nebula_core::SessionRef;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use progress::ProgressScanner;
use ring::ScrollbackRing;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};

const RING_CAPACITY: usize = 1024 * 1024;
/// Flush coalesced output at this size…
const COALESCE_BYTES: usize = 8 * 1024;
/// …or this long after the first pending byte, whichever comes first. A hard
/// deadline (not a quiet-gap timer): a child streaming continuously in small
/// chunks must still flush on time, or output arrives in laggy 8KB lumps.
const COALESCE_HOLD: std::time::Duration = std::time::Duration::from_millis(5);
/// Reader thread → pump channel bound; blocking_send gives natural
/// backpressure against a fire-hosing child.
const READER_CHANNEL_BOUND: usize = 64;
/// After the polite SIGHUP, how long the child gets to exit before its whole
/// process group is SIGKILLed.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(3);
const SIGKILL_SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
/// How long a primary switch waits for the PTY children it stopped to exit:
/// [`KILL_GRACE`] for the hangup, then time for the SIGKILL to land. A child
/// still present then makes the switch refuse.
pub const STOP_DEADLINE: std::time::Duration = KILL_GRACE.saturating_add(SIGKILL_SETTLE);
/// What a tty in line mode queues before it drops input: MAX_CANON on macOS,
/// the smallest such limit among the supported platforms.
const LINE_MODE_INPUT_MAX: usize = 1024;

/// Broadcast to attached clients (and, later, the status machine).
#[derive(Clone, Debug)]
pub enum PtyEvent {
    Output {
        seq: u64,
        data: Vec<u8>,
    },
    Exited {
        exit_code: Option<i32>,
    },
    /// The child pushed/popped kitty keyboard flags; clients re-encode keys.
    KittyFlags {
        flags: u8,
    },
    /// The child's OSC 9;4 progress state flipped. For agent CLIs this is a
    /// busy/idle edge the status machine trusts — notably it is the *only*
    /// end-of-turn signal after the user cancels, which fires no hook.
    Progress {
        busy: bool,
    },
}

enum ReaderMsg {
    Data(Vec<u8>),
    Eof { exit_code: Option<i32> },
}

pub struct PtySession {
    pub sref: SessionRef,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// Child pid: drives the SIGHUP → SIGKILL escalation (the child is its
    /// PTY session's leader — portable-pty does setsid — so pgid == pid)
    /// and the metrics modal's process-tree sums.
    pub child_pid: Option<u32>,
    pub ring: Mutex<ScrollbackRing>,
    pub events: broadcast::Sender<PtyEvent>,
    /// Last applied size, for the attach-time SIGWINCH jiggle.
    last_size: Mutex<(u16, u16)>,
    /// Kitty keyboard negotiation state, fed by the pump from live output.
    kitty: Mutex<kitty::KittyScanner>,
    /// OSC 9;4 busy/idle tracking, likewise fed from live output.
    progress: Mutex<ProgressScanner>,
    killed: std::sync::atomic::AtomicBool,
}

pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: std::path::PathBuf,
    /// Extra env vars (NEBULA_* for agents). Plain terminals get none.
    pub env: Vec<(String, String)>,
    /// Env var names to scrub from the inherited environment.
    pub scrub_env: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

impl PtySession {
    /// Spawn the child in a fresh PTY and start its reader thread + pump task.
    pub fn spawn(sref: SessionRef, spec: SpawnSpec) -> Result<Arc<Self>> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: spec.rows,
                cols: spec.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        cmd.cwd(&spec.cwd);
        cmd.env("TERM", "xterm-256color");
        for name in &spec.scrub_env {
            cmd.env_remove(name);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("spawn child in pty")?;
        drop(pair.slave);

        let killer = child.clone_killer();
        let child_pid = child.process_id();
        let reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;

        let (events, _) = broadcast::channel(256);
        let session = Arc::new(Self {
            sref,
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            child_pid,
            ring: Mutex::new(ScrollbackRing::new(RING_CAPACITY)),
            events,
            last_size: Mutex::new((spec.cols, spec.rows)),
            kitty: Mutex::new(kitty::KittyScanner::new()),
            progress: Mutex::new(ProgressScanner::new()),
            killed: std::sync::atomic::AtomicBool::new(false),
        });

        let (tx, rx) = mpsc::channel::<ReaderMsg>(READER_CHANNEL_BOUND);
        spawn_reader_thread(reader, child, tx);
        tokio::spawn(pump(session.clone(), rx));
        Ok(session)
    }

    pub fn write_input(&self, data: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)?;
        w.flush()?;
        Ok(())
    }

    /// [`Self::write_input`] for a caller that reports delivery. A tty in
    /// line mode takes the write and silently drops what overflows its input
    /// queue, so input that long is refused instead.
    pub fn write_input_whole(&self, data: &[u8]) -> Result<()> {
        use nix::sys::termios::LocalFlags;
        let line_mode = self
            .master
            .lock()
            .unwrap()
            .get_termios()
            .is_some_and(|t| t.local_flags.bits() & LocalFlags::ICANON.bits() != 0);
        if line_mode && data.len() >= LINE_MODE_INPUT_MAX {
            anyhow::bail!(
                "the session's terminal is in line mode (its CLI is not reading keys yet) and would drop input past {LINE_MODE_INPUT_MAX} bytes; nothing was sent"
            );
        }
        self.write_input(data)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let master = self.master.lock().unwrap();
        master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        *self.last_size.lock().unwrap() = (cols, rows);
        Ok(())
    }

    /// Resize on attach. The kernel only delivers SIGWINCH on a *change*, so
    /// when the requested size equals the current one, jiggle (rows-1 then
    /// back) to force a full-screen repaint — the dtach trick.
    pub fn resize_with_jiggle(&self, cols: u16, rows: u16) -> Result<()> {
        let same = { *self.last_size.lock().unwrap() == (cols, rows) };
        if same && rows > 1 {
            let master = self.master.lock().unwrap();
            master.resize(PtySize {
                rows: rows - 1,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            Ok(())
        } else {
            self.resize(cols, rows)
        }
    }

    /// SIGHUP the child, then SIGKILL its whole process group if it hasn't
    /// exited within [`KILL_GRACE`]. The group kill also reaps grandchildren
    /// that would otherwise hold the slave fd open (no EOF → reader thread,
    /// pump task, and the 1MB ring all pinned forever).
    pub fn kill(&self) {
        self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
        // Subscribe before signalling so an immediate exit can't be missed.
        let mut rx = self.events.subscribe();
        let _ = self.killer.lock().unwrap().kill();
        let Some(pid) = self.child_pid else { return };
        let sref = self.sref.clone();
        // Watchdog on a plain thread: it must not hold the session Arc (that
        // would pin the ring), and it outlives any tokio context `kill` was
        // called from.
        std::thread::Builder::new()
            .name("pty-kill-watchdog".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                let deadline = std::time::Instant::now() + KILL_GRACE;
                let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
                while std::time::Instant::now() < deadline {
                    loop {
                        use tokio::sync::broadcast::error::TryRecvError;
                        match rx.try_recv() {
                            Ok(PtyEvent::Exited { .. }) | Err(TryRecvError::Closed) => return,
                            Ok(_) | Err(TryRecvError::Lagged(_)) => continue,
                            Err(TryRecvError::Empty) => break,
                        }
                    }
                    // Reaped (ESRCH) strictly precedes the Exited broadcast,
                    // so this also covers an Exited lost to channel lag.
                    if nix::sys::signal::kill(nix_pid, None).is_err() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                tracing::warn!(session = ?sref, pid, "child ignored SIGHUP — SIGKILLing its process group");
                let _ = nix::sys::signal::killpg(nix_pid, nix::sys::signal::Signal::SIGKILL);
            })
            .expect("spawn pty kill watchdog");
    }

    /// A kill was requested. The process may still be running.
    pub fn was_killed(&self) -> bool {
        self.killed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Ring snapshot for attach replay: (base_seq, bytes).
    pub fn snapshot(&self, from_seq: Option<u64>) -> (u64, Vec<u8>) {
        self.ring.lock().unwrap().snapshot_from(from_seq)
    }

    /// Last applied PTY size (cols, rows) — the grid ReadSession renders at.
    pub fn size(&self) -> (u16, u16) {
        *self.last_size.lock().unwrap()
    }

    /// The child's current kitty keyboard flags (0 = legacy).
    pub fn kitty_flags(&self) -> u8 {
        self.kitty.lock().unwrap().flags()
    }

    /// The child's advertised OSC 9;4 busy state, or `None` if it never
    /// advertised one (a CLI without a progress bar, or one not started yet).
    pub fn progress_busy(&self) -> Option<bool> {
        self.progress.lock().unwrap().busy()
    }
}

/// PTY reads are blocking → dedicated thread per session. After EOF it reaps
/// the child to get the exit code.
fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    tx: mpsc::Sender<ReaderMsg>,
) {
    std::thread::Builder::new()
        .name("pty-reader".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx
                            .blocking_send(ReaderMsg::Data(buf[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            let exit_code = child.wait().ok().map(|st| st.exit_code() as i32);
            let _ = tx.blocking_send(ReaderMsg::Eof { exit_code });
        })
        .expect("spawn pty reader thread");
}

/// Drains the reader channel: append to the ring (always — detach is free),
/// coalesce bursts, broadcast to whoever is attached.
async fn pump(session: Arc<PtySession>, mut rx: mpsc::Receiver<ReaderMsg>) {
    let mut pending: Vec<u8> = Vec::new();

    let flush = |session: &PtySession, pending: &mut Vec<u8>| {
        if pending.is_empty() {
            return;
        }
        // Kitty keyboard negotiation rides in the output stream; nothing else
        // would ever answer the child's queries (tmux does the same).
        let actions = session.kitty.lock().unwrap().feed(pending);
        if !actions.reply.is_empty() {
            if let Err(e) = session.write_input(&actions.reply) {
                tracing::warn!(error = %e, "kitty/DA reply write failed");
            }
        }
        let busy_edge = session.progress.lock().unwrap().feed(pending);
        let seq = session.ring.lock().unwrap().append(pending);
        let _ = session.events.send(PtyEvent::Output {
            seq,
            data: std::mem::take(pending),
        });
        if let Some(flags) = actions.flags_changed {
            tracing::debug!(session = ?session.sref, flags, "child kitty flags changed");
            let _ = session.events.send(PtyEvent::KittyFlags { flags });
        }
        if let Some(busy) = busy_edge {
            tracing::debug!(session = ?session.sref, busy, "child progress state changed");
            let _ = session.events.send(PtyEvent::Progress { busy });
        }
    };

    'outer: loop {
        if pending.is_empty() {
            match rx.recv().await {
                Some(ReaderMsg::Data(d)) => pending.extend_from_slice(&d),
                Some(ReaderMsg::Eof { exit_code }) => {
                    let _ = session.events.send(PtyEvent::Exited { exit_code });
                    break;
                }
                None => break,
            }
        }
        // Coalesce until the deadline or the size cap; the deadline is fixed
        // at the first pending byte so continuous streams still flush on time.
        let deadline = tokio::time::Instant::now() + COALESCE_HOLD;
        while pending.len() < COALESCE_BYTES {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(ReaderMsg::Data(d)) => pending.extend_from_slice(&d),
                    Some(ReaderMsg::Eof { exit_code }) => {
                        flush(&session, &mut pending);
                        let _ = session.events.send(PtyEvent::Exited { exit_code });
                        break 'outer;
                    }
                    None => {
                        flush(&session, &mut pending);
                        break 'outer;
                    }
                },
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        flush(&session, &mut pending);
    }
    tracing::info!(session = ?session.sref, "pty pump ended");
}
