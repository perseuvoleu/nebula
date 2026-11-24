//! Cards on the board host: the `nebula todo` verbs against a daemon whose
//! config says `board = "local"`, through the real CLI and its --json.

use nebula_core::board::Refusal;
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
    assert_eq!(mismatch.status.code(), Some(Refusal::Conflict.exit_code()));
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
fn card_block_refuses_cycles() {
    let env = board_env();
    for title in ["One", "Two", "Three"] {
        add(&env, &["--project", "nebula"], title);
    }
    let blocked = env.json(&["todo", "block", "#1", "--by", "#2", "--json"]);
    assert_eq!(blocked["blocked_by"], serde_json::json!(["#2"]));

    let conflict = Some(Refusal::Conflict.exit_code());
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
        let note = env.json(&["todo", "note", "#1", text, "--json"]);
        assert_eq!(note["text"], text);
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
    assert_eq!(
        stale.status.code(),
        Some(Refusal::Conflict.exit_code()),
        "{}",
        stderr(&stale)
    );
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
        assert_eq!(hidden.status.code(), Some(Refusal::NotFound.exit_code()));
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
    assert!(!missing.status.success());
    let agents = env.json(&["agent", "list", "--project", "repo"]);
    let names: Vec<&str> = agents
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["carded"]);
}
