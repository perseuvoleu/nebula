//! `heavy [GB] [--here] -- <cmd>`: run a command under a memory lease held
//! by this machine's daemon. The lease is the open connection, so it ends
//! with this process however it dies. The command runs here, as our child
//! leading its own process group, so a signal aimed at us reaches its whole
//! tree; it keeps the caller's terminal and stdio, and its exit status
//! becomes ours.

use crate::ipc::{handshake, split_connection, try_connect, Incompatible, IpcChannels};
use nebula_core::heavy::{daemon_id, processes, HeavyMark, Process};
use nebula_core::{paths, Blocked, ClientRequest, ServerEvent, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::future::Future;
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::pin::Pin;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time::Instant;

const GIB: f64 = (1u64 << 30) as f64;
const REQUEUE_WINDOW: Duration = Duration::from_secs(10);
const RECONNECT_EVERY: Duration = Duration::from_millis(100);
/// For the handshake and the first answer to a request; the queue wait
/// after that is unbounded.
const ANSWER_WITHIN: Duration = Duration::from_secs(5);
/// What `env`, `nice` and `timeout` exit with when they do not run the
/// command.
const REFUSED: i32 = 125;

pub struct HeavyOpts {
    /// None takes the daemon's configured command reservation.
    pub reserve_bytes: Option<u64>,
    pub command: Vec<OsString>,
}

/// Clap parser for the GB argument: a positive, finite number of GiB.
pub fn parse_gib(text: &str) -> Result<u64, String> {
    let gib: f64 = text
        .parse()
        .map_err(|_| format!("{text:?} is not a number of GiB"))?;
    if !gib.is_finite() || gib <= 0.0 {
        return Err(format!("{text:?} must be a positive number of GiB"));
    }
    let bytes = gib * GIB;
    if bytes >= u64::MAX as f64 {
        return Err(format!("{text:?} GiB is more bytes than 64 bits hold"));
    }
    match bytes as u64 {
        0 => Err(format!("{text:?} rounds to zero bytes")),
        bytes => Ok(bytes),
    }
}

/// Rust's runtime ignores SIGPIPE before `main` runs, so the binary records
/// the caller's disposition earlier, through `note_inherited_sigpipe`.
static SIGPIPE_WAS_IGNORED: AtomicBool = AtomicBool::new(false);

/// For a constructor that runs before Rust's runtime starts.
pub extern "C" fn note_inherited_sigpipe() {
    SIGPIPE_WAS_IGNORED.store(is_ignored(SIGPIPE), Ordering::Relaxed);
}

/// Called after `std::process::Command` resets SIGPIPE in a child.
fn restore_inherited_sigpipe(ignored: bool) {
    if ignored {
        unsafe { set_disposition(SIGPIPE, SIG_IGN) };
    }
}

/// An ignored SIGCHLD reaps every child before its waiter can read it:
/// macOS `ps` in the ancestry lookup, and the command.
static SIGCHLD_WAS_IGNORED: AtomicBool = AtomicBool::new(false);

/// A failed self re-exec has already put an ignored SIGCHLD back, so this
/// runs again after it and keeps the first answer.
fn make_children_waitable() {
    if unsafe { set_disposition(SIGCHLD, SIG_DFL) } == SIG_IGN {
        SIGCHLD_WAS_IGNORED.store(true, Ordering::Relaxed);
    }
}

fn restore_inherited_sigchld(ignored: bool) {
    if ignored {
        unsafe { set_disposition(SIGCHLD, SIG_IGN) };
    }
}

/// Before any IPC, and before the async runtime starts. Inside another
/// heavy command's tree this execs the command, which then runs under the
/// outer lease. Otherwise this process re-executes itself once with its own
/// `HeavyMark`, so the mark is there before any grant can be read; it
/// returns in the marked process.
pub fn mark_or_run_nested(opts: &HeavyOpts) {
    mark_or_run_nested_with(opts, processes)
}

fn mark_or_run_nested_with(opts: &HeavyOpts, processes: fn() -> Vec<Process>) {
    make_children_waitable();
    let me = std::process::id();
    let mark = HeavyMark {
        wrapper: me,
        reserve_bytes: opts.reserve_bytes,
        daemon: daemon_id(&paths::socket_path()),
    };
    let marked = std::env::var(HeavyMark::ENV)
        .ok()
        .and_then(|v| HeavyMark::from_value(&v));
    if marked == Some(mark) {
        return;
    }
    if inside_a_wrapper_of(me, mark.daemon, processes()) {
        let mut command = std::process::Command::new(&opts.command[0]);
        command.args(&opts.command[1..]);
        let e = exec(command);
        eprintln!("heavy: {}: {e}", opts.command[0].to_string_lossy());
        std::process::exit(not_run_code(&e));
    }
    let mut args = std::env::args_os();
    let (Ok(exe), Some(argv0)) = (std::env::current_exe(), args.next()) else {
        eprintln!("heavy: cannot find this binary to mark it; a daemon restart will not count this command");
        return;
    };
    let mut again = std::process::Command::new(exe);
    again
        .arg0(argv0)
        .args(args)
        .env(HeavyMark::ENV, mark.value());
    let e = exec(again);
    make_children_waitable();
    eprintln!("heavy: cannot re-run this binary marked ({e}); a daemon restart will not count this command");
}

fn exec(mut command: std::process::Command) -> std::io::Error {
    let pipe_ignored = SIGPIPE_WAS_IGNORED.load(Ordering::Relaxed);
    let chld_ignored = SIGCHLD_WAS_IGNORED.load(Ordering::Relaxed);
    unsafe {
        command.pre_exec(move || {
            restore_inherited_sigpipe(pipe_ignored);
            restore_inherited_sigchld(chld_ignored);
            Ok(())
        });
    }
    command.exec()
}

fn inside_a_wrapper_of(me: u32, daemon: u64, processes: Vec<Process>) -> bool {
    let table: HashMap<u32, _> = processes.into_iter().map(|p| (p.pid, p)).collect();
    let mut pid = table.get(&me).map(|p| p.ppid);
    for _ in 0..table.len() {
        let Some(process) = pid.and_then(|p| table.get(&p)) else {
            return false;
        };
        if process.is_wrapper() && process.mark.is_some_and(|m| m.daemon == daemon) {
            return true;
        }
        pid = Some(process.ppid);
    }
    false
}

fn not_run_code(e: &std::io::Error) -> i32 {
    if e.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

/// Run the command and end this process the way the command ended.
pub async fn run(opts: HeavyOpts) -> ! {
    let asked = Instant::now();
    let command = opts
        .command
        .iter()
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let lease = match acquire(opts.reserve_bytes, &command).await {
        Ok(lease) => Some(lease),
        Err(refused @ Unleased::TooLarge { .. }) => {
            eprintln!("heavy: {refused}; not running it");
            std::process::exit(REFUSED);
        }
        Err(why) => {
            eprintln!("heavy: {why}; running without a memory lease");
            None
        }
    };
    let waited = asked.elapsed();

    let mut term = catch_unless_ignored(SIGTERM);
    let mut hup = catch_unless_ignored(SIGHUP);
    let mut int = catch_unless_ignored(SIGINT);
    let mut quit = catch_unless_ignored(SIGQUIT);

    let mut cont = signal(SignalKind::from_raw(SIGCONT)).ok();
    let mut terminal = Terminal::controlling();
    let mut spawner = std::process::Command::new(&opts.command[0]);
    spawner.args(&opts.command[1..]);
    let hand_over = terminal
        .as_ref()
        .map(|t| (t.tty.as_raw_fd(), t.ttou_before, t.command_has_terminal));
    let pipe_ignored = SIGPIPE_WAS_IGNORED.load(Ordering::Relaxed);
    let chld_ignored = SIGCHLD_WAS_IGNORED.load(Ordering::Relaxed);
    unsafe {
        spawner.pre_exec(move || {
            restore_inherited_sigpipe(pipe_ignored);
            restore_inherited_sigchld(chld_ignored);
            setpgid(0, 0);
            if let Some((tty, ttou_before, foreground)) = hand_over {
                Terminal::set_up_child(tty, ttou_before, foreground);
            }
            Ok(())
        });
    }
    let pid = match spawner.spawn() {
        Ok(child) => child.id(),
        Err(e) => {
            eprintln!("heavy: {}: {e}", opts.command[0].to_string_lossy());
            report_wait(waited);
            take_back(terminal);
            std::process::exit(not_run_code(&e));
        }
    };
    let mut waits = watch(pid);

    let (mut link, reserved_bytes) = match lease {
        Some(Granted {
            channels,
            reserved_bytes,
        }) => (Some(channels), reserved_bytes),
        None => (None, 0),
    };
    let mut rejoin: Option<Pin<Box<dyn Future<Output = Rejoin>>>> = None;

    let status = loop {
        tokio::select! {
            Some(wait) = waits.recv() => match wait {
                Wait::Exited(status) => break status,
                Wait::Stopped(sig) => {
                    if let Some(t) = terminal.as_mut() {
                        t.suspend(sig);
                    }
                }
            },
            _ = recv_or_pending(&mut cont) => {
                if let Some(t) = terminal.as_mut() {
                    t.resume(pid);
                }
            }
            _ = recv_or_pending(&mut term) => end(pid, SIGTERM),
            _ = recv_or_pending(&mut hup) => end(pid, SIGHUP),
            _ = recv_or_pending(&mut int) => forward(pid, SIGINT),
            _ = recv_or_pending(&mut quit) => forward(pid, SIGQUIT),
            _ = lost(&mut link) => {
                eprintln!("heavy: lost the nebula daemon; the command keeps running and re-registers when it is back");
                rejoin = Some(Box::pin(re_register(reserved_bytes, command.clone())));
            }
            rejoined = async { rejoin.as_mut().unwrap().await }, if rejoin.is_some() => {
                rejoin = None;
                match rejoined {
                    Rejoin::Leased(channels) => {
                        eprintln!("heavy: re-registered pid {pid} with the restarted nebula daemon");
                        link = Some(channels);
                    }
                    Rejoin::Refused => eprintln!("heavy: the restarted nebula daemon refused pid {pid}; the command keeps running without a memory lease"),
                    Rejoin::Incompatible(version) => eprintln!("heavy: the nebula daemon back on the socket speaks protocol v{version}, this client v{PROTOCOL_VERSION}; the command keeps running without a memory lease"),
                }
            }
        }
    };
    report_wait(waited);
    take_back(terminal);
    exit_like(status)
}

enum Wait {
    Stopped(i32),
    Exited(ExitStatus),
}

/// Our own waitpid, since tokio's child wait never reports a stop.
fn watch(pid: u32) -> mpsc::UnboundedReceiver<Wait> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || loop {
        let mut status = 0;
        let waited = unsafe { waitpid(pid as i32, &mut status, WUNTRACED) };
        let wait = if waited != pid as i32 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            Wait::Exited(ExitStatus::from_raw(WAIT_FAILED))
        } else if let Some(sig) = stop_signal(status) {
            Wait::Stopped(sig)
        } else {
            Wait::Exited(ExitStatus::from_raw(status))
        };
        let exited = matches!(wait, Wait::Exited(_));
        if tx.send(wait).is_err() || exited {
            return;
        }
    });
    rx
}

/// `WIFSTOPPED` and `WSTOPSIG`, the same encoding on Linux and macOS.
fn stop_signal(status: i32) -> Option<i32> {
    (status & 0xff == 0x7f).then_some((status >> 8) & 0xff)
}

struct Granted {
    channels: IpcChannels,
    reserved_bytes: u64,
}

enum Unleased {
    NoDaemon(anyhow::Error),
    Silent,
    TooLarge {
        reserved_bytes: u64,
        budget_bytes: u64,
    },
    DaemonLost,
}

impl std::fmt::Display for Unleased {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unleased::NoDaemon(e) => write!(f, "no nebula daemon reachable ({e})"),
            Unleased::Silent => write!(
                f,
                "the nebula daemon did not answer within {}s",
                ANSWER_WITHIN.as_secs()
            ),
            Unleased::TooLarge {
                reserved_bytes,
                budget_bytes,
            } => write!(
                f,
                "{:.1} GiB can never fit this machine's {:.1} GiB heavy budget",
                *reserved_bytes as f64 / GIB,
                *budget_bytes as f64 / GIB
            ),
            Unleased::DaemonLost => write!(f, "lost the nebula daemon while queued"),
        }
    }
}

async fn acquire(reserve_bytes: Option<u64>, command: &str) -> Result<Granted, Unleased> {
    let mut channels = connect().await.map_err(Unleased::NoDaemon)?;
    let mut give_up_at = None;
    loop {
        let _ = channels
            .tx
            .send(ClientRequest::HeavyLease {
                reserve_bytes,
                command: command.to_string(),
            })
            .await;
        let mut daemon_replied = false;
        loop {
            let event = if daemon_replied {
                channels.rx.recv().await
            } else {
                tokio::time::timeout(ANSWER_WITHIN, channels.rx.recv())
                    .await
                    .map_err(|_| Unleased::Silent)?
            };
            let Some(event) = event else {
                break;
            };
            daemon_replied = true;
            match event {
                ServerEvent::HeavyQueued { position, blocked } => {
                    eprintln!(
                        "heavy: queued at position {position}, waiting for memory ({})",
                        reason(blocked)
                    );
                }
                ServerEvent::HeavyGranted { reserved_bytes } => {
                    return Ok(Granted {
                        channels,
                        reserved_bytes,
                    })
                }
                ServerEvent::HeavyTooLarge {
                    reserved_bytes,
                    budget_bytes,
                } => {
                    return Err(Unleased::TooLarge {
                        reserved_bytes,
                        budget_bytes,
                    })
                }
                _ => {}
            }
        }
        let until = match give_up_at {
            Some(until) if !daemon_replied => until,
            _ => Instant::now() + REQUEUE_WINDOW,
        };
        give_up_at = Some(until);
        channels = match reconnect(until).await {
            Some(channels) if Instant::now() < until => channels,
            _ => return Err(Unleased::DaemonLost),
        };
    }
}

async fn connect() -> anyhow::Result<IpcChannels> {
    let connecting = async {
        let stream = try_connect(&paths::socket_path()).await?;
        anyhow::Ok(split_connection(handshake(stream).await?))
    };
    tokio::time::timeout(ANSWER_WITHIN, connecting)
        .await
        .map_err(|_| anyhow::anyhow!("no handshake within {}s", ANSWER_WITHIN.as_secs()))?
}

async fn reconnect(until: Instant) -> Option<IpcChannels> {
    loop {
        if let Ok(channels) = connect().await {
            return Some(channels);
        }
        if Instant::now() >= until {
            return None;
        }
        tokio::time::sleep(RECONNECT_EVERY).await;
    }
}

enum Rejoin {
    Leased(IpcChannels),
    Refused,
    Incompatible(u32),
}

async fn re_register(reserved_bytes: u64, command: String) -> Rejoin {
    loop {
        let mut channels = match connect().await {
            Ok(channels) => channels,
            Err(e) => {
                if let Some(Incompatible(version)) = e.downcast_ref() {
                    return Rejoin::Incompatible(*version);
                }
                tokio::time::sleep(RECONNECT_EVERY).await;
                continue;
            }
        };
        let _ = channels
            .tx
            .send(ClientRequest::HeavyRunning {
                reserved_bytes,
                command: command.clone(),
            })
            .await;
        while let Some(event) = channels.rx.recv().await {
            match event {
                ServerEvent::HeavyGranted { .. } => return Rejoin::Leased(channels),
                ServerEvent::HeavyRunningRefused => return Rejoin::Refused,
                _ => {}
            }
        }
    }
}

async fn lost(link: &mut Option<IpcChannels>) {
    let Some(channels) = link else {
        return std::future::pending().await;
    };
    while channels.rx.recv().await.is_some() {}
    *link = None;
}

fn catch_unless_ignored(sig: i32) -> Option<Signal> {
    if is_ignored(sig) {
        return None;
    }
    signal(SignalKind::from_raw(sig)).ok()
}

/// `signal()` can only read a disposition by replacing it, so the old one
/// goes straight back; `sigaction`'s struct layout differs per platform.
fn is_ignored(sig: i32) -> bool {
    let before = unsafe { set_disposition(sig, SIG_IGN) };
    if before != SIG_IGN {
        unsafe { set_disposition(sig, before) };
    }
    before == SIG_IGN
}

async fn recv_or_pending(signal: &mut Option<Signal>) {
    match signal {
        Some(signal) => {
            signal.recv().await;
        }
        None => std::future::pending().await,
    }
}

fn reason(blocked: Blocked) -> &'static str {
    match blocked {
        Blocked::OverBudget => "heavy budget full",
        Blocked::LowMemory => "low free memory",
        Blocked::Pressure => "memory pressure",
        Blocked::ProbeFailed => "memory reading failed",
    }
}

fn report_wait(waited: Duration) {
    eprintln!("heavy: waited {:.1}s for memory", waited.as_secs_f64());
}

const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGQUIT: i32 = 3;
const SIGPIPE: i32 = 13;
const SIGTERM: i32 = 15;
#[cfg(target_os = "linux")]
const SIGCHLD: i32 = 17;
#[cfg(not(target_os = "linux"))]
const SIGCHLD: i32 = 20;
#[cfg(target_os = "linux")]
const SIGCONT: i32 = 18;
#[cfg(not(target_os = "linux"))]
const SIGCONT: i32 = 19;
#[cfg(target_os = "linux")]
const SIGTSTP: i32 = 20;
#[cfg(not(target_os = "linux"))]
const SIGTSTP: i32 = 18;
const SIGTTOU: i32 = 22;
const WUNTRACED: i32 = 2;
/// A wait status saying "exited 1".
const WAIT_FAILED: i32 = 1 << 8;
const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    #[link_name = "signal"]
    fn set_disposition(sig: i32, handler: usize) -> usize;
    fn raise(sig: i32) -> i32;
    fn getpid() -> i32;
    fn getpgrp() -> i32;
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn tcgetpgrp(fd: i32) -> i32;
    fn tcsetpgrp(fd: i32, pgrp: i32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

/// Only the terminal's foreground group can read it and gets Ctrl-C and
/// Ctrl-\, so the command's group takes it over whenever we hold it: at
/// start, or on `fg` after starting in the background. SIGTTOU stays
/// ignored here, since we move the terminal while in the background.
struct Terminal {
    tty: File,
    ours: i32,
    ttou_before: usize,
    command_has_terminal: bool,
}

impl Terminal {
    fn controlling() -> Option<Self> {
        let tty = File::open("/dev/tty").ok()?;
        let ours = unsafe { getpgrp() };
        let foreground = unsafe { tcgetpgrp(tty.as_raw_fd()) } == ours;
        let ttou_before = unsafe { set_disposition(SIGTTOU, SIG_IGN) };
        Some(Self {
            tty,
            ours,
            ttou_before,
            command_has_terminal: foreground,
        })
    }

    /// Only the inherited ignored SIGTTOU lets the child, already in its
    /// own background group, take the terminal; the command then gets the
    /// caller's disposition back.
    fn set_up_child(tty: i32, ttou_before: usize, foreground: bool) {
        unsafe {
            if foreground {
                tcsetpgrp(tty, getpid());
            }
            set_disposition(SIGTTOU, ttou_before);
        }
    }

    /// SIGTTOU stays ignored here, so a command stopped by it stops us with
    /// SIGTSTP.
    fn suspend(&mut self, sig: i32) {
        self.reclaim();
        let sig = if sig == SIGTTOU { SIGTSTP } else { sig };
        unsafe {
            kill(-self.ours, sig);
        }
    }

    /// `fg` hands us the terminal, whether or not the command had stopped
    /// yet, and the command gets it; after `bg` the command runs in the
    /// background as we do.
    fn resume(&mut self, pid: u32) {
        let tty = self.tty.as_raw_fd();
        if !self.command_has_terminal && unsafe { tcgetpgrp(tty) } == self.ours {
            unsafe { tcsetpgrp(tty, pid as i32) };
            self.command_has_terminal = true;
        }
        forward(pid, SIGCONT);
    }

    fn reclaim(&mut self) {
        if std::mem::take(&mut self.command_has_terminal) {
            unsafe {
                tcsetpgrp(self.tty.as_raw_fd(), self.ours);
            }
        }
    }
}

fn take_back(terminal: Option<Terminal>) {
    if let Some(mut t) = terminal {
        t.reclaim();
    }
}

fn forward(pid: u32, sig: i32) {
    unsafe {
        kill(-(pid as i32), sig);
    }
}

/// A stopped command only acts on TERM or HUP once continued, as the
/// kernel's own hangup does.
fn end(pid: u32, sig: i32) {
    forward(pid, sig);
    forward(pid, SIGCONT);
}

fn exit_like(status: ExitStatus) -> ! {
    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    let sig = status.signal().unwrap_or(1);
    unsafe {
        set_disposition(sig, SIG_DFL);
        raise(sig);
    }
    std::process::exit(128 + sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_NESTED: i32 = 99;

    #[test]
    fn an_ignored_sigchld_still_finds_the_outer_wrapper_and_stays_ignored_in_the_command() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "heavy_cli::tests::nested_under_an_ignored_sigchld",
                "--ignored",
            ])
            .env_remove(HeavyMark::ENV)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_ne!(
            out.status.code(),
            Some(NOT_NESTED),
            "the ancestry lookup could not wait for its helper, so it missed the outer wrapper"
        );
        assert_eq!(out.status.code(), Some(7), "{stdout}");
        assert!(stdout.ends_with("trap -- '' SIGCHLD\n"), "{stdout}");
    }

    #[test]
    #[ignore = "a child process for an_ignored_sigchld_still_finds_the_outer_wrapper_and_stays_ignored_in_the_command"]
    fn nested_under_an_ignored_sigchld() {
        unsafe { set_disposition(SIGCHLD, SIG_IGN) };
        let opts = HeavyOpts {
            reserve_bytes: None,
            command: ["bash", "-c", "trap -p CHLD; exit 7"]
                .map(OsString::from)
                .into(),
        };
        mark_or_run_nested_with(&opts, an_outer_wrapper_listed_by_a_helper);
        std::process::exit(NOT_NESTED);
    }

    fn an_outer_wrapper_listed_by_a_helper() -> Vec<Process> {
        let helper = std::process::Command::new("true").status();
        if !helper.is_ok_and(|s| s.success()) {
            return Vec::new();
        }
        let outer = u32::MAX - 1;
        let mark = HeavyMark {
            wrapper: outer,
            reserve_bytes: None,
            daemon: daemon_id(&paths::socket_path()),
        };
        vec![
            Process {
                pid: std::process::id(),
                ppid: outer,
                mark: None,
            },
            Process {
                pid: outer,
                ppid: 1,
                mark: Some(mark),
            },
        ]
    }
}
