//! Cards on the board host: the `nebula todo` verbs against a daemon whose
//! config says `board = "local"`, through the real CLI and its --json.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const LIVE_SESSION_ENV: [&str; 4] = [
    "NEBULA_AGENT_ID",
    "NEBULA_API_URL",
    "NEBULA_API_TOKEN",
    "CLAUDE_CODE_EXPERIMENTAL_CODE_MODE",
];

struct Env {
    tmp: tempfile::TempDir,
    daemon: Option<Child>,
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }
}

impl Env {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
            daemon: None,
        }
    }

    fn runtime_dir(&self) -> PathBuf {
        self.tmp.path().join("rt")
    }

    fn data_dir(&self) -> PathBuf {
        self.tmp.path().join("data")
    }

    fn write_config(&self, json: &str) {
        std::fs::create_dir_all(self.data_dir()).unwrap();
        std::fs::write(self.data_dir().join("config.json"), json).unwrap();
    }

    fn board_local(&self) {
        self.write_config(r#"{"board": "local"}"#);
    }

    fn start(mut self, envs: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nebula"));
        cmd.args(["daemon", "--foreground"])
            .env("NEBULA_RUNTIME_DIR", self.runtime_dir())
            .env("NEBULA_DATA_DIR", self.data_dir())
            .env("SHELL", "/bin/sh")
            .env("NEBULA_AGENT_CMD", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for k in LIVE_SESSION_ENV {
            cmd.env_remove(k);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        self.daemon = Some(cmd.spawn().unwrap());
        wait_for(&self.runtime_dir().join("daemon.sock"));
        self
    }

    fn cli(&self, args: &[&str], agent: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nebula"));
        cmd.args(args)
            .env("NEBULA_RUNTIME_DIR", self.runtime_dir())
            .env("NEBULA_DATA_DIR", self.data_dir())
            .current_dir(self.tmp.path());
        for k in LIVE_SESSION_ENV {
            cmd.env_remove(k);
        }
        if let Some(id) = agent {
            cmd.env("NEBULA_AGENT_ID", id);
        }
        cmd.output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        json_of(self.cli(args, None), args)
    }

    fn make_repo(&self) -> PathBuf {
        let repo = self.tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.email", "test@nebula.dev"],
            &["config", "user.name", "nebula-test"],
            &["commit", "--allow-empty", "-m", "init"],
        ] {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        let added = self.cli(&["add", repo.to_str().unwrap()], None);
        assert!(added.status.success(), "{}", stderr(&added));
        repo
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the one wait in this file: the daemon's socket appearing"
)]
fn wait_for(sock: &Path) {
    for _ in 0..200 {
        if std::os::unix::net::UnixStream::connect(sock).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("daemon socket never appeared");
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json_of(out: Output, args: &[&str]) -> Value {
    assert!(
        out.status.success(),
        "nebula {args:?} failed: {}",
        stderr(&out)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("nebula {args:?} printed no JSON ({e}): {:?}", out.stdout))
}

fn board_env() -> Env {
    let env = Env::new();
    env.board_local();
    env.start(&[])
}

fn add(env: &Env, args: &[&str], title: &str) -> Value {
    let mut argv = vec!["todo", "add", "--json"];
    argv.extend_from_slice(args);
    argv.push(title);
    env.json(&argv)
}

fn titles(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap().to_string())
        .collect()
}

fn list(env: &Env, scope: &[&str]) -> Vec<String> {
    let mut argv = vec!["todo", "list", "--json"];
    argv.extend_from_slice(scope);
    let mut found = titles(&env.json(&argv)["items"]);
    found.sort();
    found
}

#[test]
fn card_add_idempotent() {
    let env = board_env();
    let card = add(&env, &["--project", "nebula"], "T");
    assert_eq!(card["number"], 1);
    assert_eq!(card["ref"], "#1");
    assert_eq!(card["stage"], "backlog");
    assert_eq!(card["title"], "T");
    assert_eq!(card["project"], "nebula");
    let id = card["id"].as_str().unwrap();
    assert!(ulid::Ulid::from_string(id).is_ok(), "{id}");

    let again = add(&env, &["--project", "nebula", "--id", id], "T");
    assert_eq!(again, card);
    let listed = env.json(&["todo", "list", "--all", "--json"]);
    assert_eq!(listed["items"].as_array().unwrap().len(), 1, "{listed}");
}

#[test]
fn card_move_stages() {
    let env = board_env();
    add(&env, &["--project", "nebula"], "Move me");
    for stage in ["next", "doing", "review", "done", "backlog"] {
        let card = env.json(&["todo", "move", "#1", stage, "--json"]);
        assert_eq!(card["stage"], stage);
    }
    let unknown = env.cli(&["todo", "move", "#1", "sideways"], None);
    let mismatch = env.cli(&["todo", "move", "#1", "done", "--expect", "review"], None);
    assert!(!unknown.status.success() && !mismatch.status.success());
    assert_eq!(mismatch.status.code(), Some(6));
    assert_ne!(unknown.status.code(), mismatch.status.code());
    let shown = env.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(shown["stage"], "backlog", "a refused move changes nothing");
    let expected = env.json(&[
        "todo", "move", "#1", "next", "--expect", "backlog", "--json",
    ]);
    assert_eq!(expected["stage"], "next");
}

#[test]
fn card_proposal_accept_dismiss() {
    let env = board_env();
    let proposal = add(&env, &["--project", "nebula", "--propose"], "Maybe");
    assert_eq!(proposal["proposed"], true);
    let accepted = env.json(&["todo", "accept", "#1", "--json"]);
    assert_eq!(accepted["proposed"], false);
    assert_eq!(accepted["archived"], false);

    add(&env, &["--project", "nebula", "--propose"], "Nope");
    let dismissed = env.json(&["todo", "dismiss", "#2", "--json"]);
    assert_eq!(dismissed["archived"], true);
    assert_eq!(list(&env, &["--all"]), ["Maybe"]);

    let refused = env.cli(&["todo", "dismiss", "#1"], None);
    assert_eq!(refused.status.code(), Some(6), "{}", stderr(&refused));
    let kept = env.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(kept["archived"], false);
}

#[test]
fn card_id_canonical() {
    let env = board_env();
    let upper = nebula_core::TodoId::generate().0;
    let lower = upper.to_lowercase();
    let card = add(&env, &["--project", "nebula", "--id", &lower], "T");
    assert_eq!(card["id"], upper.as_str());
    let shown = env.json(&["todo", "show", &lower, "--json"]);
    assert_eq!(shown["id"], upper.as_str());
    let again = add(&env, &["--project", "nebula", "--id", &upper], "T");
    assert_eq!(again, card);
    assert_eq!(list(&env, &["--all"]), ["T"]);
}

#[test]
fn card_ref_other_host_refused() {
    let env = board_env();
    add(&env, &["--project", "nebula"], "Here");
    let out = env.cli(&["todo", "move", "1@unrelated", "next"], None);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("no card #1@unrelated"),
        "{}",
        stderr(&out)
    );
    let shown = env.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(shown["stage"], "backlog");
}

#[test]
fn card_block_refuses_cycles() {
    let env = board_env();
    for title in ["One", "Two", "Three"] {
        add(&env, &["--project", "nebula"], title);
    }
    let blocked = env.json(&["todo", "block", "#1", "--by", "#2", "--json"]);
    assert_eq!(blocked["blocked_by"], serde_json::json!(["#2"]));

    let conflict = Some(6);
    assert_eq!(
        env.cli(&["todo", "block", "#1", "--by", "#1"], None)
            .status
            .code(),
        conflict
    );
    assert_eq!(
        env.cli(&["todo", "block", "#2", "--by", "#1"], None)
            .status
            .code(),
        conflict
    );
    env.json(&["todo", "block", "#2", "--by", "#3", "--json"]);
    assert_eq!(
        env.cli(&["todo", "block", "#3", "--by", "#1"], None)
            .status
            .code(),
        conflict
    );
    let three = env.json(&["todo", "show", "#3", "--json"]);
    assert_eq!(three["blocked_by"], serde_json::json!([]));

    let unblocked = env.json(&["todo", "unblock", "#1", "--by", "#2", "--json"]);
    assert_eq!(unblocked["blocked_by"], serde_json::json!([]));
}

#[test]
fn card_note_log_order() {
    let env = board_env();
    add(&env, &["--project", "nebula"], "Logged");
    for text in ["first", "second", "third"] {
        let card = env.json(&["todo", "note", "#1", text, "--json"]);
        assert_eq!(card["ref"], "#1");
        assert_eq!(card["stage"], "backlog");
        assert_eq!(card["title"], "Logged");
    }
    let shown = env.json(&["todo", "show", "#1", "--json"]);
    let log = shown["log"].as_array().unwrap();
    let texts: Vec<&str> = log.iter().map(|n| n["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["first", "second", "third"]);
    for note in log {
        assert_eq!(note["author"]["agent"], Value::Null);
        assert_eq!(note["author"]["host"], nebula_core::host::hostname());
    }
}

#[test]
fn board_since_unchanged() {
    let env = board_env();
    add(&env, &["--project", "nebula"], "A");
    add(&env, &["--project", "nebula"], "B");
    let board = || env.json(&["todo", "board", "--all", "--json"]);
    let since = |version: &str| env.json(&["todo", "board", "--all", "--since", version, "--json"]);

    let first = board();
    let mut version = first["version"].as_str().unwrap().to_string();
    assert_eq!(first["board"]["cards"].as_array().unwrap().len(), 2);
    let (board_id, _) = version.split_once('.').unwrap();
    assert!(ulid::Ulid::from_string(board_id).is_ok(), "{version}");

    let writes: [&[&str]; 6] = [
        &["todo", "note", "#1", "hello"],
        &["todo", "block", "#1", "--by", "#2"],
        &["todo", "unblock", "#1", "--by", "#2"],
        &["todo", "move", "#2", "doing"],
        &["todo", "edit", "#2", "--title", "B2"],
        &["todo", "archive", "#1"],
    ];
    for write in writes {
        let quiet = since(&version);
        assert_eq!(
            quiet,
            serde_json::json!({"version": version, "unchanged": true})
        );
        let out = env.cli(write, None);
        assert!(out.status.success(), "{write:?}: {}", stderr(&out));
        let changed = since(&version);
        let next = changed["version"].as_str().unwrap();
        assert_ne!(next, version, "{write:?} left the version");
        assert!(changed["board"]["cards"].is_array(), "{write:?}: {changed}");
        version = next.to_string();
    }
    assert_eq!(since(&version)["unchanged"], true);
}

#[test]
fn board_read_pages() {
    let env = Env::new();
    env.board_local();
    let env = env.start(&[("NEBULA_BOARD_PAGE_BYTES", "600")]);
    let long = "x".repeat(200);
    for n in 1..=6 {
        add(&env, &["--project", "nebula"], &format!("{n} {long}"));
    }
    let mut pages = vec![env.json(&["todo", "board", "--all", "--json"])];
    while let Some(next) = pages.last().unwrap()["next"].as_str().map(str::to_owned) {
        assert!(pages.len() < 10, "paging never ends");
        pages.push(env.json(&["todo", "board", "--all", "--page", &next, "--json"]));
    }
    assert!(pages.len() > 1, "six 200-byte cards fit one 600-byte page");
    let version = &pages[0]["version"];
    let mut numbers = Vec::new();
    for page in &pages {
        assert_eq!(&page["version"], version);
        for card in page["board"]["cards"].as_array().unwrap() {
            numbers.push(card["number"].as_u64().unwrap());
        }
    }
    assert_eq!(numbers, [1, 2, 3, 4, 5, 6]);

    let continuation = pages[0]["next"].as_str().unwrap().to_string();
    add(&env, &["--project", "nebula"], "late");
    let stale = env.cli(
        &["todo", "board", "--all", "--page", &continuation, "--json"],
        None,
    );
    assert_eq!(stale.status.code(), Some(6), "{}", stderr(&stale));
}

#[test]
fn human_reads_follow_pages() {
    let env = Env::new();
    env.board_local();
    let env = env.start(&[("NEBULA_BOARD_PAGE_BYTES", "600")]);
    let long = "x".repeat(200);
    for n in 1..=5 {
        add(&env, &["--project", "nebula"], &format!("{n} {long}"));
    }
    for verb in ["list", "board"] {
        let out = env.cli(&["todo", verb, "--all"], None);
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
        let printed = String::from_utf8_lossy(&out.stdout);
        for n in 1..=5 {
            assert!(
                printed.lines().any(|l| l.starts_with(&format!("#{n} "))),
                "todo {verb} dropped #{n}: {printed}"
            );
        }
    }
}

#[test]
fn card_list_scope() {
    let env = board_env();
    let a = add(&env, &["--project", "a"], "A1");
    let b = add(&env, &["--project", "b"], "B1");
    add(&env, &["--home"], "H1");
    assert_eq!(a["project"], "a");
    assert_eq!(list(&env, &["--project", "a"]), ["A1"]);
    assert_eq!(
        list(&env, &["--project", "a", "--include-home"]),
        ["A1", "H1"]
    );
    assert_eq!(list(&env, &["--all"]), ["A1", "B1"]);
    assert_eq!(list(&env, &["--all", "--include-home"]), ["A1", "B1", "H1"]);

    let b_ref = b["ref"].as_str().unwrap();
    let b_id = b["id"].as_str().unwrap();
    for reference in [b_ref, b_id] {
        let hidden = env.cli(
            &["todo", "show", reference, "--project", "a", "--json"],
            None,
        );
        assert_eq!(hidden.status.code(), Some(5));
        assert!(hidden.stdout.is_empty());
        let seen = env.json(&["todo", "show", reference, "--project", "b", "--json"]);
        assert_eq!(seen["title"], "B1");
    }
    let board = env.json(&["todo", "board", "--project", "b", "--json"]);
    assert_eq!(titles(&board["board"]["cards"]), ["B1"]);
}

#[test]
fn board_absent_refuses() {
    let env = Env::new().start(&[]);
    let verbs: [&[&str]; 9] = [
        &["todo", "add", "--project", "nebula", "--json", "T"],
        &["todo", "list", "--json"],
        &["todo", "show", "#1", "--json"],
        &["todo", "move", "#1", "next"],
        &["todo", "note", "#1", "text"],
        &["todo", "block", "#1", "--by", "#2"],
        &["todo", "accept", "#1"],
        &["todo", "archive", "#1"],
        &["todo", "board", "--json"],
    ];
    for verb in verbs {
        let out = env.cli(verb, None);
        assert!(!out.status.success(), "{verb:?} succeeded");
        assert!(
            stderr(&out).contains("no board configured"),
            "{verb:?}: {}",
            stderr(&out)
        );
    }
    env.board_local();
    let board = env.json(&["todo", "board", "--all", "--include-home", "--json"]);
    assert_eq!(board["board"]["cards"], serde_json::json!([]));
    assert_eq!(
        board["version"]
            .as_str()
            .unwrap()
            .split_once('.')
            .unwrap()
            .1,
        "0"
    );
}

fn spawn_agent(env: &Env, args: &[&str]) -> Output {
    let mut argv = vec!["agent", "new", "--project", "repo", "--worktree", "main"];
    argv.extend_from_slice(args);
    env.cli(&argv, None)
}

#[test]
fn caller_from_agent_id() {
    let env = board_env();
    env.make_repo();
    let agent = json_of(spawn_agent(&env, &["--name", "worker"]), &["agent", "new"]);
    let agent_id = agent["id"].as_str().unwrap();

    let out = env.cli(&["todo", "add", "--json", "From inside"], Some(agent_id));
    let card = json_of(out, &["todo", "add"]);
    assert_eq!(card["created_by"]["agent"], agent_id);
    assert_eq!(card["created_by"]["name"], "worker");
    assert_eq!(card["project"], "repo");

    let stranger = nebula_core::AgentId::generate().to_string();
    let refused = env.cli(&["todo", "add", "--json", "Ghost"], Some(&stranger));
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains(&format!("NEBULA_AGENT_ID {stranger} names no session")),
        "{}",
        stderr(&refused)
    );
    assert_eq!(list(&env, &["--all", "--include-home"]), ["From inside"]);
}

#[test]
fn agent_new_explicit_card() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Work");
    let spawned = json_of(
        spawn_agent(
            &env,
            &[
                "--card", "#1", "--role", "worker", "--name", "carded", "--json",
            ],
        ),
        &["agent", "new", "--card"],
    );
    assert_eq!(spawned["card"], card["id"]);
    assert_eq!(spawned["role"], "worker");
    let row = env.json(&["agent", "show", "carded", "--project", "repo"]);
    assert_eq!(row["card"], card["id"]);
    assert_eq!(row["role"], "worker");

    let missing = spawn_agent(
        &env,
        &["--card", "#99", "--role", "worker", "--name", "ghost"],
    );
    assert_eq!(missing.status.code(), Some(5), "{}", stderr(&missing));
    assert!(
        stderr(&missing).contains("no card #99"),
        "{}",
        stderr(&missing)
    );
    let agents = env.json(&["agent", "list", "--project", "repo"]);
    let names: Vec<&str> = agents
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["carded"]);
}

fn agent_names(env: &Env) -> Vec<String> {
    env.json(&["agent", "list", "--project", "repo"])
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_string())
        .collect()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", stderr(&out));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn agent_new_refused_card_leaves_checkout() {
    let env = board_env();
    let repo = env.make_repo();
    git(&repo, &["branch", "feature"]);
    let out = env.cli(
        &[
            "agent",
            "new",
            "--project",
            "repo",
            "--worktree",
            "feature",
            "--card",
            "#99",
            "--name",
            "ghost",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert!(stderr(&out).contains("no card #99"), "{}", stderr(&out));
    assert_eq!(git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    assert!(agent_names(&env).is_empty());
}

#[test]
fn agent_new_card_refuses_unknown_caller() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Work");
    let stranger = nebula_core::AgentId::generate().to_string();
    let out = env.cli(
        &[
            "agent",
            "new",
            "--project",
            "repo",
            "--worktree",
            "main",
            "--card",
            "#1",
            "--name",
            "orphan",
        ],
        Some(&stranger),
    );
    assert!(
        !out.status.success(),
        "an unknown caller was persisted as parent"
    );
    assert!(
        stderr(&out).contains(&format!("NEBULA_AGENT_ID {stranger} names no session")),
        "{}",
        stderr(&out)
    );
    assert!(agent_names(&env).is_empty());
}

#[test]
fn card_write_outside_scope_refused() {
    let env = board_env();
    add(&env, &["--project", "other"], "Elsewhere");
    add(&env, &["--project", "nebula"], "Here");
    add(&env, &["--home"], "Home");
    let scope = ["--scope", "nebula"];

    for argv in [
        &["todo", "move", "#1", "next"][..],
        &["todo", "archive", "#1"],
        &["todo", "note", "#1", "hidden"],
        &["todo", "block", "#2", "--by", "#1"],
        &["todo", "edit", "#1", "--title", "Renamed"],
        &["todo", "edit", "#1", "--project", "nebula"],
        &["todo", "archive", "#3"],
    ] {
        let out = env.cli(&[argv, &scope[..]].concat(), None);
        assert_eq!(out.status.code(), Some(5), "{argv:?}: {}", stderr(&out));
    }
    let untouched = env.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(untouched["stage"], "backlog");
    assert_eq!(untouched["archived"], false);
    assert_eq!(untouched["log"], serde_json::json!([]));
    assert_eq!(
        env.json(&["todo", "show", "#2", "--json"])["blocked_by"],
        serde_json::json!([])
    );

    let moved = env.json(&["todo", "move", "#2", "next", "--json", "--scope", "nebula"]);
    assert_eq!(moved["stage"], "next");
    let edited = env.json(&[
        "todo",
        "edit",
        "#2",
        "--project",
        "other",
        "--scope",
        "nebula",
        "--json",
    ]);
    assert_eq!(edited["project"], "other");
    let everywhere = env.json(&["todo", "move", "#1", "done", "--scope", "*", "--json"]);
    assert_eq!(everywhere["stage"], "done");
    let home = env.json(&[
        "todo",
        "archive",
        "#3",
        "--scope",
        "nebula",
        "--include-home",
        "--json",
    ]);
    assert_eq!(home["archived"], true);
}

fn board_at(host: &str) -> String {
    format!(r#"{{"board": "{host}"}}"#)
}

fn proxy_to(board: &Env) -> String {
    format!(
        "NEBULA_RUNTIME_DIR='{}' NEBULA_DATA_DIR='{}' '{}' {{proxy}}",
        board.runtime_dir().display(),
        board.data_dir().display(),
        env!("CARGO_BIN_EXE_nebula")
    )
}

fn pump_to(board: &Env, drop_once: Option<&Path>) -> String {
    let script = board.tmp.path().join("pump.py");
    std::fs::write(
        &script,
        r#"import os, socket, sys, threading
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
marker = sys.argv[2] if len(sys.argv) > 2 else None
drop = marker is not None and not os.path.exists(marker)
def up():
    while True:
        b = os.read(0, 65536)
        if not b:
            break
        s.sendall(b)
    s.shutdown(socket.SHUT_WR)
threading.Thread(target=up, daemon=True).start()
def exact(n):
    buf = b''
    while len(buf) < n:
        c = s.recv(n - len(buf))
        if not c:
            os._exit(0)
        buf += c
    return buf
while True:
    head = exact(4)
    body = exact(int.from_bytes(head, 'little'))
    if drop and b'BoardReply' in body:
        open(marker, 'w').close()
        os._exit(0)
    os.write(1, head + body)
"#,
    )
    .unwrap();
    let mut line = format!(
        "exec python3 '{}' '{}'",
        script.display(),
        board.runtime_dir().join("daemon.sock").display()
    );
    if let Some(marker) = drop_once {
        line.push_str(&format!(" '{}'", marker.display()));
    }
    line
}

fn viewer(relay_cmd: &str) -> Env {
    let env = Env::new();
    env.write_config(&board_at("boardhost"));
    env.start(&[("NEBULA_RELAY_CMD", relay_cmd)])
}

fn stop(env: &mut Env) {
    if let Some(mut daemon) = env.daemon.take() {
        daemon.kill().unwrap();
        daemon.wait().unwrap();
    }
}

#[test]
fn link_add_shows_on_board() {
    let board = board_env();
    let here = viewer(&proxy_to(&board));
    let card = add(&here, &["--project", "nebula"], "From B");
    assert_eq!(card["ref"], "#1@boardhost");
    assert_eq!(card["number"], 1);

    let shown = board.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(shown["id"], card["id"]);
    assert_eq!(shown["title"], "From B");
    assert_eq!(shown["ref"], "#1");

    let moved = here.json(&["todo", "move", "#1@boardhost", "next", "--json"]);
    assert_eq!(moved["stage"], "next");
    assert_eq!(
        board.json(&["todo", "show", "#1", "--json"])["stage"],
        "next"
    );
}

const UNREACHABLE: &str = "nebula: board host boardhost is unreachable; nothing was sent";

#[test]
fn link_down_exit_3() {
    let mut board = board_env();
    let here = viewer(&proxy_to(&board));
    stop(&mut board);

    let out = here.cli(&["todo", "add", "--project", "nebula", "Lost"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains(UNREACHABLE), "{}", stderr(&out));

    let board = board.start(&[]);
    assert_eq!(list(&board, &["--all"]), Vec::<String>::new());
}

#[test]
fn link_host_lost_exit_3() {
    let mut board = board_env();
    let here = viewer(&proxy_to(&board));
    add(&here, &["--project", "nebula"], "Before");
    stop(&mut board);

    let out = here.cli(&["todo", "add", "--project", "nebula", "After"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains(UNREACHABLE), "{}", stderr(&out));

    let board = board.start(&[]);
    assert_eq!(list(&board, &["--all"]), ["Before"]);
}

#[test]
fn link_lost_after_send_exit_4() {
    let board = board_env();
    let marker = board.tmp.path().join("dropped");
    let here = viewer(&pump_to(&board, Some(&marker)));
    let id = nebula_core::TodoId::generate().0;
    let argv = [
        "todo",
        "add",
        "--json",
        "--project",
        "nebula",
        "--id",
        &id,
        "Once",
    ];

    let lost = here.cli(&argv, None);
    assert_eq!(lost.status.code(), Some(4), "{}", stderr(&lost));
    assert!(
        marker.exists(),
        "the reply was dropped after the board wrote"
    );

    let rerun = json_of(here.cli(&argv, None), &argv);
    assert_eq!(rerun["id"], id.as_str());
    assert_eq!(list(&board, &["--all"]), ["Once"]);
}

#[test]
fn link_concurrent_replies() {
    let board = board_env();
    let here = viewer(&proxy_to(&board));
    let spawn = |title: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nebula"));
        cmd.args(["todo", "add", "--json", "--project", "nebula", title])
            .env("NEBULA_RUNTIME_DIR", here.runtime_dir())
            .env("NEBULA_DATA_DIR", here.data_dir())
            .current_dir(here.tmp.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for k in LIVE_SESSION_ENV {
            cmd.env_remove(k);
        }
        cmd.spawn().unwrap()
    };
    let children: Vec<_> = (0..4)
        .map(|i| (format!("Card {i}"), spawn(&format!("Card {i}"))))
        .collect();
    let mut numbers = Vec::new();
    for (title, child) in children {
        let card = json_of(child.wait_with_output().unwrap(), &[title.as_str()]);
        assert_eq!(
            card["title"],
            title.as_str(),
            "each caller gets its own reply"
        );
        numbers.push(card["number"].as_u64().unwrap());
    }
    numbers.sort();
    assert_eq!(numbers, [1, 2, 3, 4]);
}

fn frame<T: serde::Serialize>(msg: &T) -> Vec<u8> {
    let mut frame = Vec::new();
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(nebula_core::codec::write_frame(&mut frame, msg))
        .unwrap();
    frame
}

fn reply_file(env: &Env, name: &str, event: &nebula_core::ServerEvent) -> PathBuf {
    let path = env.tmp.path().join(name);
    std::fs::write(&path, frame(event)).unwrap();
    path
}

#[test]
fn link_input_closed_before_write_exit_3() {
    let env = Env::new();
    let hello = frame(&nebula_core::ClientRequest::Hello {
        protocol_version: nebula_core::PROTOCOL_VERSION,
    });
    let ok = reply_file(
        &env,
        "hello_ok.bin",
        &nebula_core::ServerEvent::HelloOk {
            protocol_version: nebula_core::PROTOCOL_VERSION,
            daemon_pid: 1,
        },
    );
    let here = viewer(&format!(
        "dd bs=1 count={} of=/dev/null 2>/dev/null; exec 0<&-; cat '{}'; exec sleep 30",
        hello.len(),
        ok.display()
    ));

    let out = here.cli(&["todo", "add", "--project", "nebula", "Unsent"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains(UNREACHABLE), "{}", stderr(&out));
}

#[test]
fn link_refuses_protocol_31() {
    let env = Env::new();
    let reply = reply_file(
        &env,
        "incompatible.bin",
        &nebula_core::ServerEvent::Incompatible {
            daemon_protocol_version: 31,
        },
    );
    let here = viewer(&format!("cat '{}'; cat > /dev/null", reply.display()));

    let out = here.cli(&["todo", "add", "--project", "nebula", "Old peer"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("protocol 31"), "{}", stderr(&out));
}

#[test]
fn link_refuses_hello_ok_31() {
    let env = Env::new();
    let reply = reply_file(
        &env,
        "hello_ok_31.bin",
        &nebula_core::ServerEvent::HelloOk {
            protocol_version: 31,
            daemon_pid: 1,
        },
    );
    let here = viewer(&format!("cat '{}'; cat > /dev/null", reply.display()));

    let out = here.cli(&["todo", "add", "--project", "nebula", "Old peer"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("protocol 31"), "{}", stderr(&out));
}

#[test]
fn link_refusal_cleared_by_later_attempt() {
    let env = Env::new();
    let reply = reply_file(
        &env,
        "incompatible.bin",
        &nebula_core::ServerEvent::Incompatible {
            daemon_protocol_version: 31,
        },
    );
    let marker = env.tmp.path().join("answered");
    let here = viewer(&format!(
        "[ -e '{m}' ] && exit 0; touch '{m}'; cat '{}'; cat > /dev/null",
        reply.display(),
        m = marker.display()
    ));
    let argv = ["todo", "add", "--project", "nebula", "Peer"];

    let refused = here.cli(&argv, None);
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("protocol 31"),
        "{}",
        stderr(&refused)
    );

    let gone = here.cli(&argv, None);
    assert_eq!(gone.status.code(), Some(3), "{}", stderr(&gone));
    assert!(stderr(&gone).contains(UNREACHABLE), "{}", stderr(&gone));
}

fn rerun_id(out: &Output) -> String {
    let text = stderr(out);
    let id = text
        .split("--id ")
        .nth(1)
        .unwrap_or_else(|| panic!("exit 4 names no --id: {text}"));
    id.chars().take_while(char::is_ascii_alphanumeric).collect()
}

#[test]
fn link_lost_add_without_id_names_it() {
    let board = board_env();
    let marker = board.tmp.path().join("dropped");
    let here = viewer(&pump_to(&board, Some(&marker)));

    let lost = here.cli(&["todo", "add", "--project", "nebula", "Once"], None);
    assert_eq!(lost.status.code(), Some(4), "{}", stderr(&lost));
    let id = rerun_id(&lost);

    let argv = [
        "todo",
        "add",
        "--json",
        "--project",
        "nebula",
        "--id",
        &id,
        "Once",
    ];
    let rerun = json_of(here.cli(&argv, None), &argv);
    assert_eq!(rerun["id"], id.as_str());
    assert_eq!(list(&board, &["--all"]), ["Once"]);
}

#[test]
fn link_lost_note_without_id_names_it() {
    let board = board_env();
    add(&board, &["--project", "nebula"], "Card");
    let marker = board.tmp.path().join("dropped");
    let here = viewer(&pump_to(&board, Some(&marker)));

    let lost = here.cli(&["todo", "note", "#1@boardhost", "once"], None);
    assert_eq!(lost.status.code(), Some(4), "{}", stderr(&lost));
    let id = rerun_id(&lost);

    let argv = [
        "todo",
        "note",
        "#1@boardhost",
        "once",
        "--id",
        &id,
        "--json",
    ];
    json_of(here.cli(&argv, None), &argv);
    let log = board.json(&["todo", "show", "#1", "--json"])["log"].clone();
    assert_eq!(log.as_array().map(Vec::len), Some(1), "{log}");
    assert_eq!(log[0]["id"], id.as_str());
}
