use nebula_core::codec::{read_frame, write_frame};
use nebula_core::heavy::{daemon_id, HeavyMark};
use nebula_core::{ClientRequest, ServerEvent, PROTOCOL_VERSION};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
const SIGTSTP: i32 = 20;
#[cfg(not(target_os = "linux"))]
const SIGTSTP: i32 = 18;

const ONE_COMMAND_BUDGET: &str = r#"{"heavy":{"budget_gib":1.0,"floor_gib":0.0,"psi_some_avg10_max":100.0,"command_reservation_gib":0.6}}"#;

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let env = Self {
            tmp: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(env.data_dir()).unwrap();
        std::fs::write(env.data_dir().join("config.json"), ONE_COMMAND_BUDGET).unwrap();
        env
    }

    fn data_dir(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    fn runtime_dir(&self) -> PathBuf {
        self.tmp.path().join("rt")
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn spawn_daemon(&self) -> Daemon {
        self.spawn_daemon_with(&[])
    }

    fn spawn_daemon_with(&self, vars: &[(&str, &str)]) -> Daemon {
        let child = Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(["daemon", "--foreground"])
            .env("NEBULA_RUNTIME_DIR", self.runtime_dir())
            .env("NEBULA_DATA_DIR", self.data_dir())
            .env("SHELL", "/bin/sh")
            .env("NEBULA_AGENT_CMD", "/bin/sh")
            .envs(vars.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let sock = self.runtime_dir().join("daemon.sock");
        wait_until(Duration::from_secs(10), "daemon socket accepts", || {
            std::os::unix::net::UnixStream::connect(&sock).is_ok()
        });
        Daemon(child)
    }

    fn heavy(&self, args: &[&str], stdin: Option<&str>) -> Run {
        self.heavy_at(
            Path::new(env!("CARGO_BIN_EXE_nebula")),
            &["heavy"],
            args,
            stdin,
        )
    }

    fn heavy_at(&self, exe: &Path, prefix: &[&str], args: &[&str], stdin: Option<&str>) -> Run {
        let mut command = Command::new(exe);
        command
            .args(prefix)
            .args(args)
            .current_dir(self.tmp.path())
            .env("NEBULA_RUNTIME_DIR", self.runtime_dir())
            .env("NEBULA_DATA_DIR", self.data_dir())
            .env_remove("NEBULA_AGENT_ID")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(|| {
                setsid();
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        if let Some(input) = stdin {
            let mut pipe = child.stdin.take().unwrap();
            pipe.write_all(input.as_bytes()).unwrap();
        }
        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let readers = vec![
            collect(child.stdout.take().unwrap(), stdout.clone()),
            collect(child.stderr.take().unwrap(), stderr.clone()),
        ];
        Run {
            child,
            stdout,
            stderr,
            readers,
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        kill_commands_left_in(&self.tmp.path().canonicalize().unwrap());
    }
}

/// Commands lead process groups of their own, so killing a wrapper's group
/// misses them; all of them run in the test's directory.
fn kill_commands_left_in(dir: &Path) {
    for _ in 0..3 {
        let left = processes_in(dir);
        if left.is_empty() {
            return;
        }
        for pid in left {
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "linux")]
fn processes_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let cwd = std::fs::read_link(entry.path().join("cwd")).ok()?;
            cwd.starts_with(dir)
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn processes_in(dir: &Path) -> Vec<String> {
    let out = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-F", "pn"])
        .output()
        .unwrap();
    let mut pid = String::new();
    let mut found = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.to_string();
        } else if line
            .strip_prefix('n')
            .is_some_and(|cwd| Path::new(cwd).starts_with(dir))
        {
            found.push(pid.clone());
        }
    }
    found
}

fn collect(pipe: impl Read + Send + 'static, sink: Arc<Mutex<String>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            let mut text = sink.lock().unwrap();
            text.push_str(&line);
            text.push('\n');
        }
    })
}

struct Daemon(Child);

impl Daemon {
    fn stop(mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.0.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("daemon ignored SIGTERM");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Run {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

struct Finished {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl Run {
    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    fn wait_stderr(&self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.stderr().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("stderr never showed {needle:?}; got:\n{}", self.stderr());
    }

    fn signal(&self, sig: &str) {
        let ok = Command::new("kill")
            .args([sig, &self.child.id().to_string()])
            .status()
            .unwrap()
            .success();
        assert!(ok, "kill {sig} {}", self.child.id());
    }

    fn finish(mut self) -> Finished {
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "heavy never exited; stderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.readers.iter().all(JoinHandle::is_finished) {
            assert!(
                Instant::now() < deadline,
                "heavy exited but something it started still holds its output; stderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Finished {
            status,
            stdout: self.stdout.lock().unwrap().clone(),
            stderr: self.stderr(),
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", self.child.id())])
            .status();
        let _ = self.child.wait();
    }
}

fn wait_until(timeout: Duration, what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn held(name: &str) -> String {
    format!(
        "touch {name}.started; while [ -f {name}.started ] && [ ! -f {name}.release ]; do sleep 0.05; done"
    )
}

fn process_state(pid: &str) -> String {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn is_gone(pid: &str) -> bool {
    let state = process_state(pid);
    state.is_empty() || state.starts_with('Z')
}

fn wait_lines(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|l| l.starts_with("heavy: waited "))
        .count()
}

#[test]
fn hq_t04_second_command_queues_until_the_first_exits() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();

    let first = env.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });

    let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
    second.wait_stderr("heavy: queued at position 1,");
    assert!(
        !env.path("b.started").exists(),
        "second command ran while the first held the budget"
    );

    std::fs::write(env.path("a.release"), "").unwrap();
    let first = first.finish();
    assert!(first.status.success(), "first: {}", first.stderr);
    let second = second.finish();
    assert!(second.status.success(), "second: {}", second.stderr);
    assert!(env.path("b.started").exists());
}

#[test]
fn hq_t05_streams_exit_code_and_signals_pass_through() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();

    let link = env.path("heavy");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_nebula"), &link).unwrap();
    let run = env.heavy_at(
        &link,
        &[],
        &[
            "0.2",
            "--here",
            "--",
            "sh",
            "-c",
            "read line; echo \"got $line\"; echo to-stderr >&2; exit 7",
        ],
        Some("from-stdin\n"),
    );
    let done = run.finish();
    assert_eq!(done.status.code(), Some(7), "stderr:\n{}", done.stderr);
    assert_eq!(done.stdout, "got from-stdin\n");
    assert!(done.stderr.contains("to-stderr\n"), "{}", done.stderr);
    assert_eq!(wait_lines(&done.stderr), 1, "{}", done.stderr);

    let killed = env
        .heavy(&["--", "sh", "-c", "kill -TERM $$"], None)
        .finish();
    assert_eq!(killed.status.signal(), Some(15), "{}", killed.stderr);

    let trapping = env.heavy(
        &[
            "--",
            "sh",
            "-c",
            "trap 'echo child got TERM; exit 0' TERM; touch t.started; while :; do sleep 0.05; done",
        ],
        None,
    );
    wait_until(Duration::from_secs(20), "trapping child starts", || {
        env.path("t.started").exists()
    });
    trapping.signal("-TERM");
    let trapped = trapping.finish();
    assert_eq!(trapped.stdout, "child got TERM\n", "{}", trapped.stderr);
    assert_eq!(trapped.status.code(), Some(0), "{}", trapped.stderr);
}

#[test]
fn hq_t06_killed_wrappers_release_their_place_and_lease() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();

    let running = env.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    let waiting = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
    waiting.wait_stderr("heavy: queued at position 1,");
    let behind = env.heavy(&["--", "sh", "-c", "touch c.started"], None);
    behind.wait_stderr("heavy: queued at position 2,");

    waiting.signal("-KILL");
    behind.wait_stderr("heavy: queued at position 1,");

    running.signal("-KILL");
    let behind = behind.finish();
    assert!(behind.status.success(), "{}", behind.stderr);
    assert!(env.path("c.started").exists());
    assert!(!env.path("b.started").exists(), "killed waiter still ran");
}

#[test]
fn hq_t07_running_command_re_registers_after_daemon_restart() {
    let env = Env::new();
    let daemon = env.spawn_daemon();

    let script = format!("echo run >> runs; {}", held("a"));
    let running = env.heavy(&["--", "sh", "-c", &script], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });

    daemon.stop();
    let _daemon = env.spawn_daemon();
    running.wait_stderr("heavy: re-registered");

    let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
    second.wait_stderr("heavy: queued at position 1,");
    assert!(!env.path("b.started").exists());

    std::fs::write(env.path("a.release"), "").unwrap();
    let first = running.finish();
    assert!(first.status.success(), "{}", first.stderr);
    assert_eq!(wait_lines(&first.stderr), 1, "{}", first.stderr);
    let second = second.finish();
    assert!(second.status.success(), "{}", second.stderr);
    assert_eq!(
        std::fs::read_to_string(env.path("runs")).unwrap(),
        "run\n",
        "the command ran more than once"
    );
}

#[test]
fn hq_t08_absent_daemon_warns_and_runs_directly() {
    let env = Env::new();
    let done = env
        .heavy(&["--", "sh", "-c", "echo ran; exit 3"], None)
        .finish();
    assert_eq!(done.status.code(), Some(3), "{}", done.stderr);
    assert_eq!(done.stdout, "ran\n");
    assert!(
        done.stderr.contains("heavy: no nebula daemon reachable"),
        "{}",
        done.stderr
    );
}

#[test]
fn reservation_over_the_whole_budget_is_refused_and_never_runs() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();

    let done = env.heavy(&["2", "--", "touch", "ran"], None).finish();
    assert_eq!(done.status.code(), Some(125), "{}", done.stderr);
    assert!(!env.path("ran").exists(), "the refused command ran");
    assert!(done.stderr.contains("can never fit"), "{}", done.stderr);
}

#[test]
fn a_paused_wrapper_keeps_its_lease_across_a_daemon_restart() {
    let env = Env::new();
    let daemon = env.spawn_daemon();

    let script = format!("echo run >> runs; {}", held("a"));
    let running = env.heavy(&["--", "sh", "-c", &script], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    running.signal("-STOP");
    daemon.stop();
    let _daemon = env.spawn_daemon();

    let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !env.path("b.started").exists(),
        "the restarted daemon admitted a second command next to the paused one's"
    );
    second.wait_stderr("heavy: queued at position 1,");

    running.signal("-CONT");
    running.wait_stderr("heavy: re-registered");
    assert!(!env.path("b.started").exists());
    std::fs::write(env.path("a.release"), "").unwrap();
    let first = running.finish();
    assert!(first.status.success(), "{}", first.stderr);
    let second = second.finish();
    assert!(second.status.success(), "{}", second.stderr);
    assert_eq!(std::fs::read_to_string(env.path("runs")).unwrap(), "run\n");
}

#[test]
fn re_registration_is_refused_to_any_peer_the_daemon_did_not_count_at_startup() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let socket = env.runtime_dir().join("daemon.sock");

    let running = env.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    let unmarked = claim(&socket, re_registration());
    let forger = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "forged_claim_peer", "--ignored", "--nocapture"])
        .env(PEER_SOCKET, &socket)
        .output()
        .unwrap();
    let forger = String::from_utf8_lossy(&forger.stdout);
    assert_eq!(
        verdict(&unmarked),
        "refused",
        "an unmarked peer: {unmarked:?}"
    );
    assert!(
        forger.contains("claim answered: refused"),
        "a peer that marked itself after the daemon started re-registered:\n{forger}"
    );

    let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
    second.wait_stderr("heavy: queued at position 1,");
    assert!(
        !env.path("b.started").exists(),
        "a refused claim freed or bypassed the first command's lease"
    );
    std::fs::write(env.path("a.release"), "").unwrap();
    assert!(running.finish().status.success());
    assert!(second.finish().status.success());
}

#[test]
fn a_failed_rss_reading_keeps_the_last_one() {
    let env = Env::new();
    let path = fake_ps(&env);
    let _daemon = env.spawn_daemon_with(&[("PATH", &path)]);

    let script = format!("echo $$ > a.pid; {}", held("a"));
    let big = env.heavy(&["0.1", "--", "sh", "-c", &script], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    std::fs::copy(env.path("a.pid"), env.path("ps.big")).unwrap();
    wait_until(Duration::from_secs(20), "the daemon reads its RSS", || {
        line_count(&env.path("ps.served")) >= 2
    });

    let second = env.heavy(&["0.1", "--", "sh", "-c", "touch b.started"], None);
    second.wait_stderr("heavy: queued at position 1,");
    std::fs::write(env.path("ps.fail"), "").unwrap();
    wait_until(Duration::from_secs(20), "an RSS reading fails", || {
        line_count(&env.path("ps.failed")) >= 2
    });
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !env.path("b.started").exists(),
        "a failed RSS reading dropped the first command's measured memory"
    );

    std::fs::remove_file(env.path("ps.fail")).unwrap();
    std::fs::write(env.path("a.release"), "").unwrap();
    assert!(big.finish().status.success());
    assert!(second.finish().status.success());
}

#[test]
fn an_empty_rss_reading_keeps_a_recovered_lease_whose_wrapper_lives() {
    let env = Env::new();
    let path = fake_ps(&env);
    let daemon = env.spawn_daemon_with(&[("PATH", &path)]);

    let running = env.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    running.signal("-STOP");
    daemon.stop();
    let _daemon = env.spawn_daemon_with(&[("PATH", &path)]);
    let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
    second.wait_stderr("heavy: queued at position 1,");

    std::fs::write(env.path("ps.empty"), "").unwrap();
    wait_until(Duration::from_secs(20), "an empty RSS reading", || {
        line_count(&env.path("ps.emptied")) >= 1
    });
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !env.path("b.started").exists(),
        "an empty RSS reading released the paused command's lease"
    );

    std::fs::remove_file(env.path("ps.empty")).unwrap();
    running.signal("-CONT");
    running.wait_stderr("heavy: re-registered");
    std::fs::write(env.path("a.release"), "").unwrap();
    assert!(running.finish().status.success());
    assert!(second.finish().status.success());
}

#[test]
fn a_recovered_wrapper_keeps_its_measured_rss_when_it_re_registers() {
    let env = Env::new();
    let path = fake_ps(&env);
    let daemon = env.spawn_daemon_with(&[("PATH", &path)]);

    let script = format!("echo $$ > a.pid; {}", held("a"));
    let running = env.heavy(&["0.1", "--", "sh", "-c", &script], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    running.signal("-STOP");
    daemon.stop();
    let _daemon = env.spawn_daemon_with(&[("PATH", &path)]);
    std::fs::copy(env.path("a.pid"), env.path("ps.big")).unwrap();
    wait_until(
        Duration::from_secs(20),
        "the restarted daemon reads its RSS",
        || line_count(&env.path("ps.served")) >= 2,
    );

    let second = env.heavy(&["0.2", "--", "sh", "-c", "touch b.started"], None);
    second.wait_stderr("heavy: queued at position 1,");
    running.signal("-CONT");
    running.wait_stderr("heavy: re-registered");
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        !env.path("b.started").exists(),
        "re-registration dropped the measured RSS and admitted a command over budget"
    );

    std::fs::remove_file(env.path("ps.big")).unwrap();
    wait_until(
        Duration::from_secs(20),
        "the waiter runs once the RSS falls",
        || env.path("b.started").exists(),
    );
    assert!(second.finish().status.success());
    std::fs::write(env.path("a.release"), "").unwrap();
    assert!(running.finish().status.success());
}

#[test]
fn a_daemon_that_never_answers_counts_as_absent() {
    for answer in [Answer::Nothing, Answer::HelloOnly] {
        let env = Env::new();
        let _peer = FakeDaemon::bind(&env, answer);
        let done = env
            .heavy(&["--", "sh", "-c", "echo ran; exit 3"], None)
            .finish();
        assert_eq!(done.status.code(), Some(3), "{answer:?}: {}", done.stderr);
        assert_eq!(done.stdout, "ran\n");
        assert!(
            done.stderr.contains("running without a memory lease"),
            "{}",
            done.stderr
        );
    }
}

#[test]
fn a_daemon_from_before_heavy_is_refused_at_the_handshake() {
    let env = Env::new();
    let old = FakeDaemon::bind(&env, Answer::Protocol(PRE_HEAVY_PROTOCOL));
    let started = Instant::now();
    let done = env
        .heavy(&["--", "sh", "-c", "echo ran; exit 3"], None)
        .finish();
    assert_eq!(
        old.accepted(),
        1,
        "heavy kept reconnecting to the old daemon; stderr:\n{}",
        done.stderr
    );
    assert_eq!(done.status.code(), Some(3), "{}", done.stderr);
    assert_eq!(done.stdout, "ran\n");
    assert!(
        done.stderr
            .contains(&format!("speaks protocol v{PRE_HEAVY_PROTOCOL}")),
        "{}",
        done.stderr
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn signals_to_a_wrapper_nested_in_a_shell_reach_the_whole_command_tree() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let outer =
        r#""$0" heavy -- sh -c "$1" & echo $! > wrapper.pid; wait $!; echo "wrapper-rc=$?""#;

    let mut survivors = Vec::new();
    for (sig, rc) in [("-TERM", 143), ("-HUP", 129)] {
        let _ = std::fs::remove_file(env.path("grandchild.pid"));
        let inner = format!(
            "({}) >/dev/null 2>&1 & echo $! > grandchild.pid; wait",
            held("g")
        );
        let run = env.heavy_at(
            Path::new("/bin/sh"),
            &["-c", outer, env!("CARGO_BIN_EXE_nebula")],
            &[&inner],
            None,
        );
        wait_until(Duration::from_secs(20), "grandchild starts", || {
            std::fs::read_to_string(env.path("grandchild.pid")).is_ok_and(|p| p.ends_with('\n'))
        });
        let wrapper = std::fs::read_to_string(env.path("wrapper.pid")).unwrap();
        let grandchild = std::fs::read_to_string(env.path("grandchild.pid")).unwrap();
        let (wrapper, grandchild) = (wrapper.trim(), grandchild.trim());
        assert!(Command::new("kill")
            .args([sig, wrapper])
            .status()
            .unwrap()
            .success());

        wait_until(Duration::from_secs(20), "wrapper exits", || {
            is_gone(wrapper)
        });
        if (0..250).all(|_| {
            std::thread::sleep(Duration::from_millis(20));
            !is_gone(grandchild)
        }) {
            survivors.push(format!("{sig} left grandchild {grandchild} running"));
        }
        std::fs::write(env.path("g.release"), "").unwrap();
        let done = run.finish();
        assert_eq!(done.stdout, format!("wrapper-rc={rc}\n"), "{}", done.stderr);
        wait_until(Duration::from_secs(10), "grandchild exits", || {
            is_gone(grandchild)
        });
        let _ = std::fs::remove_file(env.path("g.release"));
    }
    assert!(survivors.is_empty(), "{survivors:#?}");
}

#[test]
fn term_to_the_wrapper_ends_a_stopped_command() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();

    let run = env.heavy(
        &["--", "sh", "-c", "echo $$ > child.pid; kill -STOP $$"],
        None,
    );
    wait_until(Duration::from_secs(20), "command stops itself", || {
        std::fs::read_to_string(env.path("child.pid"))
            .is_ok_and(|pid| pid.ends_with('\n') && process_state(pid.trim()).starts_with('T'))
    });
    run.signal("-TERM");
    let done = run.finish();
    assert_eq!(done.status.signal(), Some(15), "{}", done.stderr);
}

#[test]
fn a_terminal_reaches_the_command_and_comes_back_to_the_caller() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let script = r#"
        "$0" heavy -- sh -c 'echo ready; read line; echo "got $line"; exit 7'; echo "rc=$?"
        read line; echo "caller read $line"
        "$0" heavy -- sh -c 'trap "echo INT >> sigs" INT; trap "echo QUIT >> sigs" QUIT; echo armed; while [ ! -f stop ]; do sleep 0.05; done'; echo "rc=$?"
        "$0" heavy -- sh -c 'echo sleeping; exec sleep 30'; echo "rc=$?"
        "$0" heavy -- sh -c 'echo sleeping; exec sleep 30'; echo "rc=$?"
    "#;
    let mut tty = Tty::spawn(&env, script);

    tty.wait_for("ready");
    tty.type_keys("hello\r");
    tty.wait_for("got hello");
    tty.wait_for("rc=7");
    tty.type_keys("back\r");
    tty.wait_for("caller read back");

    tty.wait_for("armed");
    let sigs = env.path("sigs");
    tty.type_keys("\x03");
    wait_until(Duration::from_secs(10), "child traps INT", || {
        std::fs::read_to_string(&sigs).is_ok_and(|s| s.lines().count() >= 1)
    });
    tty.type_keys("\x1c");
    wait_until(Duration::from_secs(10), "child traps QUIT", || {
        std::fs::read_to_string(&sigs).is_ok_and(|s| s.lines().count() >= 2)
    });
    std::fs::write(env.path("stop"), "").unwrap();
    tty.wait_for("rc=0");
    assert_eq!(std::fs::read_to_string(&sigs).unwrap(), "INT\nQUIT\n");

    tty.wait_for("sleeping");
    tty.type_keys("\x03");
    tty.wait_for("rc=130");
    tty.wait_for_nth("sleeping", 2);
    tty.type_keys("\x1c");
    tty.wait_for("rc=131");
}

#[test]
fn ctrl_z_suspends_the_wrapper_as_a_job_and_fg_resumes_the_command() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let script = r#"
        set -m
        "$0" heavy -- sh -c 'echo ready; read line; echo "got $line"; exit 7'; echo "stopped-rc=$?"
        read line; echo "caller read $line"
        fg; echo "fg-rc=$?"
    "#;
    let mut tty = Tty::spawn(&env, script);

    tty.wait_for("ready");
    tty.type_keys("\x1a");
    tty.wait_for(&format!("stopped-rc={}", 128 + SIGTSTP));
    tty.type_keys("back\r");
    tty.wait_for("caller read back");
    tty.type_keys("hello\r");
    tty.wait_for("got hello");
    tty.wait_for("fg-rc=7");
}

#[test]
fn a_command_started_in_the_background_gets_the_terminal_on_fg() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let script = r#"
        set -m
        "$0" heavy -- sh -c 'echo $$ > cmd.pid; echo ready; read line; echo "got $line"; exit 7' &
        read line; echo "caller read $line"
        fg; echo "fg-rc=$?"
    "#;
    let mut tty = Tty::spawn(&env, script);

    tty.wait_for("ready");
    let command = std::fs::read_to_string(env.path("cmd.pid")).unwrap();
    let command = command.trim();
    wait_until(
        Duration::from_secs(10),
        "a background read stops the command",
        || process_state(command).starts_with('T'),
    );
    tty.type_keys("go\r");
    tty.wait_for("caller read go");
    wait_until(Duration::from_secs(10), "fg resumes the command", || {
        !process_state(command).starts_with('T')
    });
    tty.type_keys("hello\r");
    tty.wait_for("got hello");
    tty.wait_for("fg-rc=7");
}

#[test]
fn signals_the_caller_left_ignored_stay_ignored_and_others_are_forwarded() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let background = r#""$0" heavy -- sh -c "$1" & wait $!; echo "rc=$?""#;
    let nohup = r#"trap '' HUP; "$0" heavy -- sh -c "$1"; echo "rc=$?""#;
    let foreground = r#""$0" heavy -- sh -c "$1"; echo "rc=$?""#;
    let inner = format!("echo $PPID > wrapper.pid; {}; echo finished", held("c"));

    let mut wrong = Vec::new();
    for (caller, sig, expected) in [
        (background, "-INT", "finished\nrc=0\n"),
        (background, "-QUIT", "finished\nrc=0\n"),
        (nohup, "-HUP", "finished\nrc=0\n"),
        (foreground, "-INT", "rc=130\n"),
        (foreground, "-QUIT", "rc=131\n"),
    ] {
        let _ = std::fs::remove_file(env.path("wrapper.pid"));
        let _ = std::fs::remove_file(env.path("c.release"));
        let run = env.heavy_at(
            Path::new("/bin/sh"),
            &["-c", caller, env!("CARGO_BIN_EXE_nebula")],
            &[&inner],
            None,
        );
        wait_until(Duration::from_secs(20), "command starts", || {
            std::fs::read_to_string(env.path("wrapper.pid")).is_ok_and(|p| p.ends_with('\n'))
        });
        let wrapper = std::fs::read_to_string(env.path("wrapper.pid")).unwrap();
        assert!(Command::new("kill")
            .args([sig, wrapper.trim()])
            .status()
            .unwrap()
            .success());
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(env.path("c.release"), "").unwrap();
        let done = run.finish();
        if done.stdout != expected {
            wrong.push(format!(
                "{caller} got {sig}: stdout {:?}, want {expected:?}",
                done.stdout
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn malformed_reservation_is_rejected_before_any_ipc() {
    let env = Env::new();
    std::fs::create_dir_all(env.runtime_dir()).unwrap();
    let socket =
        std::os::unix::net::UnixListener::bind(env.runtime_dir().join("daemon.sock")).unwrap();
    socket.set_nonblocking(true).unwrap();

    for gb in [
        "abc",
        "",
        "0",
        "-1",
        "nan",
        "inf",
        "1e-30",
        "1e300",
        "17179869184",
    ] {
        let done = env.heavy(&[gb, "--", "touch", "ran"], None).finish();
        assert_eq!(done.status.code(), Some(2), "{gb:?}: {}", done.stderr);
        assert!(done.stderr.contains("GB"), "{gb:?}: {}", done.stderr);
        assert!(!env.path("ran").exists(), "{gb:?} ran the command");
        assert!(socket.accept().is_err(), "{gb:?} reached the daemon socket");
    }
}

#[test]
fn a_command_that_drops_its_environment_keeps_its_lease_across_a_daemon_restart() {
    let inner = format!("echo run >> runs; {}", held("a"));
    for scrub in [
        format!("exec env -i PATH=/usr/bin:/bin sh -c '{inner}'"),
        format!("unset NEBULA_HEAVY_LEASE; exec sh -c '{inner}'"),
    ] {
        let env = Env::new();
        let daemon = env.spawn_daemon();
        let running = env.heavy(&["--", "sh", "-c", &scrub], None);
        wait_until(Duration::from_secs(20), "first command starts", || {
            env.path("a.started").exists()
        });
        daemon.stop();
        let _daemon = env.spawn_daemon();
        running.wait_stderr("heavy: re-registered");

        let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
        second.wait_stderr("heavy: queued at position 1,");
        assert!(
            !env.path("b.started").exists(),
            "{scrub}: a second command ran beside the first after the restart"
        );
        std::fs::write(env.path("a.release"), "").unwrap();
        assert!(running.finish().status.success());
        assert!(second.finish().status.success());
        assert_eq!(std::fs::read_to_string(env.path("runs")).unwrap(), "run\n");
    }
}

#[test]
fn a_daemon_counts_only_the_wrappers_that_ask_it() {
    let busy = Env::new();
    let _busy_daemon = busy.spawn_daemon();
    let running = busy.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        busy.path("a.started").exists()
    });

    let other = Env::new();
    let _other_daemon = other.spawn_daemon();
    let done = other.heavy(&["--", "true"], None).finish();
    assert!(done.status.success(), "{}", done.stderr);
    assert!(
        !done.stderr.contains("queued"),
        "a daemon counted another daemon's wrapper: {}",
        done.stderr
    );
    std::fs::write(busy.path("a.release"), "").unwrap();
    assert!(running.finish().status.success());
}

#[test]
fn a_grant_left_unread_by_a_stopped_wrapper_still_counts_after_a_daemon_restart() {
    let env = Env::new();
    let daemon = env.spawn_daemon();

    let first = env.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    let granted = env.heavy(
        &["--", "sh", "-c", &format!("echo g >> runs; {}", held("g"))],
        None,
    );
    granted.wait_stderr("heavy: queued at position 1,");
    granted.signal("-STOP");
    let newcomer = env.heavy(&["--", "sh", "-c", "echo n >> runs"], None);
    newcomer.wait_stderr("heavy: queued at position 2,");

    std::fs::write(env.path("a.release"), "").unwrap();
    assert!(first.finish().status.success());
    newcomer.wait_stderr("heavy: queued at position 1,");
    daemon.stop();
    let _daemon = env.spawn_daemon();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !env.path("runs").exists(),
        "the restarted daemon admitted a newcomer while a stopped wrapper held an unread grant"
    );

    granted.signal("-CONT");
    granted.wait_stderr("heavy: re-registered");
    wait_until(Duration::from_secs(20), "granted command starts", || {
        env.path("g.started").exists()
    });
    assert_eq!(std::fs::read_to_string(env.path("runs")).unwrap(), "g\n");
    std::fs::write(env.path("g.release"), "").unwrap();
    assert!(granted.finish().status.success());
    assert!(newcomer.finish().status.success());
    assert_eq!(std::fs::read_to_string(env.path("runs")).unwrap(), "g\nn\n");
}

#[test]
fn a_default_wrapper_blocks_every_admission_until_it_re_registers_after_a_restart() {
    let env = Env::new();
    let config = env.data_dir().join("config.json");
    let daemon = env.spawn_daemon();
    let running = env.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(Duration::from_secs(20), "first command starts", || {
        env.path("a.started").exists()
    });
    running.signal("-STOP");
    daemon.stop();
    let raised_budget = ONE_COMMAND_BUDGET.replace(
        "\"command_reservation_gib\":0.6",
        "\"command_reservation_gib\":0.1",
    );
    let shrunk_budget = raised_budget.replace("\"budget_gib\":1.0", "\"budget_gib\":0.5");
    std::fs::write(&config, shrunk_budget).unwrap();
    let _daemon = env.spawn_daemon();

    let fits_beside_the_grant = env.heavy(&["0.4", "--", "sh", "-c", &held("b")], None);
    fits_beside_the_grant.wait_stderr("heavy: queued at position 1,");
    std::fs::write(&config, raised_budget).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !env.path("b.started").exists(),
        "after the budget was raised to 1.0 GiB, 0.4 GiB ran beside a paused wrapper whose 0.6 GiB grant is unknown to the restarted daemon"
    );
    let fills_the_budget = env.heavy(&["0.1", "--", "sh", "-c", "touch c.started"], None);
    fills_the_budget.wait_stderr("heavy: queued at position 2,");

    running.signal("-CONT");
    running.wait_stderr("heavy: re-registered");
    wait_until(
        Duration::from_secs(10),
        "0.4 GiB fits beside the re-registered 0.6 GiB grant",
        || env.path("b.started").exists(),
    );
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        !env.path("c.started").exists(),
        "the re-registered wrapper was charged less than its 0.6 GiB grant"
    );
    std::fs::write(env.path("a.release"), "").unwrap();
    assert!(running.finish().status.success());
    wait_until(
        Duration::from_secs(10),
        "freed grant admits the last command",
        || env.path("c.started").exists(),
    );
    std::fs::write(env.path("b.release"), "").unwrap();
    assert!(fits_beside_the_grant.finish().status.success());
    assert!(fills_the_budget.finish().status.success());
}

#[test]
fn heavy_inside_heavy_runs_under_the_outer_lease() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let link = env.path("heavy");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_nebula"), &link).unwrap();
    let nebula = env!("CARGO_BIN_EXE_nebula");
    let link = link.to_str().unwrap();
    let inner = format!("{}; echo inner; echo inner-err >&2; exit 5", held("n"));

    for nested in [
        vec![nebula, "heavy"],
        vec![link],
        vec!["env", "-u", "NEBULA_HEAVY_LEASE", nebula, "heavy"],
    ] {
        for leftover in ["n.started", "n.release", "b.started"] {
            let _ = std::fs::remove_file(env.path(leftover));
        }
        let mut args = vec!["--"];
        args.extend(&nested);
        args.extend(["--", "sh", "-c", &inner]);
        let outer = env.heavy(&args, None);
        wait_until(Duration::from_secs(20), "nested command starts", || {
            env.path("n.started").exists()
        });
        let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
        second.wait_stderr("heavy: queued at position 1,");
        assert!(
            !env.path("b.started").exists(),
            "{nested:?}: a command ran beside the nested one"
        );

        std::fs::write(env.path("n.release"), "").unwrap();
        let done = outer.finish();
        assert_eq!(done.status.code(), Some(5), "{nested:?}: {}", done.stderr);
        assert_eq!(done.stdout, "inner\n", "{nested:?}");
        assert!(done.stderr.contains("inner-err\n"), "{}", done.stderr);
        assert_eq!(wait_lines(&done.stderr), 1, "{nested:?}: {}", done.stderr);
        assert!(second.finish().status.success());
        assert!(env.path("b.started").exists());
    }
}

#[test]
fn fg_right_after_a_background_start_hands_the_command_the_terminal() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let script = r#"
        set -m
        "$0" heavy -- sh -c 'echo $$ > cmd.pid; echo ready; while [ ! -f go ]; do sleep 0.05; done; read line; echo "got $line"; exit 7' &
        read line; echo "caller read $line"
        fg; echo "fg-rc=$?"
    "#;
    let mut tty = Tty::spawn(&env, script);

    tty.wait_for("ready");
    let command = std::fs::read_to_string(env.path("cmd.pid")).unwrap();
    let command = command.trim();
    tty.type_keys("x\r");
    tty.wait_for("caller read x");
    wait_until(
        Duration::from_secs(10),
        "fg hands the terminal to the command before it reads",
        || terminal_foreground(command) == command,
    );
    std::fs::write(env.path("go"), "").unwrap();
    tty.type_keys("hello\r");
    tty.wait_for("got hello");
    tty.wait_for("fg-rc=7");
}

#[test]
fn an_ignored_sigpipe_stays_ignored_and_a_default_one_stays_default() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let command = r#"exec "$0" heavy -- sh -c 'kill -PIPE $$; echo survived'"#;

    let ignored = env
        .heavy_at(
            Path::new("/bin/sh"),
            &[
                "-c",
                &format!("trap '' PIPE; {command}"),
                env!("CARGO_BIN_EXE_nebula"),
            ],
            &[],
            None,
        )
        .finish();
    assert_eq!(ignored.status.code(), Some(0), "{}", ignored.stderr);
    assert_eq!(ignored.stdout, "survived\n");

    let default = env
        .heavy_at(
            Path::new("/bin/sh"),
            &["-c", command, env!("CARGO_BIN_EXE_nebula")],
            &[],
            None,
        )
        .finish();
    assert_eq!(default.status.signal(), Some(13), "{}", default.stderr);
    assert_eq!(default.stdout, "");
}

#[test]
fn an_ignored_sigchld_keeps_exact_statuses_and_stays_ignored_in_the_command() {
    let env = Env::new();
    let _daemon = env.spawn_daemon();
    let ignored = r#"trap '' CHLD; exec "$0" heavy -- bash -c "$1""#;
    let default = r#"exec "$0" heavy -- bash -c "$1""#;
    let nested = r#"trap '' CHLD; exec "$0" heavy -- "$0" heavy -- bash -c "$1""#;
    let shown = "trap -- '' SIGCHLD\n";
    let one_wait = 1;

    let mut wrong = Vec::new();
    for (caller, command, stdout, code, signal) in [
        (ignored, "trap -p CHLD; exit 0", shown, Some(0), None),
        (ignored, "trap -p CHLD; exit 7", shown, Some(7), None),
        (
            ignored,
            "trap -p CHLD; kill -TERM $$",
            shown,
            None,
            Some(15),
        ),
        (default, "trap -p CHLD; exit 7", "", Some(7), None),
        (nested, "trap -p CHLD; exit 7", shown, Some(7), None),
    ] {
        let done = env
            .heavy_at(
                Path::new("/bin/bash"),
                &["-c", caller, env!("CARGO_BIN_EXE_nebula")],
                &[command],
                None,
            )
            .finish();
        let got = (
            done.stdout.as_str(),
            done.status.code(),
            done.status.signal(),
            wait_lines(&done.stderr),
        );
        if got != (stdout, code, signal, one_wait) {
            wrong.push(format!(
                "{caller} ran {command:?}: got {got:?}, want {:?}; stderr {:?}",
                (stdout, code, signal, one_wait),
                done.stderr
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_running_command_stops_rejoining_a_daemon_that_came_back_on_another_protocol() {
    for version in [PRE_HEAVY_PROTOCOL, PROTOCOL_VERSION + 1] {
        let env = Env::new();
        let daemon = env.spawn_daemon();
        let script = format!("echo run >> runs; {}; exit 7", held("a"));
        let running = env.heavy(&["--", "sh", "-c", &script], None);
        wait_until(Duration::from_secs(20), "command starts", || {
            env.path("a.started").exists()
        });
        daemon.stop();
        running.wait_stderr("heavy: lost the nebula daemon");

        let other = FakeDaemon::bind(&env, Answer::Protocol(version));
        wait_until(
            Duration::from_secs(10),
            "heavy reaches the new daemon",
            || other.accepted() >= 1,
        );
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(
            other.accepted(),
            1,
            "v{version}: heavy kept handshaking with a daemon it cannot talk to; stderr:\n{}",
            running.stderr()
        );

        std::fs::write(env.path("a.release"), "").unwrap();
        let done = running.finish();
        assert_eq!(done.status.code(), Some(7), "v{version}: {}", done.stderr);
        let warning = format!("speaks protocol v{version}");
        assert_eq!(
            done.stderr.matches(&warning).count(),
            1,
            "v{version}: {}",
            done.stderr
        );
        assert!(
            done.stderr
                .contains("the command keeps running without a memory lease"),
            "v{version}: {}",
            done.stderr
        );
        assert_eq!(wait_lines(&done.stderr), 1, "v{version}: {}", done.stderr);
        assert_eq!(
            std::fs::read_to_string(env.path("runs")).unwrap(),
            "run\n",
            "v{version}: the command ran more than once"
        );
    }
}

#[test]
fn a_counted_wrapper_cannot_re_register_for_less_than_it_was_counted() {
    let reservation = (0.6 * (1u64 << 30) as f64) as u64;
    for (mark, expected) in [(Some(reservation), "refused"), (None, "granted")] {
        let env = Env::new();
        let socket = env.runtime_dir().join("daemon.sock");
        let mut peer = Command::new(std::env::current_exe().unwrap());
        peer.args(["--exact", "counted_claim_peer", "--ignored", "--nocapture"])
            .current_dir(env.tmp.path())
            .env(PEER_SOCKET, &socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(bytes) = mark {
            peer.env(PEER_MARK, bytes.to_string());
        }
        let mut peer = peer.spawn().unwrap();
        wait_until(Duration::from_secs(20), "the peer marks itself", || {
            env.path("claim.ready").exists()
        });
        let _daemon = env.spawn_daemon();
        std::fs::write(env.path("claim.go"), "").unwrap();
        wait_until(Duration::from_secs(20), "the claim is answered", || {
            std::fs::read_to_string(env.path("claim.verdict")).is_ok_and(|v| v.ends_with('\n'))
        });
        let verdict = std::fs::read_to_string(env.path("claim.verdict")).unwrap();
        assert_eq!(verdict.trim(), expected, "mark {mark:?}");

        let second = env.heavy(&["--", "sh", "-c", "touch b.started"], None);
        second.wait_stderr("heavy: queued at position 1,");
        assert!(
            !env.path("b.started").exists(),
            "mark {mark:?}: a 1-byte claim freed the counted wrapper's reservation"
        );
        std::fs::write(env.path("claim.done"), "").unwrap();
        assert!(peer.wait().unwrap().success());
        assert!(second.finish().status.success());
        assert!(env.path("b.started").exists());
    }
}

#[test]
fn heavy_nested_under_another_daemons_lease_queues_at_its_own_daemon() {
    let outer_env = Env::new();
    let _outer_daemon = outer_env.spawn_daemon();
    let busy = Env::new();
    let _busy_daemon = busy.spawn_daemon();
    let holder = busy.heavy(&["--", "sh", "-c", &held("a")], None);
    wait_until(
        Duration::from_secs(20),
        "the other daemon's command starts",
        || busy.path("a.started").exists(),
    );

    let inner = format!(
        r#"NEBULA_RUNTIME_DIR={} NEBULA_DATA_DIR={} "$0" heavy -- touch inner.started"#,
        busy.runtime_dir().display(),
        busy.data_dir().display()
    );
    let outer = outer_env.heavy(
        &["--", "sh", "-c", &inner, env!("CARGO_BIN_EXE_nebula")],
        None,
    );
    outer.wait_stderr("heavy: queued at position 1,");
    assert!(
        !outer_env.path("inner.started").exists(),
        "a command nested under one daemon's lease skipped another daemon's queue"
    );

    std::fs::write(busy.path("a.release"), "").unwrap();
    assert!(holder.finish().status.success());
    let done = outer.finish();
    assert!(done.status.success(), "{}", done.stderr);
    assert!(outer_env.path("inner.started").exists());
}

const PEER_SOCKET: &str = "HEAVY_TEST_PEER_SOCKET";

#[test]
#[ignore = "a peer process for re_registration_is_refused_to_any_peer_the_daemon_did_not_count_at_startup"]
fn forged_claim_peer() {
    let Some(socket) = std::env::var_os(PEER_SOCKET) else {
        return;
    };
    reexec_with_own_mark(Path::new(&socket), None);
    let events = claim(Path::new(&socket), re_registration());
    println!("claim answered: {} {events:?}", verdict(&events));
}

const PEER_MARK: &str = "HEAVY_TEST_PEER_MARK";

#[test]
#[ignore = "a peer process for a_counted_wrapper_cannot_re_register_for_less_than_it_was_counted"]
fn counted_claim_peer() {
    let Some(socket) = std::env::var_os(PEER_SOCKET) else {
        return;
    };
    let socket = Path::new(&socket);
    let reserve_bytes = std::env::var(PEER_MARK).ok().map(|b| b.parse().unwrap());
    reexec_with_own_mark(socket, reserve_bytes);
    std::fs::write("claim.ready", "").unwrap();
    while !Path::new("claim.go").exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    runtime().block_on(async {
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        let events = exchange(
            &mut stream,
            ClientRequest::HeavyRunning {
                reserved_bytes: 1,
                command: "claim".into(),
            },
        )
        .await;
        std::fs::write("claim.verdict", format!("{}\n", verdict(&events))).unwrap();
        while !Path::new("claim.done").exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}

fn reexec_with_own_mark(socket: &Path, reserve_bytes: Option<u64>) {
    let mark = HeavyMark {
        wrapper: std::process::id(),
        reserve_bytes,
        daemon: daemon_id(socket),
    };
    let marked = std::env::var(HeavyMark::ENV)
        .ok()
        .and_then(|v| HeavyMark::from_value(&v));
    if marked != Some(mark) {
        let e = Command::new(std::env::current_exe().unwrap())
            .args(std::env::args_os().skip(1))
            .env(HeavyMark::ENV, mark.value())
            .exec();
        panic!("re-exec with a forged mark: {e}");
    }
}

fn re_registration() -> ClientRequest {
    ClientRequest::HeavyRunning {
        reserved_bytes: (0.6 * (1u64 << 30) as f64) as u64,
        command: "claim".into(),
    }
}

fn verdict(events: &[ServerEvent]) -> &'static str {
    if events
        .iter()
        .any(|e| matches!(e, ServerEvent::HeavyGranted { .. }))
    {
        "granted"
    } else if events
        .iter()
        .any(|e| matches!(e, ServerEvent::HeavyRunningRefused))
    {
        "refused"
    } else {
        "no answer"
    }
}

fn terminal_foreground(pid: &str) -> String {
    let out = Command::new("ps")
        .args(["-o", "tpgid=", "-p", pid])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn line_count(path: &Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |t| t.lines().count())
}

fn real_ps() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("ps"))
        .find(|ps| ps.is_file())
        .unwrap()
}

fn fake_ps(env: &Env) -> String {
    let bin = env.path("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake_ps = bin.join("ps");
    std::fs::write(
        &fake_ps,
        format!(
            r#"#!/bin/sh
cd {dir}
if [ -f ps.fail ]; then echo >> ps.failed; exit 1; fi
if [ -f ps.empty ]; then echo >> ps.emptied; exit 0; fi
big=$(cat ps.big 2>/dev/null)
[ -n "$big" ] && echo >> ps.served
{real} "$@" | awk -v big="$big" '$1 == big {{ $3 = 1000000 }} {{ print }}'
"#,
            dir = env.tmp.path().display(),
            real = real_ps().display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &fake_ps,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn claim(socket: &Path, request: ClientRequest) -> Vec<ServerEvent> {
    runtime().block_on(async {
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        exchange(&mut stream, request).await
    })
}

async fn exchange(stream: &mut tokio::net::UnixStream, request: ClientRequest) -> Vec<ServerEvent> {
    for request in [
        ClientRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
        request,
    ] {
        write_frame(stream, &request).await.unwrap();
    }
    let mut events = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(Ok(Some(event))) =
        tokio::time::timeout_at(deadline, read_frame::<ServerEvent, _>(stream)).await
    {
        let answered = matches!(
            event,
            ServerEvent::HeavyGranted { .. } | ServerEvent::HeavyRunningRefused
        );
        events.push(event);
        if answered {
            break;
        }
    }
    events
}

const PRE_HEAVY_PROTOCOL: u32 = 29;

#[derive(Debug, Clone, Copy)]
enum Answer {
    Nothing,
    HelloOnly,
    Protocol(u32),
}

struct FakeDaemon {
    accepted: Arc<AtomicUsize>,
    _thread: JoinHandle<()>,
}

impl FakeDaemon {
    fn bind(env: &Env, answer: Answer) -> Self {
        std::fs::create_dir_all(env.runtime_dir()).unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(env.runtime_dir().join("daemon.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        let thread = std::thread::spawn(move || {
            runtime().block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                let mut held = Vec::new();
                while let Ok((mut stream, _)) = listener.accept().await {
                    counter.fetch_add(1, Ordering::SeqCst);
                    match answer {
                        Answer::Nothing => held.push(stream),
                        Answer::HelloOnly => {
                            let _ = read_frame::<ClientRequest, _>(&mut stream).await;
                            let _ = write_frame(
                                &mut stream,
                                &ServerEvent::HelloOk {
                                    protocol_version: PROTOCOL_VERSION,
                                    daemon_pid: 1,
                                },
                            )
                            .await;
                            held.push(stream);
                        }
                        Answer::Protocol(version) => {
                            let reply = match read_frame::<ClientRequest, _>(&mut stream).await {
                                Ok(Some(ClientRequest::Hello { protocol_version }))
                                    if protocol_version == version =>
                                {
                                    ServerEvent::HelloOk {
                                        protocol_version: version,
                                        daemon_pid: 1,
                                    }
                                }
                                _ => ServerEvent::Incompatible {
                                    daemon_protocol_version: version,
                                },
                            };
                            let _ = write_frame(&mut stream, &reply).await;
                        }
                    }
                }
            })
        });
        Self {
            accepted,
            _thread: thread,
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

extern "C" {
    fn setsid() -> i32;
}

struct Tty {
    writer: Box<dyn Write + Send>,
    screen: Arc<Mutex<String>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

impl Tty {
    fn spawn(env: &Env, script: &str) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args(["-c", script, env!("CARGO_BIN_EXE_nebula")]);
        cmd.cwd(env.tmp.path());
        cmd.env("NEBULA_RUNTIME_DIR", env.runtime_dir());
        cmd.env("NEBULA_DATA_DIR", env.data_dir());
        cmd.env_remove("NEBULA_AGENT_ID");
        let child = pty.slave.spawn_command(cmd).unwrap();
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().unwrap();
        let writer = pty.master.take_writer().unwrap();
        let screen = Arc::new(Mutex::new(String::new()));
        let sink = screen.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                sink.lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        });
        Self {
            writer,
            screen,
            child,
            _master: pty.master,
        }
    }

    fn type_keys(&mut self, keys: &str) {
        self.writer.write_all(keys.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    fn wait_for(&self, needle: &str) {
        self.wait_for_nth(needle, 1);
    }

    fn wait_for_nth(&self, needle: &str, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.screen.lock().unwrap().matches(needle).count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let screen = self.screen.lock().unwrap().clone();
        panic!("terminal never showed {needle:?} x{n}; got:\n{screen}");
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
