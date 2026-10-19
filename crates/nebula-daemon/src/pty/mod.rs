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
use tokio::sync::{broadcast, mpsc, oneshot};

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
    reaped: tokio::sync::watch::Sender<Option<Option<i32>>>,
    exited_at: std::sync::OnceLock<std::time::Instant>,
    resume_until: std::sync::OnceLock<std::time::Instant>,
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
        Self::spawn_holding(sref, spec, Box::new(()))
    }

    /// `spawn`, keeping `hold` alive until the child has been reaped (or
    /// dropping it at once when the spawn fails).
    pub fn spawn_holding(
        sref: SessionRef,
        spec: SpawnSpec,
        hold: Box<dyn Send>,
    ) -> Result<Arc<Self>> {
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

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .context("spawn child in pty")?;
        drop(pair.slave);

        let killer = child.clone_killer();
        let child_pid = child.process_id();
        let io = pair
            .master
            .try_clone_reader()
            .context("clone pty reader")
            .and_then(|r| Ok((r, pair.master.take_writer().context("take pty writer")?)));
        let (reader, writer) = match io {
            Ok(io) => io,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };

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
            reaped: tokio::sync::watch::Sender::new(None),
            exited_at: std::sync::OnceLock::new(),
            resume_until: std::sync::OnceLock::new(),
        });

        let (data_tx, data_rx) = mpsc::channel(READER_CHANNEL_BOUND);
        let (exit_tx, exit_rx) = oneshot::channel();
        let wait = Box::new(move || child.wait().ok().map(|st| st.exit_code() as i32));
        spawn_reader_thread(reader, wait, data_tx, exit_tx);
        tokio::spawn(pump(session.clone(), data_rx, exit_rx, hold));
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
        let line_mode = self.master.lock().unwrap().get_termios().is_some_and(|t| {
            t.local_flags.bits() & LocalFlags::ICANON.bits() != 0
        });
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

    /// Resolves once the child has been reaped and what `spawn_holding`
    /// held was dropped.
    pub async fn reaped(&self) {
        self.exited().await;
    }

    pub fn has_exited(&self) -> bool {
        self.reaped.borrow().is_some()
    }

    /// Mark this process as a resumed conversation that counts as stale if
    /// it fails before `until`.
    pub fn resumed_until(&self, until: std::time::Instant) {
        let _ = self.resume_until.set(until);
    }

    pub fn resume_failed(&self, exit_code: Option<i32>) -> bool {
        let in_time = match (self.resume_until.get(), self.exited_at.get()) {
            (Some(until), Some(at)) => at <= until,
            _ => false,
        };
        in_time && exit_code.unwrap_or(1) != 0 && !self.was_killed()
    }

    /// `reaped`, with the child's exit code. It stays answered after the
    /// fact, for an observer that starts watching only after the exit.
    pub async fn exited(&self) -> Option<i32> {
        let mut rx = self.reaped.subscribe();
        let exit = rx.wait_for(Option::is_some).await;
        exit.ok().and_then(|code| *code).flatten()
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
    wait: Box<dyn FnOnce() -> Option<i32> + Send>,
    data: mpsc::Sender<Vec<u8>>,
    exit: oneshot::Sender<Option<i32>>,
) {
    std::thread::Builder::new()
        .name("pty-reader".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut buf = [0u8; 16 * 1024];
                let mut relaying = true;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            relaying = relaying && data.blocking_send(buf[..n].to_vec()).is_ok();
                        }
                    }
                }
            }));
            if read.is_err() {
                tracing::error!("pty reader panicked; waiting for the child anyway");
            }
            drop(data);
            let _ = exit.send(wait());
        })
        .expect("spawn pty reader thread");
}

/// The child's memory charge (`hold`) is released, and `reaped` set, only
/// after the reader thread reports the child waited for. A reader that
/// vanishes without that report leaves liveness unknown, so the charge
/// stays held for good.
async fn pump(
    session: Arc<PtySession>,
    data: mpsc::Receiver<Vec<u8>>,
    exit: oneshot::Receiver<Option<i32>>,
    hold: Box<dyn Send>,
) {
    let output = tokio::spawn(pump_output(session.clone(), data));
    let exited = exit.await;
    let _ = session.exited_at.set(std::time::Instant::now());
    if let Err(e) = output.await {
        tracing::error!(session = ?session.sref, error = %e, "pty output pump failed; output after it was dropped");
    }
    let Ok(exit_code) = exited else {
        tracing::error!(session = ?session.sref, "pty reader ended without reaping its child; its memory charge stays held");
        std::mem::forget(hold);
        return;
    };
    drop(hold);
    session.reaped.send_replace(Some(exit_code));
    let _ = session.events.send(PtyEvent::Exited { exit_code });
    tracing::info!(session = ?session.sref, "pty pump ended");
}

/// Drains the reader channel: append to the ring (always — detach is free),
/// coalesce bursts, broadcast to whoever is attached.
async fn pump_output(session: Arc<PtySession>, mut rx: mpsc::Receiver<Vec<u8>>) {
    let session = &*session;
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

    loop {
        if pending.is_empty() {
            match rx.recv().await {
                Some(d) => pending.extend_from_slice(&d),
                None => return,
            }
        }
        // Coalesce until the deadline or the size cap; the deadline is fixed
        // at the first pending byte so continuous streams still flush on time.
        let deadline = tokio::time::Instant::now() + COALESCE_HOLD;
        while pending.len() < COALESCE_BYTES {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(d) => pending.extend_from_slice(&d),
                    None => {
                        flush(session, &mut pending);
                        return;
                    }
                },
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
        flush(session, &mut pending);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct SlowRelease(Arc<AtomicBool>);

    impl Drop for SlowRelease {
        fn drop(&mut self) {
            std::thread::sleep(std::time::Duration::from_millis(300));
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reaped_session_has_already_released_what_it_held() {
        let released = Arc::new(AtomicBool::new(false));
        let session = PtySession::spawn_holding(
            SessionRef::Agent(nebula_core::AgentId("a".into())),
            SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "exit 0".into()],
                cwd: std::env::temp_dir(),
                env: vec![],
                scrub_env: vec![],
                cols: 80,
                rows: 24,
            },
            Box::new(SlowRelease(released.clone())),
        )
        .unwrap();
        let mut events = session.events.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(10), session.reaped())
            .await
            .expect("the child was reaped");
        assert!(
            released.load(Ordering::SeqCst),
            "reaped before the hold was released"
        );
        let exited = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Ok(PtyEvent::Exited { .. }) = events.recv().await {
                    return released.load(Ordering::SeqCst);
                }
            }
        })
        .await
        .expect("Exited was broadcast");
        assert!(exited, "Exited went out before the hold was released");
        tokio::time::timeout(std::time::Duration::from_secs(1), session.reaped())
            .await
            .expect("a second wait after the exit returns at once");
    }

    fn sleeping_session() -> Arc<PtySession> {
        PtySession::spawn(
            SessionRef::Agent(nebula_core::AgentId("a".into())),
            SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "exec sleep 30".into()],
                cwd: std::env::temp_dir(),
                env: vec![],
                scrub_env: vec![],
                cols: 80,
                rows: 24,
            },
        )
        .unwrap()
    }

    /// Reports from its own OS thread, so an early wake cannot hide behind a
    /// busy tokio worker.
    fn wake_watcher(
        session: &Arc<PtySession>,
        released: &Arc<AtomicBool>,
    ) -> std::sync::mpsc::Receiver<bool> {
        let (woke, wake) = std::sync::mpsc::channel();
        let (session, released) = (session.clone(), released.clone());
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(session.reaped());
            let _ = woke.send(released.load(Ordering::SeqCst));
        });
        wake
    }

    struct PanickingReader;

    impl Read for PanickingReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("the pty read failed")
        }
    }

    #[tokio::test]
    async fn a_reader_that_panics_still_waits_for_its_child_and_reports_the_exit() {
        let waited = Arc::new(AtomicBool::new(false));
        let (data_tx, mut data_rx) = mpsc::channel(1);
        let (exit_tx, exit_rx) = oneshot::channel();
        let flag = waited.clone();
        spawn_reader_thread(
            Box::new(PanickingReader),
            Box::new(move || {
                flag.store(true, Ordering::SeqCst);
                Some(7)
            }),
            data_tx,
            exit_tx,
        );
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), exit_rx)
            .await
            .expect("the reader reported");
        assert_eq!(exit.ok(), Some(Some(7)));
        assert!(waited.load(Ordering::SeqCst));
        assert!(data_rx.recv().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reader_that_vanishes_without_reaping_keeps_the_charge_and_wakes_no_one() {
        let session = sleeping_session();
        let released = Arc::new(AtomicBool::new(false));
        let wake = wake_watcher(&session, &released);
        let mut events = session.events.subscribe();
        let (data_tx, data_rx) = mpsc::channel(1);
        let (exit_tx, exit_rx) = oneshot::channel::<Option<i32>>();
        let ending = tokio::spawn(pump(
            session.clone(),
            data_rx,
            exit_rx,
            Box::new(SlowRelease(released.clone())),
        ));
        drop((data_tx, exit_tx));
        ending.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !released.load(Ordering::SeqCst),
            "released while the child may still run"
        );
        assert!(
            wake.try_recv().is_err(),
            "signalled reaped with liveness unknown"
        );
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, PtyEvent::Exited { .. }),
                "broadcast Exited with no exit report"
            );
        }
        session.kill();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_output_pump_that_panics_holds_the_charge_until_the_child_is_reaped() {
        let session = sleeping_session();
        {
            let session = session.clone();
            let _ = std::thread::spawn(move || {
                let _poisoned = session.kitty.lock().unwrap();
                panic!("poison the scanner the output pump uses");
            })
            .join();
        }
        let released = Arc::new(AtomicBool::new(false));
        let wake = wake_watcher(&session, &released);
        let mut events = session.events.subscribe();
        let (data_tx, data_rx) = mpsc::channel(1);
        let (exit_tx, exit_rx) = oneshot::channel();
        let ending = tokio::spawn(pump(
            session.clone(),
            data_rx,
            exit_rx,
            Box::new(SlowRelease(released.clone())),
        ));
        data_tx.send(b"output".to_vec()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !released.load(Ordering::SeqCst),
            "the output pump's panic released the charge"
        );
        assert!(wake.try_recv().is_err());

        drop(data_tx);
        exit_tx.send(Some(0)).unwrap();
        let released_at_wake = tokio::task::spawn_blocking(move || {
            wake.recv_timeout(std::time::Duration::from_secs(2))
        })
        .await
        .unwrap()
        .expect("the reaped child woke the waiter");
        assert!(released_at_wake, "woken before the hold was released");
        ending.await.unwrap();
        let exited = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Ok(PtyEvent::Exited { exit_code }) = events.recv().await {
                    return exit_code;
                }
            }
        })
        .await
        .expect("Exited was broadcast");
        assert_eq!(exited, Some(0));
        session.kill();
    }
}
