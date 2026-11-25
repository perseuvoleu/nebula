//! Cards on the board host: the `nebula todo` verbs against a daemon whose
//! config says `board = "local"`, through the real CLI and its --json.

use nebula_core::board::{
    BoardOp, BoardResult, CardRef, Dispatch, DispatchState, HostCheckout, HostReport, Role,
    SessionRow,
};
use nebula_core::{AgentId, AgentKind, AgentStatus, DispatchId, TodoId};
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
    host: Option<String>,
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
            host: None,
        }
    }

    fn on_host(mut self, host: &str) -> Self {
        self.host = Some(host.into());
        self
    }

    fn host_name(&self) -> String {
        self.host
            .clone()
            .unwrap_or_else(nebula_core::host::hostname)
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
        cmd.env_remove("NEBULA_RAG_ENV")
            .env("NEBULA_RAG_LOCAL", "1")
            .env("CF_ACCOUNT_ID", "")
            .env("CF_AI_TOKEN", "");
        if let Some(host) = &self.host {
            cmd.env("NEBULA_HOSTNAME", host);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        self.daemon = Some(cmd.spawn().unwrap());
        wait_for(&self.runtime_dir().join("daemon.sock"));
        self
    }

    fn cli(&self, args: &[&str], agent: Option<&str>) -> Output {
        self.cli_env(args, agent, &[])
    }

    fn cli_env(&self, args: &[&str], agent: Option<&str>, envs: &[(&str, &str)]) -> Output {
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
        if let Some(host) = &self.host {
            cmd.env("NEBULA_HOSTNAME", host);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn spawn_cli(&self, args: &[String]) -> Child {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nebula"));
        cmd.args(args)
            .env("NEBULA_RUNTIME_DIR", self.runtime_dir())
            .env("NEBULA_DATA_DIR", self.data_dir())
            .current_dir(self.tmp.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for k in LIVE_SESSION_ENV {
            cmd.env_remove(k);
        }
        cmd.spawn().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        json_of(self.cli(args, None), args)
    }

    fn make_repo(&self) -> PathBuf {
        self.make_repo_named("repo")
    }

    fn make_repo_named(&self, name: &str) -> PathBuf {
        let repo = self.tmp.path().join(name);
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
    reason = "the one poll loop: every wait in this file goes through it"
)]
fn poll_until<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for(sock: &Path) {
    poll_until(Duration::from_secs(5), || {
        std::os::unix::net::UnixStream::connect(sock).ok()
    })
    .expect("daemon socket never appeared");
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
fn board_marks_cleared_blockers_and_card_ages() {
    let env = board_env();
    for title in ["Blocked", "Finished", "Shelved", "Open"] {
        add(&env, &["--project", "nebula"], title);
    }
    for by in ["#2", "#3", "#4"] {
        env.json(&["todo", "block", "#1", "--by", by, "--json"]);
    }
    let start = now_ms();
    env.json(&["todo", "move", "#2", "done", "--json"]);
    env.json(&["todo", "archive", "#3", "--json"]);
    let board = env.json(&["todo", "board", "--all", "--json"]);
    let blocked = board["board"]["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["ref"] == "#1")
        .unwrap()
        .clone();
    assert_eq!(blocked["blocked_by"], serde_json::json!(["#2", "#3", "#4"]));
    assert_eq!(blocked["cleared_blockers"], serde_json::json!(["#2", "#3"]));
    let created = blocked["created_at"].as_i64().unwrap();
    assert!(created > 0 && created <= start, "{blocked}");
    let moved = env.json(&["todo", "show", "#2", "--json"]);
    assert!(moved["updated_at"].as_i64().unwrap() >= start, "{moved}");
    assert!(moved["created_at"].as_i64().unwrap() <= start, "{moved}");
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
fn a_local_board_with_no_other_host_keeps_its_version_after_boot() {
    let env = Env::new();
    env.board_local();
    let env = env.start(&[("NEBULA_BOARD_STALE_MS", "100")]);
    let version = || env.json(&["todo", "board", "--all", "--json"])["version"].clone();
    let before = version();
    let bumped = poll_until(Duration::from_millis(800), || {
        (version() != before).then_some(())
    });
    assert_eq!(bumped, None, "the boot timer bumped an unchanged board");
}

#[test]
fn edit_to_equal_values_keeps_the_version() {
    let env = board_env();
    add(&env, &["--project", "nebula"], "Same");
    add(&env, &[], "Home");
    let version = || env.json(&["todo", "board", "--all", "--json"])["version"].clone();
    let before = version();
    for edit in [
        &["todo", "edit", "#1", "--title", "Same"][..],
        &["todo", "edit", "#1", "--project", "nebula"],
        &["todo", "edit", "#2", "--home"],
    ] {
        let out = env.cli(edit, None);
        assert!(out.status.success(), "{edit:?}: {}", stderr(&out));
        assert_eq!(version(), before, "{edit:?} bumped the version");
    }
}

#[test]
fn board_read_pages() {
    let env = Env::new();
    env.board_local();
    let budget = 1600;
    let env = env.start(&[("NEBULA_BOARD_PAGE_BYTES", &budget.to_string())]);
    let long = "x".repeat(1200);
    for title in ["1", "2", &format!("3 {long}"), "4", "5", "6"] {
        add(&env, &["--project", "nebula"], title);
    }
    let mut pages = vec![env.json(&["todo", "board", "--all", "--json"])];
    while let Some(next) = pages.last().unwrap()["next"].as_str().map(str::to_owned) {
        assert!(pages.len() < 10, "paging never ends");
        pages.push(env.json(&["todo", "board", "--all", "--page", &next, "--json"]));
    }
    let version = &pages[0]["version"];
    let mut numbers = Vec::new();
    let mut sizes = Vec::new();
    for page in &pages {
        assert_eq!(&page["version"], version);
        let cards = page["board"]["cards"].as_array().unwrap();
        let bytes: usize = cards
            .iter()
            .map(|c| serde_json::to_vec(c).unwrap().len())
            .sum();
        assert!(
            cards.len() == 1 || bytes <= budget,
            "{bytes} bytes over a {budget}-byte page"
        );
        sizes.push(cards.len());
        numbers.extend(cards.iter().map(|c| c["number"].as_u64().unwrap()));
    }
    assert_eq!(numbers, [1, 2, 3, 4, 5, 6]);
    assert!(
        pages.len() > 1,
        "a 1200-byte card and five short ones fit one page: {sizes:?}"
    );
    assert!(
        sizes.iter().any(|n| *n > 1),
        "short cards never share a page: {sizes:?}"
    );

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

fn eventually<T>(what: &str, probe: impl FnMut() -> Option<T>) -> T {
    poll_until(Duration::from_secs(20), probe).unwrap_or_else(|| panic!("{what} never happened"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn board_op(env: &Env, op: BoardOp) -> Result<BoardResult, String> {
    use nebula_core::codec::{read_frame, write_frame};
    use nebula_core::{ClientRequest, ServerEvent, PROTOCOL_VERSION};
    let sock = env.runtime_dir().join("daemon.sock");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let mut stream = tokio::net::UnixStream::connect(sock).await.unwrap();
            let hello = ClientRequest::Hello {
                protocol_version: PROTOCOL_VERSION,
            };
            write_frame(&mut stream, &hello).await.unwrap();
            read_frame::<ServerEvent, _>(&mut stream).await.unwrap();
            write_frame(&mut stream, &ClientRequest::Board { req_id: 7, op })
                .await
                .unwrap();
            loop {
                match read_frame::<ServerEvent, _>(&mut stream).await.unwrap() {
                    Some(ServerEvent::BoardReply { req_id: 7, result }) => return Ok(result),
                    Some(ServerEvent::Error {
                        req_id: Some(7),
                        message,
                    }) => return Err(message),
                    Some(_) => continue,
                    None => return Err("the daemon hung up".into()),
                }
            }
        })
}

const FAR_HOST: &str = "elsewhere";

fn far_row(card: &Value, agent: &str, status: AgentStatus, changed_at: i64) -> SessionRow {
    SessionRow {
        agent_id: AgentId(agent.into()),
        host: FAR_HOST.into(),
        card_id: TodoId(card["id"].as_str().unwrap().into()),
        role: Some(Role::Worker),
        kind: AgentKind::Codex,
        name: agent.into(),
        parent: None,
        status,
        status_changed_at: changed_at,
        writer: false,
        alive: true,
        archived: false,
        branch: Some("main".into()),
        dispatch_id: None,
        created_at: changed_at,
        project: Some("nebula".into()),
        worktree: false,
    }
}

fn far_report(env: &Env, sessions: Vec<SessionRow>) {
    far_report_holding(env, Vec::new(), sessions);
}

fn far_report_holding(env: &Env, checkouts: Vec<HostCheckout>, sessions: Vec<SessionRow>) {
    let report = HostReport {
        host: FAR_HOST.into(),
        writers: 0,
        writer_slots: None,
        checkouts,
        sessions,
    };
    board_op(env, BoardOp::Report(report)).expect("the board takes a host report");
}

fn far_host(hosts: &[Value]) -> Value {
    hosts
        .iter()
        .find(|h| h["host"] == FAR_HOST)
        .cloned()
        .unwrap()
}

fn stage_of(env: &Env, card: &str) -> String {
    env.json(&["todo", "show", card, "--json"])["stage"]
        .as_str()
        .unwrap()
        .to_string()
}

fn hosts_of(env: &Env, scope: &[&str]) -> Vec<Value> {
    let mut argv = vec!["todo", "board", "--json"];
    argv.extend_from_slice(scope);
    env.json(&argv)["board"]["hosts"]
        .as_array()
        .unwrap()
        .clone()
}

fn this_host(hosts: &[Value]) -> Option<Value> {
    hosts
        .iter()
        .find(|h| h["host"] == nebula_core::host::hostname())
        .cloned()
}

fn chain(env: &Env, card: &str) -> Vec<Value> {
    env.json(&["todo", "show", card, "--json"])["sessions"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn child_of_an_archived_card_joins_none() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Gone");
    let parent = json_of(
        spawn_agent(&env, &["--card", "#1", "--name", "parent"]),
        &["agent", "new", "parent"],
    );
    env.json(&["todo", "archive", "#1", "--json"]);
    let argv = [
        "agent",
        "new",
        "--project",
        "repo",
        "--worktree",
        "main",
        "--name",
        "late",
    ];
    let child = json_of(env.cli(&argv, parent["id"].as_str()), &argv);
    assert_eq!(child["card"], Value::Null);
}

#[test]
fn child_inherits_card_not_role() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Chain");
    let parent = json_of(
        spawn_agent(
            &env,
            &["--card", "#1", "--role", "worker", "--name", "parent"],
        ),
        &["agent", "new", "parent"],
    );
    let parent_id = parent["id"].as_str().unwrap();
    let child = |extra: &[&str]| {
        let mut argv = vec!["agent", "new", "--project", "repo", "--worktree", "main"];
        argv.extend_from_slice(extra);
        json_of(env.cli(&argv, Some(parent_id)), &argv)
    };
    let reviewer = child(&["--role", "review", "--name", "reviewer"]);
    assert_eq!(reviewer["card"], card["id"]);
    assert_eq!(reviewer["role"], "review");
    let roleless = child(&["--name", "roleless"]);
    assert_eq!(roleless["card"], card["id"]);
    assert_eq!(roleless["role"], Value::Null, "a child never inherits role");
    let loose = child(&["--no-card", "--name", "loose"]);
    assert_eq!(loose["card"], Value::Null);
    let row = env.json(&["agent", "show", "reviewer", "--project", "repo"]);
    assert_eq!(row["card"], card["id"]);

    let sessions = eventually("the chain on the card", || {
        let sessions = chain(&env, "#1");
        (sessions.len() == 3).then_some(sessions)
    });
    let listed: Vec<(&str, &Value)> = sessions
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), &s["role"]))
        .collect();
    assert_eq!(
        listed,
        [
            ("parent", &serde_json::json!("worker")),
            ("reviewer", &serde_json::json!("review")),
            ("roleless", &Value::Null),
        ]
    );
}

#[test]
fn auto_stage_worker_doing_review() {
    let replay = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/replay");
    let agent = format!(
        "{} {}",
        replay.join("agent").display(),
        replay.join("turn.replay").display()
    );
    let env = Env::new();
    env.board_local();
    let env = env.start(&[
        ("NEBULA_AGENT_CMD", agent.as_str()),
        ("NEBULA_STATUS_TICK_MS", "100"),
    ]);
    env.make_repo();
    add(&env, &["--project", "repo"], "Work");
    json_of(
        spawn_agent(
            &env,
            &["--card", "#1", "--role", "worker", "--name", "worker"],
        ),
        &["agent", "new", "worker"],
    );
    eventually("the card in doing", || {
        (stage_of(&env, "#1") == "doing").then_some(())
    });
    let sent = env.cli(
        &["agent", "send", "worker", "go", "--project", "repo"],
        None,
    );
    assert!(sent.status.success(), "{}", stderr(&sent));
    eventually("the card in review", || {
        (stage_of(&env, "#1") == "review").then_some(())
    });
    let sessions = chain(&env, "#1");
    assert_eq!(sessions[0]["status"], "finished");
}

#[test]
fn auto_stage_ignores_duplicate_report() {
    let env = board_env();
    let card = add(&env, &["--project", "nebula"], "Work");
    let report = vec![far_row(&card, "w1", AgentStatus::Running, 10)];
    far_report(&env, report.clone());
    assert_eq!(stage_of(&env, "#1"), "doing");
    env.json(&["todo", "move", "#1", "next", "--json"]);
    far_report(&env, report);
    assert_eq!(stage_of(&env, "#1"), "next");
}

#[test]
fn card_show_chain_and_checkout() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Chain");
    let worker = json_of(
        spawn_agent(
            &env,
            &["--card", "#1", "--role", "worker", "--name", "worker"],
        ),
        &["agent", "new", "worker"],
    );
    let argv = [
        "agent",
        "new",
        "--project",
        "repo",
        "--worktree",
        "main",
        "--role",
        "review",
        "--name",
        "reviewer",
    ];
    json_of(env.cli(&argv, worker["id"].as_str()), &argv);
    let archived = env.cli(&["agent", "archive", "reviewer", "--project", "repo"], None);
    assert!(archived.status.success(), "{}", stderr(&archived));

    let shown = eventually("the archived reviewer reported", || {
        let shown = env.json(&["todo", "show", "#1", "--json"]);
        let sessions = shown["sessions"].as_array().unwrap();
        (sessions.len() == 2 && sessions[1]["alive"] == false).then_some(shown)
    });
    let sessions = shown["sessions"].as_array().unwrap();
    let host = nebula_core::host::hostname();
    assert_eq!(sessions[0]["name"], "worker");
    assert_eq!(sessions[0]["id"], worker["id"]);
    assert_eq!(sessions[0]["role"], "worker");
    assert_eq!(sessions[0]["alive"], true);
    assert_eq!(sessions[1]["name"], "reviewer");
    assert_eq!(sessions[1]["role"], "review");
    for session in sessions {
        assert_eq!(session["host"], host.as_str());
        assert_eq!(session["kind"], "claude");
        assert_eq!(session["writer"], false);
        assert_eq!(session["stale"], false);
        assert!(session["status"].is_string(), "{session}");
        assert!(session["status_changed_at"].is_i64(), "{session}");
    }
    assert_eq!(shown["checkout"]["host"], host.as_str());
    assert_eq!(shown["checkout"]["branch"], "main");
    assert_eq!(shown["checkout"]["worktree"], false);
}

#[test]
fn board_hosts_writers() {
    let env = board_env();
    let repo = env.make_repo();
    add(&env, &["--project", "repo"], "Write");
    json_of(
        spawn_agent(
            &env,
            &[
                "--card", "#1", "--role", "worker", "--writer", "--name", "writer",
            ],
        ),
        &["agent", "new", "--writer"],
    );
    let host = eventually("one live writer reported", || {
        this_host(&hosts_of(&env, &["--all"])).filter(|h| h["writers"] == 1)
    });
    assert_eq!(host["writer_slots"], Value::Null);
    let checkouts = host["checkouts"].as_array().unwrap();
    assert_eq!(checkouts.len(), 1, "{host}");
    assert_eq!(checkouts[0]["project"], "repo");
    assert_eq!(checkouts[0]["branch"], "main");
    assert_eq!(checkouts[0]["worktree"], false);
    assert_eq!(
        Path::new(checkouts[0]["path"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        repo.canonicalize().unwrap()
    );
    let other = this_host(&hosts_of(&env, &["--project", "other"])).unwrap();
    assert_eq!(other["checkouts"], serde_json::json!([]), "scope applies");
}

#[test]
fn report_unchanged_keeps_version() {
    let env = board_env();
    eventually("this host's start report", || {
        this_host(&hosts_of(&env, &["--all"]))
    });
    let card = add(&env, &["--project", "nebula"], "Work");
    let rows = vec![far_row(&card, "w1", AgentStatus::Running, 10)];
    far_report(&env, rows.clone());
    let version = env.json(&["todo", "board", "--all", "--json"])["version"]
        .as_str()
        .unwrap()
        .to_string();
    far_report(&env, rows);
    let again = env.json(&["todo", "board", "--all", "--since", &version, "--json"]);
    assert_eq!(again["unchanged"], true, "{again}");
    far_report(
        &env,
        vec![far_row(&card, "w1", AgentStatus::NeedsFeedback, 20)],
    );
    let changed = env.json(&["todo", "board", "--all", "--since", &version, "--json"]);
    assert_ne!(changed["version"], version.as_str());
}

#[test]
fn away_digest_sessions() {
    let env = board_env();
    let work = add(&env, &["--project", "nebula"], "Work");
    add(&env, &["--project", "nebula"], "Old");
    let ask = add(&env, &["--project", "nebula"], "Ask");
    env.json(&["todo", "move", "#2", "review", "--json"]);
    let before = now_ms() - 60_000;
    far_report(
        &env,
        vec![far_row(&ask, "n1", AgentStatus::NeedsFeedback, before)],
    );
    let since = now_ms();
    let after = since + 1;
    far_report(
        &env,
        vec![far_row(&work, "w1", AgentStatus::Running, after)],
    );
    far_report(
        &env,
        vec![
            far_row(&work, "w1", AgentStatus::Finished, after + 1),
            far_row(&ask, "n2", AgentStatus::NeedsFeedback, after + 1),
        ],
    );
    let away_since = since.to_string();
    let board = env.json(&[
        "todo",
        "board",
        "--all",
        "--away-since",
        &away_since,
        "--json",
    ]);
    let away = &board["board"]["away"];
    assert_eq!(away["finished"], serde_json::json!([work["id"]]), "{away}");
    assert_eq!(
        away["needs_you"],
        serde_json::json!([{"host": FAR_HOST, "id": "n2", "name": "n2", "card": ask["id"], "status_changed_at": after + 1}]),
        "{away}"
    );
}

#[test]
fn auto_stage_sees_the_whole_report() {
    let env = board_env();
    let card = add(&env, &["--project", "nebula"], "Work");
    far_report(&env, vec![far_row(&card, "w1", AgentStatus::Running, 10)]);
    assert_eq!(stage_of(&env, "#1"), "doing");
    far_report(
        &env,
        vec![
            far_row(&card, "w1", AgentStatus::Finished, 20),
            far_row(&card, "w2", AgentStatus::Running, 20),
        ],
    );
    assert_eq!(stage_of(&env, "#1"), "doing", "w2 still works on the card");
}

#[test]
fn card_show_lists_sessions_in_created_order() {
    let env = board_env();
    let card = add(&env, &["--project", "nebula"], "Work");
    let newer = SessionRow {
        created_at: 20,
        ..far_row(&card, "newer", AgentStatus::Running, 5)
    };
    let older = SessionRow {
        created_at: 10,
        ..far_row(&card, "older", AgentStatus::Running, 5)
    };
    far_report(&env, vec![newer.clone()]);
    far_report(&env, vec![newer, older]);
    let names: Vec<Value> = chain(&env, "#1")
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(names, ["older", "newer"]);
}

#[test]
fn card_checkout_matches_project_and_branch() {
    let env = board_env();
    let card = add(&env, &["--home"], "Home");
    let held = |project: &str| HostCheckout {
        project: project.into(),
        branch: "main".into(),
        path: format!("/work/{project}"),
        worktree: false,
    };
    let worker = SessionRow {
        project: Some("bbb".into()),
        ..far_row(&card, "w1", AgentStatus::Running, 10)
    };
    far_report_holding(&env, vec![held("aaa"), held("bbb")], vec![worker]);
    let shown = env.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(shown["checkout"]["path"], "/work/bbb", "{shown}");
}

#[test]
fn host_needs_you_follows_the_read_scope() {
    let env = board_env();
    let card = add(&env, &["--project", "nebula"], "Ask");
    far_report(
        &env,
        vec![far_row(&card, "n1", AgentStatus::NeedsFeedback, 10)],
    );
    assert_eq!(far_host(&hosts_of(&env, &["--all"]))["needs_you"], 1);
    assert_eq!(
        far_host(&hosts_of(&env, &["--project", "other"]))["needs_you"],
        0,
        "the waiting session's card is out of scope"
    );
}

#[test]
fn host_needs_you_skips_a_card_not_on_this_board() {
    let env = board_env();
    let elsewhere = serde_json::json!({"id": nebula_core::TodoId::generate().0});
    far_report(
        &env,
        vec![far_row(&elsewhere, "n1", AgentStatus::NeedsFeedback, 10)],
    );
    assert_eq!(
        far_host(&hosts_of(&env, &["--include-home"]))["needs_you"],
        0,
        "a session waiting on another board's card counted as home"
    );
}

#[test]
fn board_needs_you_lists_every_waiting_session_in_scope() {
    let env = board_env();
    let ask = add(&env, &["--project", "nebula"], "Ask");
    let work = add(&env, &["--project", "nebula"], "Work");
    let before = now_ms() - 60_000;
    far_report(
        &env,
        vec![
            far_row(&ask, "n1", AgentStatus::NeedsFeedback, before),
            far_row(&work, "w1", AgentStatus::Running, before),
        ],
    );
    let away_since = (before + 30_000).to_string();
    let read = |scope: &str| {
        env.json(&[
            "todo",
            "board",
            scope,
            "--away-since",
            &away_since,
            "--json",
        ])["board"]
            .clone()
    };
    let all = read("--all");
    assert_eq!(
        all["needs_you"],
        serde_json::json!([{"host": FAR_HOST, "id": "n1", "name": "n1", "card": ask["id"], "status_changed_at": before}]),
        "{all}"
    );
    assert_eq!(all["away"]["needs_you"], serde_json::json!([]), "{all}");
    assert_eq!(read("--project=other")["needs_you"], serde_json::json!([]));
}

#[test]
fn omitted_session_stops_needing_the_user() {
    let env = board_env();
    let card = add(&env, &["--project", "nebula"], "Ask");
    let since = now_ms();
    far_report(
        &env,
        vec![far_row(&card, "n1", AgentStatus::NeedsFeedback, since + 1)],
    );
    let away_since = since.to_string();
    let away = |env: &Env| {
        env.json(&[
            "todo",
            "board",
            "--all",
            "--away-since",
            &away_since,
            "--json",
        ])["board"]["away"]["needs_you"]
            .clone()
    };
    assert_eq!(
        away(&env),
        serde_json::json!([{"host": FAR_HOST, "id": "n1", "name": "n1", "card": card["id"], "status_changed_at": since + 1}])
    );
    far_report(&env, Vec::new());
    assert_eq!(away(&env), serde_json::json!([]), "n1 is no longer live");
    assert_eq!(far_host(&hosts_of(&env, &["--all"]))["needs_you"], 0);
}

#[test]
fn agent_new_refuses_unknown_caller_on_every_path() {
    let env = board_env();
    env.make_repo();
    let stranger = nebula_core::AgentId::generate().to_string();
    for extra in [&[][..], &["--no-card"]] {
        let mut argv = vec![
            "agent",
            "new",
            "--project",
            "repo",
            "--worktree",
            "main",
            "--name",
            "orphan",
        ];
        argv.extend_from_slice(extra);
        let out = env.cli(&argv, Some(&stranger));
        assert!(
            !out.status.success(),
            "{argv:?} spawned for an unknown caller"
        );
        assert!(
            stderr(&out).contains(&format!("NEBULA_AGENT_ID {stranger} names no session")),
            "{}",
            stderr(&out)
        );
    }
    assert!(agent_names(&env).is_empty());
}

#[test]
fn human_show_and_away_name_sessions() {
    let env = board_env();
    let work = add(&env, &["--project", "nebula"], "Work");
    let ask = add(&env, &["--project", "nebula"], "Ask");
    let since = now_ms();
    let writer = |status, at| SessionRow {
        writer: true,
        ..far_row(&work, "w1", status, at)
    };
    far_report(&env, vec![writer(AgentStatus::Running, since + 1)]);
    far_report(
        &env,
        vec![
            writer(AgentStatus::Finished, since + 2),
            far_row(&ask, "n1", AgentStatus::NeedsFeedback, since + 2),
        ],
    );
    let human = |argv: &[&str]| {
        let out = env.cli(argv, None);
        assert!(out.status.success(), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let show = human(&["todo", "show", "#1"]);
    assert!(
        show.contains("worker w1 on elsewhere: codex finished just now, writer, live"),
        "{show}"
    );
    let away_since = since.to_string();
    let board = human(&["todo", "board", "--all", "--away-since", &away_since]);
    assert!(board.contains("finished: #1\n"), "{board}");
    assert!(board.contains("needs you: n1 on elsewhere\n"), "{board}");
}

impl Env {
    fn restart(mut self) -> Self {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        let _ = std::fs::remove_file(self.runtime_dir().join("daemon.sock"));
        self.start(&[])
    }

    fn stored(&self, id: &str) -> PathBuf {
        self.data_dir().join("artifacts").join(id)
    }
}

fn tree(root: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    for (path, bytes) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    root.canonicalize().unwrap()
}

fn e2e_run(root: &Path, screenshot: &str) -> PathBuf {
    let result = serde_json::json!({
        "title": "board.spec",
        "status": "passed",
        "started_at": 1764000000000_i64,
        "duration_ms": 1200,
        "steps": [
            {"name": "open", "status": "passed", "duration_ms": 700, "screenshot": screenshot},
            {"name": "drag", "status": "passed", "duration_ms": 500}
        ]
    });
    tree(
        root,
        &[
            ("result.json", result.to_string().as_bytes()),
            ("shots/open.png", b"png"),
        ],
    )
}

fn carded_agent(env: &Env, worktree: &str) -> (Value, String) {
    let card = add(env, &["--project", "repo"], "Publish");
    let out = env.cli(
        &[
            "agent",
            "new",
            "--project",
            "repo",
            "--worktree",
            worktree,
            "--card",
            "#1",
            "--role",
            "worker",
            "--name",
            "publisher",
            "--json",
        ],
        None,
    );
    let agent = json_of(out, &["agent", "new", "--card"]);
    (card, agent["id"].as_str().unwrap().to_string())
}

fn publish(env: &Env, agent: Option<&str>, args: &[&str]) -> Value {
    let mut argv = vec!["artifact", "add", "--json"];
    argv.extend_from_slice(args);
    json_of(env.cli(&argv, agent), &argv)
}

fn artifact_ids(env: &Env) -> Vec<String> {
    env.json(&["artifact", "list", "--all", "--include-home", "--json"])["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn artifact_add_links_caller() {
    let env = board_env();
    let repo = env.make_repo();
    let (card, agent) = carded_agent(&env, "main");
    let recap = tree(
        &env.tmp.path().join("recap"),
        &[
            ("index.html", b"<h1>recap</h1>"),
            ("assets/app.css", b"h1{}"),
        ],
    );
    let run = e2e_run(&env.tmp.path().join("run"), "shots/open.png");
    let host = nebula_core::host::hostname();

    for (dir, kind, entry) in [
        (&recap, "recap", "index.html"),
        (&run, "e2e-run", "result.json"),
    ] {
        let added = publish(&env, Some(&agent), &[dir.to_str().unwrap(), "--kind", kind]);
        assert_eq!(added["kind"], kind);
        assert_eq!(added["card"], card["id"], "{kind}");
        assert_eq!(added["project"], "repo");
        assert_eq!(added["session"]["id"], agent.as_str());
        assert_eq!(added["session"]["name"], "publisher");
        assert_eq!(added["checkout"]["branch"], "main");
        assert_eq!(
            Path::new(added["checkout"]["path"].as_str().unwrap()),
            repo.canonicalize().unwrap()
        );
        let id = added["id"].as_str().unwrap();
        let shown = env.json(&["artifact", "show", id, "--json"]);
        assert_eq!(
            shown["content"],
            serde_json::json!({"host": host, "entry": entry})
        );
        assert_eq!(shown, added);
        assert!(
            env.stored(id).join(entry).is_file(),
            "{kind} entry not stored"
        );
    }
    let on_card = env.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(
        on_card["artifacts"].as_array().unwrap().len(),
        2,
        "{on_card}"
    );
}

#[test]
fn artifact_survives_worktree_archive_restart() {
    let env = board_env();
    env.make_repo();
    let made = env.cli(&["worktree", "new", "feature", "--project", "repo"], None);
    assert!(made.status.success(), "{}", stderr(&made));
    let (_, agent) = carded_agent(&env, "feature");
    let recap = tree(
        &env.tmp.path().join("recap"),
        &[("index.html", b"<h1>kept</h1>")],
    );
    let added = publish(
        &env,
        Some(&agent),
        &[recap.to_str().unwrap(), "--kind", "recap"],
    );
    let id = added["id"].as_str().unwrap().to_string();
    std::fs::remove_dir_all(&recap).unwrap();

    for step in [
        &["agent", "archive", "publisher", "--project", "repo"][..],
        &[
            "worktree",
            "delete",
            "feature",
            "--force",
            "--project",
            "repo",
        ],
    ] {
        let out = env.cli(step, None);
        assert!(out.status.success(), "{step:?}: {}", stderr(&out));
    }
    let env = env.restart();
    let shown = env.json(&["artifact", "show", &id, "--json"]);
    assert_eq!(shown, added);
    let bytes = std::fs::read(env.stored(&id).join("index.html")).unwrap();
    assert_eq!(bytes, b"<h1>kept</h1>");
}

#[test]
fn artifact_add_refusals() {
    let env = Env::new();
    env.board_local();
    let env = env.start(&[("NEBULA_ARTIFACT_MAX_BYTES", "1024")]);
    let root = env.tmp.path();
    std::fs::write(root.join("secret.png"), b"secret").unwrap();

    let linked = tree(&root.join("linked"), &[("index.html", b"x")]);
    std::os::unix::fs::symlink(root.join("secret.png"), linked.join("leak.png")).unwrap();
    let escape = e2e_run(&root.join("escape"), "../secret.png");
    let missing = e2e_run(&root.join("missing"), "shots/gone.png");
    let invalid = tree(&root.join("invalid"), &[("result.json", b"{\"title\": 1}")]);
    let big = tree(&root.join("big"), &[("index.html", &[b'x'; 2048])]);

    for (dir, kind) in [
        (&linked, "recap"),
        (&escape, "e2e-run"),
        (&missing, "e2e-run"),
        (&invalid, "e2e-run"),
        (&big, "recap"),
    ] {
        let out = env.cli(
            &[
                "artifact",
                "add",
                dir.to_str().unwrap(),
                "--kind",
                kind,
                "--json",
            ],
            None,
        );
        assert!(!out.status.success(), "{dir:?} was stored");
        assert!(out.stdout.is_empty(), "{dir:?}");
    }
    assert!(artifact_ids(&env).is_empty());
    let leftovers: Vec<_> = std::fs::read_dir(env.data_dir().join("artifacts"))
        .map(|dir| dir.map(|e| e.unwrap().file_name()).collect())
        .unwrap_or_default();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    let small = tree(&root.join("small"), &[("index.html", &[b'x'; 512])]);
    publish(&env, None, &[small.to_str().unwrap(), "--kind", "recap"]);
    assert_eq!(artifact_ids(&env).len(), 1);
}

#[test]
fn artifact_add_url_link() {
    let env = board_env();
    let added = publish(&env, None, &["https://vela.possu.dev"]);
    assert_eq!(added["kind"], "link");
    assert_eq!(
        added["content"],
        serde_json::json!({"url": "https://vela.possu.dev"})
    );
    let id = added["id"].as_str().unwrap();
    assert_eq!(env.json(&["artifact", "show", id, "--json"]), added);
    assert!(!env.stored(id).exists());
}

#[test]
fn artifact_rm_soft() {
    let env = board_env();
    let page = tree(&env.tmp.path().join("page"), &[("index.html", b"x")]);
    let kept = publish(&env, None, &["https://example.com"]);
    let gone = publish(&env, None, &[page.to_str().unwrap(), "--kind", "prototype"]);
    let id = gone["id"].as_str().unwrap();

    let removed = env.json(&["artifact", "rm", id, "--json"]);
    assert!(removed["removed_at"].is_i64(), "{removed}");
    assert_eq!(artifact_ids(&env), [kept["id"].as_str().unwrap()]);
    let shown = env.json(&["artifact", "show", id, "--json"]);
    assert_eq!(shown["removed_at"], removed["removed_at"]);
    assert!(
        env.stored(id).join("index.html").is_file(),
        "rm deleted the bytes"
    );

    let unknown = env.cli(
        &["artifact", "rm", &nebula_core::ArtifactId::generate().0],
        None,
    );
    assert_eq!(unknown.status.code(), Some(5), "{}", stderr(&unknown));
}

fn refused(out: &Output, why: &str) {
    assert!(
        !out.status.success(),
        "stored: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(out.stdout.is_empty());
    assert!(stderr(out).contains(why), "{why:?} not in {}", stderr(out));
}

fn store_entries(env: &Env) -> Vec<std::ffi::OsString> {
    std::fs::read_dir(env.data_dir().join("artifacts"))
        .map(|dir| dir.map(|e| e.unwrap().file_name()).collect())
        .unwrap_or_default()
}

fn wire(env: &Env, request: nebula_core::ClientRequest) -> nebula_core::ServerEvent {
    use nebula_core::codec::{read_frame, write_frame};
    use nebula_core::{ClientRequest, ServerEvent, PROTOCOL_VERSION};
    let sock = env.runtime_dir().join("daemon.sock");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut stream = tokio::net::UnixStream::connect(sock).await.unwrap();
        let hello = ClientRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        write_frame(&mut stream, &hello).await.unwrap();
        write_frame(&mut stream, &request).await.unwrap();
        loop {
            match read_frame::<ServerEvent, _>(&mut stream).await.unwrap() {
                Some(ServerEvent::HelloOk { .. }) => continue,
                Some(reply) => return reply,
                None => panic!("daemon closed the connection"),
            }
        }
    })
}

#[test]
fn artifact_add_rerun_with_id() {
    let env = board_env();
    let page = tree(&env.tmp.path().join("page"), &[("index.html", b"one")]);
    let id = nebula_core::ArtifactId::generate().0;
    let args = [page.to_str().unwrap(), "--kind", "recap", "--id", &id];
    let first = publish(&env, None, &args);
    assert_eq!(first["id"], id.as_str());
    assert_eq!(publish(&env, None, &args), first);
    assert_eq!(artifact_ids(&env), [id.as_str()]);

    std::fs::write(page.join("index.html"), b"two").unwrap();
    let mut argv = vec!["artifact", "add"];
    argv.extend_from_slice(&args);
    let changed = env.cli(&argv, None);
    assert_eq!(changed.status.code(), Some(6), "{}", stderr(&changed));
    assert_eq!(
        std::fs::read(env.stored(&id).join("index.html")).unwrap(),
        b"one"
    );
}

#[test]
fn artifact_add_rerun_without_board_keeps_the_stored_tree() {
    let env = board_env();
    let page = tree(&env.tmp.path().join("page"), &[("index.html", b"one")]);
    let id = nebula_core::ArtifactId::generate().0;
    let args = [page.to_str().unwrap(), "--kind", "recap", "--id", &id];
    publish(&env, None, &args);

    env.write_config("{}");
    let mut argv = vec!["artifact", "add"];
    argv.extend_from_slice(&args);
    refused(&env.cli(&argv, None), "no board configured");
    assert_eq!(
        std::fs::read(env.stored(&id).join("index.html")).unwrap(),
        b"one"
    );
}

#[test]
fn artifact_add_refused_publish_drops_the_tree() {
    let env = Env::new().start(&[]);
    let page = tree(&env.tmp.path().join("page"), &[("index.html", b"x")]);
    let id = nebula_core::ArtifactId::generate().0;
    let out = env.cli(
        &[
            "artifact",
            "add",
            page.to_str().unwrap(),
            "--kind",
            "recap",
            "--id",
            &id,
        ],
        None,
    );
    refused(&out, "no board configured");
    assert!(store_entries(&env).is_empty(), "{:?}", store_entries(&env));
}

#[test]
fn artifact_entry_stays_inside_its_tree() {
    use nebula_core::board::{Artifact, ArtifactContent, BoardOp};
    use nebula_core::{ArtifactId, ClientRequest, ServerEvent};
    let env = board_env();
    for entry in ["../secret.png", "/etc/passwd", "a/../../b", ""] {
        let artifact = Artifact {
            id: ArtifactId::generate(),
            kind: "recap".into(),
            title: "escape".into(),
            card: None,
            project: None,
            session: None,
            checkout: None,
            content: ArtifactContent::Files {
                host: nebula_core::host::hostname(),
                entry: entry.into(),
            },
            meta: Default::default(),
            created_at: 1,
            removed_at: None,
        };
        let reply = wire(
            &env,
            ClientRequest::Board {
                req_id: 1,
                op: BoardOp::Publish(artifact),
            },
        );
        assert!(
            matches!(reply, ServerEvent::Error { .. }),
            "{entry:?}: {reply:?}"
        );
    }
    assert!(artifact_ids(&env).is_empty());

    let page = tree(&env.tmp.path().join("page"), &[("index.html", b"x")]);
    std::fs::write(env.tmp.path().join("secret.png"), b"secret").unwrap();
    let added = publish(&env, None, &[page.to_str().unwrap(), "--kind", "recap"]);
    let reply = wire(
        &env,
        ClientRequest::LocateArtifact {
            req_id: 1,
            id: ArtifactId(added["id"].as_str().unwrap().into()),
            entry: "../../../secret.png".into(),
        },
    );
    assert!(matches!(reply, ServerEvent::Error { .. }), "{reply:?}");
}

#[test]
fn artifact_add_refuses_symlinked_parent() {
    let env = board_env();
    let root = env.tmp.path().canonicalize().unwrap();
    tree(&root.join("real/recap"), &[("index.html", b"x")]);
    std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
    let source = root.join("alias/recap");
    let out = env.cli(
        &[
            "artifact",
            "add",
            source.to_str().unwrap(),
            "--kind",
            "recap",
        ],
        None,
    );
    refused(&out, "symlink");
    assert!(store_entries(&env).is_empty(), "{:?}", store_entries(&env));
}

#[test]
fn artifact_directory_needs_index() {
    let env = board_env();
    for kind in ["recap", "diagram", "prototype"] {
        let dir = tree(&env.tmp.path().join(kind), &[("report.html", b"x")]);
        let out = env.cli(
            &["artifact", "add", dir.to_str().unwrap(), "--kind", kind],
            None,
        );
        refused(&out, "index.html");
    }
    assert!(artifact_ids(&env).is_empty());
    let file = tree(&env.tmp.path().join("single"), &[("report.html", b"x")]).join("report.html");
    let added = publish(&env, None, &[file.to_str().unwrap(), "--kind", "recap"]);
    assert_eq!(added["content"]["entry"], "report.html");
}

#[test]
fn artifact_add_url_forms() {
    let env = board_env();
    let upper = publish(&env, None, &["HTTPS://Example.com/x"]);
    assert_eq!(upper["kind"], "link");
    assert_eq!(
        upper["content"],
        serde_json::json!({"url": "HTTPS://Example.com/x"})
    );
    let kinded = publish(&env, None, &["https://vela.possu.dev", "--kind", "recap"]);
    assert_eq!(kinded["kind"], "link");
    for hostless in ["https://", "http:///path", "https://:8080/x"] {
        let out = env.cli(&["artifact", "add", hostless], None);
        refused(&out, "host");
    }
    assert_eq!(artifact_ids(&env).len(), 2);
}

#[test]
fn artifact_project_is_callers() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "beta"], "Elsewhere");
    let out = env.cli(
        &[
            "agent",
            "new",
            "--project",
            "repo",
            "--worktree",
            "main",
            "--name",
            "publisher",
            "--json",
        ],
        None,
    );
    let agent = json_of(out, &["agent", "new"]);
    let added = publish(
        &env,
        agent["id"].as_str(),
        &["https://example.com", "--card", "#1"],
    );
    assert_eq!(added["card"], card["id"]);
    assert_eq!(added["project"], "repo");
}

#[test]
fn artifact_add_project_flag() {
    let env = board_env();
    let added = publish(
        &env,
        None,
        &[
            "https://vela.possu.dev",
            "--kind",
            "link",
            "--project",
            "vela-hub-fork",
        ],
    );
    assert_eq!(added["project"], "vela-hub-fork");
    assert_eq!(added["card"], Value::Null);
}

#[test]
fn artifact_open_uses_the_daemons_store() {
    let env = board_env();
    let page = tree(&env.tmp.path().join("page"), &[("index.html", b"x")]);
    let added = publish(&env, None, &[page.to_str().unwrap(), "--kind", "recap"]);
    let id = added["id"].as_str().unwrap();
    let bin = env.tmp.path().join("bin");
    let opened = env.tmp.path().join("opened");
    std::fs::create_dir_all(&bin).unwrap();
    for opener in ["open", "xdg-open"] {
        let script = bin.join(opener);
        std::fs::write(&script, "#!/bin/sh\nprintf '%s' \"$1\" > \"$OPENED\"\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["artifact", "open", id])
        .env("NEBULA_RUNTIME_DIR", env.runtime_dir())
        .env("NEBULA_DATA_DIR", env.tmp.path().join("elsewhere"))
        .env("PATH", path)
        .env("OPENED", &opened)
        .env_remove("NEBULA_AGENT_ID")
        .current_dir(env.tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let expected = env.stored(id).join("index.html").canonicalize().unwrap();
    assert_eq!(
        std::fs::read_to_string(&opened).unwrap(),
        expected.to_str().unwrap()
    );
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

#[test]
fn note_replay_bound_to_its_card() {
    let env = board_env();
    add(&env, &["--project", "other"], "Elsewhere");
    add(&env, &["--project", "nebula"], "Here");
    let id = nebula_core::TodoId::generate().0;
    env.json(&["todo", "note", "#1", "first", "--id", &id, "--json"]);

    for scope in [&["--scope", "nebula"][..], &[]] {
        let argv = [&["todo", "note", "#2", "first", "--id", &id][..], scope].concat();
        let out = env.cli(&argv, None);
        assert_eq!(out.status.code(), Some(6), "{argv:?}: {}", stderr(&out));
        assert!(out.stdout.is_empty(), "{argv:?} printed {:?}", out.stdout);
    }
    let replay = env.json(&["todo", "note", "#1", "first", "--id", &id, "--json"]);
    assert_eq!(replay["ref"], "#1");
    assert_eq!(
        env.json(&["todo", "show", "#2", "--json"])["log"],
        serde_json::json!([])
    );
    let log = env.json(&["todo", "show", "#1", "--json"])["log"].clone();
    assert_eq!(log.as_array().map(Vec::len), Some(1), "{log}");
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
swallow = os.environ.get('PUMP_SWALLOW', '').encode()
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
    if swallow and swallow in body and b'BoardReply' in body:
        continue
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

#[test]
fn link_qualifies_cleared_blockers_like_blocked_by() {
    let board = board_env();
    let here = viewer(&proxy_to(&board));
    add(&board, &["--project", "nebula"], "Waits");
    add(&board, &["--project", "nebula"], "Blocker");
    board.json(&["todo", "block", "#1", "--by", "#2", "--json"]);
    board.json(&["todo", "archive", "#2", "--json"]);
    let shown = here.json(&["todo", "show", "#1@boardhost", "--json"]);
    assert_eq!(shown["blocked_by"], serde_json::json!(["#2@boardhost"]));
    assert_eq!(
        shown["cleared_blockers"],
        serde_json::json!(["#2@boardhost"])
    );
}

#[test]
fn link_refusals_name_cards_with_their_host() {
    let board = board_env();
    let here = viewer(&proxy_to(&board));
    add(&board, &["--project", "nebula"], "Waits");
    let missing = here.cli(&["todo", "show", "99@boardhost"], None);
    assert!(
        stderr(&missing).contains("no card #99@boardhost"),
        "{}",
        stderr(&missing)
    );
    let moved = here.cli(
        &["todo", "move", "#1@boardhost", "doing", "--expect", "next"],
        None,
    );
    assert!(
        stderr(&moved).contains("#1@boardhost is in backlog"),
        "{}",
        stderr(&moved)
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
fn link_times_out_one_request_alone() {
    let board = board_env();
    let launches = board.tmp.path().join("launches");
    let here = viewer_on(
        &format!(
            "echo >> '{}'; PUMP_SWALLOW=Swallowed {}",
            launches.display(),
            pump_to(&board, None)
        ),
        "here",
        &[("NEBULA_BOARD_REPLY_MS", "500")],
    );
    let lost = here.cli(&["todo", "add", "--project", "nebula", "Swallowed"], None);
    assert_eq!(lost.status.code(), Some(4), "{}", stderr(&lost));
    add(&here, &["--project", "nebula"], "After");
    let launched = std::fs::read_to_string(&launches).unwrap().lines().count();
    assert_eq!(launched, 1, "the link reconnected over one late reply");
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
fn link_names_a_host_without_connect_only() {
    let here = viewer("echo \"error: unexpected argument '--connect-only' found\" >&2; exit 2");
    let out = here.cli(&["todo", "add", "--project", "nebula", "Old peer"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("without `proxy --connect-only`"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn link_refuses_a_relay_template_without_proxy() {
    let here = viewer(&proxy_to(&board_env()).replace("{proxy}", "proxy"));
    let out = here.cli(&["todo", "add", "--project", "nebula", "Plain"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("{proxy}"), "{}", stderr(&out));
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
    let marker = env.tmp.path().join("gone");
    let here = viewer(&format!(
        "[ -e '{m}' ] && exit 0; cat '{}'; cat > /dev/null",
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

    std::fs::write(&marker, "").unwrap();
    let gone = here.cli(&argv, None);
    assert_eq!(gone.status.code(), Some(3), "{}", stderr(&gone));
    assert!(stderr(&gone).contains(UNREACHABLE), "{}", stderr(&gone));
}

#[test]
fn link_stops_when_the_board_turns_local() {
    let env = Env::new();
    let launches = env.tmp.path().join("launches");
    let here = viewer(&format!("echo >> '{}'; exit 0", launches.display()));
    let count = || {
        std::fs::read_to_string(&launches)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let out = here.cli(&["todo", "add", "--project", "nebula", "Away"], None);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));

    here.board_local();
    let out = here.cli(&["todo", "list", "--all"], None);
    assert!(out.status.success(), "{}", stderr(&out));
    let before = count();
    assert!(before > 0, "the link never launched its transport");
    let relaunched = poll_until(Duration::from_secs(7), || (count() > before).then_some(()));
    assert_eq!(
        relaunched, None,
        "the old board host's link kept reconnecting"
    );
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

fn run_card(env: &Env, card: &str, extra: &[&str]) -> Output {
    run_card_as(env, card, &[], extra)
}

fn run_argv(env: &Env, card: &str, set: &[(&str, &str)], extra: &[&str]) -> Vec<String> {
    let host = env.host_name();
    let mut flags = vec![
        ("--role", "worker"),
        ("--kind", "claude"),
        ("--host", host.as_str()),
        ("--worktree", "main"),
    ];
    for (flag, value) in set {
        flags.iter_mut().find(|f| f.0 == *flag).unwrap().1 = value;
    }
    let mut argv: Vec<String> = ["todo", "run", card, "--json"].map(String::from).to_vec();
    for (flag, value) in flags {
        argv.extend([flag.to_string(), value.to_string()]);
    }
    argv.extend(extra.iter().map(|s| s.to_string()));
    argv
}

fn run_card_as(env: &Env, card: &str, set: &[(&str, &str)], extra: &[&str]) -> Output {
    let argv = run_argv(env, card, set, extra);
    env.cli(&argv.iter().map(String::as_str).collect::<Vec<_>>(), None)
}

fn begin_run(env: &Env, card: &Value, dispatch: &str, worktree: &str) {
    let begun = board_op(
        env,
        BoardOp::BeginRun(Dispatch {
            id: DispatchId(dispatch.into()),
            card: CardRef::Id(TodoId(card["id"].as_str().unwrap().into())),
            host: env.host_name(),
            role: Role::Worker,
            kind: AgentKind::Claude,
            worktree: worktree.into(),
        }),
    )
    .expect("the board begins a run");
    let BoardResult::Run(begun) = begun else {
        panic!("BeginRun answered {begun:?}");
    };
    assert_eq!(begun.state, DispatchState::Pending);
}

fn lost_spawn(env: &Env, card: &str, dispatch: &str) -> Value {
    json_of(
        spawn_agent(
            env,
            &[
                "--card",
                card,
                "--role",
                "worker",
                "--dispatch",
                dispatch,
                "--name",
                "lost",
            ],
        ),
        &["agent", "new", "--dispatch", dispatch],
    )
}

fn receipt(out: Output) -> Value {
    json_of(out, &["todo", "run"])
}

fn agents_of(env: &Env) -> Vec<Value> {
    env.json(&["agent", "list", "--project", "repo"])
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn run_moves_card_to_doing() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Run me");
    let run = receipt(run_card(&env, "#1", &[]));
    assert_eq!(run["state"], "started");
    assert_eq!(run["card"], card["id"]);
    assert_eq!(run["session"]["host"], nebula_core::host::hostname());
    let session = run["session"]["id"].as_str().unwrap().to_string();
    let agents = agents_of(&env);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["id"], session.as_str());
    let sessions = eventually("the worker under the card", || {
        let sessions = chain(&env, "#1");
        (!sessions.is_empty()).then_some(sessions)
    });
    assert_eq!(sessions[0]["id"], session.as_str());
    assert_eq!(sessions[0]["role"], "worker");
    eventually("the card in doing", || {
        (stage_of(&env, "#1") == "doing").then_some(())
    });
}

#[test]
fn run_same_dispatch_spawns_once() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Once");
    let dispatch = TodoId::generate().0;
    let first = receipt(run_card(&env, "#1", &["--dispatch", &dispatch]));
    let again = receipt(run_card(&env, "#1", &["--dispatch", &dispatch]));
    assert_eq!(first["state"], "started");
    assert_eq!(again, first);
    assert_eq!(agents_of(&env).len(), 1);

    let lost = TodoId::generate().0;
    let pending = board_op(
        &env,
        BoardOp::BeginRun(Dispatch {
            id: DispatchId(lost.clone()),
            card: CardRef::Id(TodoId(card["id"].as_str().unwrap().into())),
            host: nebula_core::host::hostname(),
            role: Role::Worker,
            kind: AgentKind::Claude,
            worktree: "main".into(),
        }),
    )
    .expect("the board begins a run");
    let BoardResult::Run(pending) = pending else {
        panic!("BeginRun answered {pending:?}");
    };
    assert_eq!(pending.state, DispatchState::Pending);
    let spawned = json_of(
        spawn_agent(
            &env,
            &[
                "--card",
                "#1",
                "--role",
                "worker",
                "--dispatch",
                &lost,
                "--name",
                "lost",
            ],
        ),
        &["agent", "new", "--dispatch"],
    );
    let rerun = receipt(run_card(&env, "#1", &["--dispatch", &lost]));
    assert_eq!(rerun["state"], "started");
    assert_eq!(rerun["session"]["id"], spawned["id"]);
    assert_eq!(agents_of(&env).len(), 2);
}

#[test]
fn run_refuses_blocked_or_archived() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Blocked");
    add(&env, &["--project", "repo"], "Blocker");
    add(&env, &["--project", "repo"], "Shelved");
    env.json(&["todo", "block", "#1", "--by", "#2", "--json"]);
    env.json(&["todo", "archive", "#3", "--json"]);
    for card in ["#1", "#3"] {
        let refused = run_card(&env, card, &[]);
        assert_eq!(
            refused.status.code(),
            Some(6),
            "{card}: {}",
            stderr(&refused)
        );
    }
    assert!(agents_of(&env).is_empty());
    env.json(&["todo", "move", "#2", "done", "--json"]);
    assert_eq!(receipt(run_card(&env, "#1", &[]))["state"], "started");
}

#[test]
fn run_treats_an_archived_blocker_as_cleared() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Blocked");
    add(&env, &["--project", "repo"], "Shelved blocker");
    env.json(&["todo", "block", "#1", "--by", "#2", "--json"]);
    env.json(&["todo", "archive", "#2", "--json"]);
    let shown = env.json(&["todo", "show", "#1", "--json"]);
    assert_eq!(shown["cleared_blockers"], serde_json::json!(["#2"]));
    let run = run_card(&env, "#1", &[]);
    assert_eq!(run.status.code(), Some(0), "{}", stderr(&run));
    assert_eq!(receipt(run)["state"], "started");
}

#[test]
fn run_writer_busy_fails_dispatch() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Busy");
    json_of(
        spawn_agent(&env, &["--writer", "--name", "holder"]),
        &["agent", "new", "--writer"],
    );
    let dispatch = TodoId::generate().0;
    let refused = run_card(&env, "#1", &["--dispatch", &dispatch]);
    assert_eq!(refused.status.code(), Some(6), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("\"holder\" is the writer on main"),
        "{}",
        stderr(&refused)
    );
    let failed: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["session"], Value::Null);
    let rerun = run_card(&env, "#1", &["--dispatch", &dispatch]);
    assert_eq!(rerun.status.code(), Some(6), "{}", stderr(&rerun));
    let names: Vec<Value> = agents_of(&env).iter().map(|a| a["name"].clone()).collect();
    assert_eq!(names, ["holder"]);
}

#[test]
fn run_writer_name_keeps_the_refusal() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Busy");
    json_of(
        spawn_agent(&env, &["--writer", "--name", "holder; nothing was sent"]),
        &["agent", "new", "--writer"],
    );
    let dispatch = TodoId::generate().0;
    let refused = run_card(&env, "#1", &["--dispatch", &dispatch]);
    assert_eq!(refused.status.code(), Some(6), "{}", stderr(&refused));
    let failed: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(failed["state"], "failed");
}

#[test]
fn run_retry_scope_covers_the_admitted_project() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Moved");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &card, &dispatch, "main");
    env.json(&["todo", "edit", "#1", "--project", "twin", "--json"]);
    let refused = run_card(&env, "#1", &["--dispatch", &dispatch, "--scope", "twin"]);
    assert_eq!(refused.status.code(), Some(5), "{}", stderr(&refused));
    assert!(agents_of(&env).is_empty());
}

#[test]
fn run_recovery_matches_role_and_kind() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Roles");
    for (role, kind, name) in [
        ("review", "claude", "reviewer"),
        ("worker", "codex", "coder"),
    ] {
        let dispatch = TodoId::generate().0;
        begin_run(&env, &card, &dispatch, "main");
        let argv = [
            "--card",
            "#1",
            "--role",
            role,
            "--kind",
            kind,
            "--dispatch",
            &dispatch,
            "--name",
            name,
        ];
        json_of(spawn_agent(&env, &argv), &["agent", "new", name]);
        let refused = run_card(&env, "#1", &["--dispatch", &dispatch]);
        assert_eq!(
            refused.status.code(),
            Some(6),
            "{name}: {}",
            stderr(&refused)
        );
    }
    assert_eq!(agents_of(&env).len(), 2);
}

#[test]
fn run_lost_end_reconciles_before_card_checks() {
    let env = board_env();
    env.make_repo();
    let shelved = add(&env, &["--project", "repo"], "Shelved");
    add(&env, &["--project", "repo"], "Blocker");
    let blocked = add(&env, &["--project", "repo"], "Blocked");
    let (first, second) = (TodoId::generate().0, TodoId::generate().0);
    begin_run(&env, &shelved, &first, "main");
    begin_run(&env, &blocked, &second, "main");
    let spawned = [
        lost_spawn(&env, "#1", &first),
        lost_spawn(&env, "#3", &second),
    ];
    for (card, agent) in [("#1", &spawned[0]), ("#3", &spawned[1])] {
        eventually("the launched session on its card", || {
            chain(&env, card)
                .iter()
                .any(|s| s["id"] == agent["id"])
                .then_some(())
        });
    }
    env.json(&["todo", "archive", "#1", "--json"]);
    env.json(&["todo", "block", "#3", "--by", "#2", "--json"]);
    for (card, dispatch, agent) in [("#1", &first, &spawned[0]), ("#3", &second, &spawned[1])] {
        let rerun = receipt(run_card(&env, card, &["--dispatch", dispatch]));
        assert_eq!(rerun["state"], "started", "{card}");
        assert_eq!(rerun["session"]["id"], agent["id"], "{card}");
    }
    assert_eq!(agents_of(&env).len(), 2);
}

#[test]
fn run_lost_end_takes_the_live_session() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Replaced");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &card, &dispatch, "main");
    let dead = lost_spawn(&env, "#1", &dispatch);
    let dead = dead["id"].as_str().unwrap().to_string();
    eventually("the first session on the card", || {
        chain(&env, "#1")
            .iter()
            .any(|s| s["id"] == dead.as_str())
            .then_some(())
    });
    let deleted = env.cli(
        &["agent", "delete-id", "--project", "repo", "--", &dead],
        None,
    );
    assert!(deleted.status.success(), "{}", stderr(&deleted));
    let live = lost_spawn(&env, "#1", &dispatch);
    let live = live["id"].as_str().unwrap().to_string();
    assert_ne!(live, dead);
    eventually("the dead and the live session on the card", || {
        let rows = chain(&env, "#1");
        let alive = |id: &str| {
            rows.iter()
                .find(|s| s["id"] == id)
                .map(|s| s["alive"] == true)
        };
        (alive(&dead) == Some(false) && alive(&live) == Some(true)).then_some(())
    });
    let rerun = receipt(run_card(&env, "#1", &["--dispatch", &dispatch]));
    assert_eq!(rerun["state"], "started");
    assert_eq!(rerun["session"]["id"], live.as_str());
}

#[test]
fn run_replay_names_the_agents_own_project() {
    let env = board_env();
    env.make_repo();
    env.make_repo_named("twin");
    let card = add(&env, &["--project", "repo"], "Twin");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &card, &dispatch, "main");
    let first = lost_spawn(&env, "#1", &dispatch);
    let argv = [
        "agent",
        "new",
        "--project",
        "twin",
        "--worktree",
        "main",
        "--card",
        "#1",
        "--role",
        "worker",
        "--dispatch",
        &dispatch,
        "--name",
        "lost",
    ];
    let again = json_of(env.cli(&argv, None), &argv);
    assert_eq!(again["project"], "repo");
    assert_eq!(again, first);
}

#[test]
fn agent_new_dispatch_only_on_the_admitted_host() {
    let board = board_env();
    let laptop = viewer_on(&proxy_to(&board), "laptop", &[]);
    laptop.make_repo();
    let card = add(&laptop, &["--project", "repo"], "Admitted");
    let unadmitted = TodoId::generate().0;
    let refused = spawn_agent(
        &laptop,
        &[
            "--card",
            "#1@boardhost",
            "--role",
            "review",
            "--dispatch",
            &unadmitted,
            "--name",
            "unadmitted",
        ],
    );
    assert_eq!(refused.status.code(), Some(6), "{}", stderr(&refused));
    assert!(agents_of(&laptop).is_empty());
    let dispatch = TodoId::generate().0;
    begin_run(&board, &card, &dispatch, "main");
    let refused = spawn_agent(
        &laptop,
        &[
            "--card",
            "#1@boardhost",
            "--role",
            "worker",
            "--dispatch",
            &dispatch,
            "--name",
            "stray",
        ],
    );
    assert_eq!(refused.status.code(), Some(6), "{}", stderr(&refused));
    assert!(agents_of(&laptop).is_empty());
}

fn session_row(env: &Env, id: &str) -> Value {
    agents_of(env).into_iter().find(|a| a["id"] == id).unwrap()
}

#[test]
fn approve_checks_status_timestamp() {
    let replay = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/replay");
    let agent = format!(
        "{} {}",
        replay.join("agent").display(),
        replay.join("ask.replay").display()
    );
    let env = Env::new();
    env.board_local();
    let input = env.tmp.path().join("input.log");
    let env = env.start(&[
        ("NEBULA_AGENT_CMD", agent.as_str()),
        ("NEBULA_STATUS_TICK_MS", "100"),
        ("REPLAY_INPUT_LOG", input.to_str().unwrap()),
    ]);
    let inputs = || {
        std::fs::read_to_string(&input)
            .map(|log| log.lines().count())
            .unwrap_or(0)
    };
    env.make_repo();
    add(&env, &["--project", "repo"], "Ask");
    let run = receipt(run_card(&env, "#1", &[]));
    let session = run["session"]["id"].as_str().unwrap().to_string();
    let approve = |at: i64, scope: &[&str]| {
        let at = at.to_string();
        let mut argv = vec![
            "todo",
            "approve",
            "#1",
            "--session",
            &session,
            "--expected-status-at",
            &at,
            "--json",
        ];
        argv.extend_from_slice(scope);
        env.cli(&argv, None)
    };
    eventually("the session on the card", || {
        chain(&env, "#1")
            .iter()
            .any(|s| s["id"] == session.as_str())
            .then_some(())
    });
    let fresh = session_row(&env, &session);
    let not_asking = approve(fresh["status_changed_at"].as_i64().unwrap(), &[]);
    assert_eq!(not_asking.status.code(), Some(6), "{}", stderr(&not_asking));
    assert_eq!(inputs(), 0, "a session that asked nothing got the key");
    let name = fresh["name"].as_str().unwrap().to_string();
    for text in ["go", "on"] {
        let sent = env.cli(&["agent", "send", &name, text, "--project", "repo"], None);
        assert!(sent.status.success(), "{}", stderr(&sent));
    }
    let asked = eventually("the session asking", || {
        let row = session_row(&env, &session);
        (row["status"] == "needs_feedback").then_some(row)
    });
    let at = asked["status_changed_at"].as_i64().unwrap();
    eventually("the asking session on the card", || {
        chain(&env, "#1")
            .iter()
            .any(|s| s["id"] == session.as_str() && s["status_changed_at"] == at)
            .then_some(())
    });
    let before = eventually("both prompts read", || (inputs() == 2).then_some(2));
    for (stamp, scope, code) in [(at - 1, &[][..], 6), (at, &["--include-home"][..], 5)] {
        let refused = approve(stamp, scope);
        assert_eq!(
            refused.status.code(),
            Some(code),
            "{scope:?}: {}",
            stderr(&refused)
        );
        let row = session_row(&env, &session);
        assert_eq!(row["status"], "needs_feedback", "{scope:?}");
        assert_eq!(row["status_changed_at"], at, "{scope:?}");
        assert_eq!(inputs(), before, "{scope:?}: a refused approve sent input");
    }
    let current = approve(at, &[]);
    assert!(current.status.success(), "{}", stderr(&current));
    eventually("the session moving on", || {
        let row = session_row(&env, &session);
        (row["status"] != "needs_feedback" && row["status_changed_at"].as_i64() > Some(at))
            .then_some(())
    });
    assert_eq!(inputs(), before + 1);
}

#[test]
fn cross_host_child_keeps_parent() {
    let board = board_env();
    let here = viewer_on(&proxy_to(&board), "laptop", &[]);
    here.make_repo();
    add(&here, &["--project", "repo"], "Far work");
    let worker = json_of(
        spawn_agent(
            &here,
            &[
                "--card",
                "#1@boardhost",
                "--role",
                "worker",
                "--name",
                "worker",
            ],
        ),
        &["agent", "new", "worker"],
    );
    let worker_id = worker["id"].as_str().unwrap();
    let child = json_of(
        here.cli(
            &[
                "agent",
                "new",
                "--project",
                "repo",
                "--worktree",
                "main",
                "--role",
                "review",
                "--name",
                "child",
            ],
            Some(worker_id),
        ),
        &["agent", "new", "child"],
    );
    let sessions = eventually("both sessions on the board", || {
        let sessions = chain(&board, "#1");
        (sessions.len() == 2).then_some(sessions)
    });
    let listed = sessions.iter().find(|s| s["id"] == child["id"]).unwrap();
    assert_eq!(listed["role"], "review");
    assert_eq!(listed["host"], "laptop");
    assert_eq!(
        listed["parent"],
        serde_json::json!({"host": "laptop", "id": worker_id})
    );
    eventually("the card in doing", || {
        (stage_of(&board, "#1") == "doing").then_some(())
    });
}

#[test]
fn cross_host_stale_after_link_down() {
    let env = Env::new();
    env.board_local();
    let env = env.start(&[("NEBULA_BOARD_STALE_MS", "1500")]);
    let card = add(&env, &["--project", "nebula"], "Far work");
    let report = || far_report(&env, vec![far_row(&card, "w1", AgentStatus::Running, 10)]);
    report();
    let version = env.json(&["todo", "board", "--all", "--json"])["version"]
        .as_str()
        .unwrap()
        .to_string();
    let stale_version = eventually("the far host reading stale on a poll", || {
        let polled = env.json(&["todo", "board", "--all", "--since", &version, "--json"]);
        let hosts = polled["board"]["hosts"].as_array()?.clone();
        (far_host(&hosts)["stale"] == true).then(|| polled["version"].as_str().unwrap().to_string())
    });
    assert_eq!(chain(&env, "#1")[0]["stale"], true);
    report();
    assert_eq!(chain(&env, "#1")[0]["stale"], false);
    assert_eq!(far_host(&hosts_of(&env, &["--all"]))["stale"], false);
    let polled = env.json(&[
        "todo",
        "board",
        "--all",
        "--since",
        &stale_version,
        "--json",
    ]);
    let hosts = polled["board"]["hosts"]
        .as_array()
        .unwrap_or_else(|| panic!("a poll since the stale version saw no change: {polled}"));
    assert_eq!(far_host(hosts)["stale"], false);
}

fn viewer_on(relay_cmd: &str, host: &str, envs: &[(&str, &str)]) -> Env {
    let env = Env::new().on_host(host);
    env.write_config(&board_at("boardhost"));
    let mut all = vec![("NEBULA_RELAY_CMD", relay_cmd)];
    all.extend_from_slice(envs);
    env.start(&all)
}

fn reported(board: &Env, host: &str) {
    eventually("the host reported to the board", || {
        hosts_of(board, &["--all"])
            .iter()
            .any(|h| h["host"] == host)
            .then_some(())
    });
}

fn ssh_stub(env: &Env, target: &Env) -> (String, PathBuf, PathBuf) {
    ssh_stub_as(env, target, &target.host_name())
}

fn ssh_stub_as(env: &Env, target: &Env, host: &str) -> (String, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let home = env.tmp.path().join("ssh-home");
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = env.tmp.path().join("ssh-argv");
    let down = env.tmp.path().join("ssh-down");
    let nebula = Path::new(env!("CARGO_BIN_EXE_nebula")).parent().unwrap();
    std::fs::write(
        bin.join("ssh"),
        format!(
            "#!/bin/sh\n[ \"$(cat '{log}' 2>/dev/null | grep -c '^BatchMode=yes$')\" -ge 3 ] && exit 255\nprintf '%s\\n' \"$@\" >> '{log}'\n[ -e '{down}' ] && {{ echo 'ssh: connect to host: Connection refused' >&2; exit 255; }}\nfor arg do script=$arg; done\nunset NEBULA_AGENT_ID\nexport HOME='{home}' NEBULA_RUNTIME_DIR='{rt}' NEBULA_DATA_DIR='{data}' NEBULA_HOSTNAME='{host}' PATH='{nebula}':\"$PATH\"\n[ -e '{lost}' ] && {{ /bin/sh -c \"$script\" >/dev/null 2>&1; . '{lost}'; echo 'Connection to host closed by remote host.' >&2; exit 255; }}\nexec /bin/sh -c \"$script\"\n",
            log = log.display(),
            down = down.display(),
            lost = env.tmp.path().join("ssh-lost").display(),
            home = home.display(),
            rt = target.runtime_dir().display(),
            data = target.data_dir().display(),
            nebula = nebula.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    (path, log, down)
}

fn replay_agent(fixture: &str) -> String {
    let replay = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/replay");
    format!(
        "{} {}",
        replay.join("agent").display(),
        replay.join(fixture).display()
    )
}

#[test]
fn cross_host_approve_over_ssh() {
    let board = board_env();
    let agent = replay_agent("ask.replay");
    let laptop = viewer_on(
        &proxy_to(&board),
        "laptop",
        &[
            ("NEBULA_AGENT_CMD", agent.as_str()),
            ("NEBULA_STATUS_TICK_MS", "100"),
        ],
    );
    laptop.make_repo();
    add(&laptop, &["--project", "repo"], "Far ask");
    reported(&board, "laptop");
    let run = receipt(run_card(&laptop, "#1@boardhost", &[]));
    assert_eq!(run["session"]["host"], "laptop");
    let session = run["session"]["id"].as_str().unwrap().to_string();
    let name = session_row(&laptop, &session)["name"]
        .as_str()
        .unwrap()
        .to_string();
    for text in ["go", "on"] {
        let sent = laptop.cli(&["agent", "send", &name, text, "--project", "repo"], None);
        assert!(sent.status.success(), "{}", stderr(&sent));
    }
    let at = eventually("the laptop session asking on the board", || {
        chain(&board, "#1")
            .iter()
            .find(|s| s["id"] == session.as_str() && s["status"] == "needs_feedback")
            .and_then(|s| s["status_changed_at"].as_i64())
    })
    .to_string();
    let (path, log, down) = ssh_stub(&board, &laptop);
    let approve = || {
        board.cli_env(
            &[
                "todo",
                "approve",
                "#1",
                "--session",
                &session,
                "--expected-status-at",
                &at,
                "--json",
            ],
            None,
            &[("PATH", &path)],
        )
    };
    std::fs::write(&down, "").unwrap();
    let unreachable = approve();
    assert_eq!(
        unreachable.status.code(),
        Some(3),
        "{}",
        stderr(&unreachable)
    );
    assert_eq!(session_row(&laptop, &session)["status"], "needs_feedback");
    std::fs::remove_file(&down).unwrap();
    let approved = json_of(approve(), &["todo", "approve"]);
    assert_eq!(approved["approved"], true);
    assert_eq!(approved["session"]["host"], "laptop");
    let argv = std::fs::read_to_string(&log).unwrap();
    assert!(
        argv.lines().any(|l| l == "BatchMode=yes") && argv.lines().any(|l| l == "laptop"),
        "ssh argv: {argv:?}"
    );
    eventually("the laptop session moving on", || {
        (session_row(&laptop, &session)["status"] != "needs_feedback").then_some(())
    });
}

#[test]
fn cross_host_run_over_ssh() {
    let board = board_env();
    let laptop = viewer_on(&proxy_to(&board), "laptop", &[]);
    laptop.make_repo();
    let card = add(&board, &["--project", "repo"], "Far run");
    reported(&board, "laptop");
    let (path, _, _) = ssh_stub(&board, &laptop);
    let dispatch = TodoId::generate().0;
    let argv = run_argv(
        &board,
        "#1",
        &[("--host", "laptop")],
        &["--dispatch", &dispatch],
    );
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let run = receipt(board.cli_env(&argv, None, &[("PATH", &path)]));
    assert_eq!(run["state"], "started");
    assert_eq!(run["card"], card["id"]);
    assert_eq!(run["session"]["host"], "laptop");
    let agents = agents_of(&laptop);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["id"], run["session"]["id"]);
    eventually("the far worker under the card", || {
        chain(&board, "#1")
            .iter()
            .any(|s| s["id"] == run["session"]["id"] && s["host"] == "laptop")
            .then_some(())
    });
    let again = receipt(board.cli_env(&argv, None, &[("PATH", &path)]));
    assert_eq!(again, run);
    assert_eq!(agents_of(&laptop).len(), 1);
}

#[test]
fn run_host_is_a_reported_host() {
    let board = board_env();
    let laptop = viewer_on(&proxy_to(&board), "laptop", &[]);
    laptop.make_repo();
    add(&board, &["--project", "repo"], "Hosts");
    reported(&board, "laptop");
    let run_as = |host: &str, landing: &str| {
        let (path, log, _) = ssh_stub_as(&board, &laptop, landing);
        let argv = run_argv(&board, "#1", &[("--host", host)], &[]);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let out = board.cli_env(&argv, None, &[("PATH", &path)]);
        let hops = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "BatchMode=yes")
            .count();
        std::fs::remove_file(&log).ok();
        (out, hops)
    };
    let (unknown, hops) = run_as("user@laptop", "laptop");
    assert_eq!(unknown.status.code(), Some(6), "{}", stderr(&unknown));
    assert_eq!(hops, 0);
    let (elsewhere, hops) = run_as("laptop", "otherbox");
    assert_eq!(elsewhere.status.code(), Some(6), "{}", stderr(&elsewhere));
    assert!(
        stderr(&elsewhere).contains("laptop is not this host (otherbox)"),
        "{}",
        stderr(&elsewhere)
    );
    assert_eq!(hops, 1);
    assert!(agents_of(&laptop).is_empty());
    let (upper, hops) = run_as("LAPTOP", "laptop");
    let run = receipt(upper);
    assert_eq!(run["state"], "started");
    assert_eq!(run["session"]["host"], "laptop");
    assert_eq!(hops, 1);
    assert_eq!(agents_of(&laptop).len(), 1);
}

#[test]
fn cross_host_run_keeps_option_values() {
    let board = board_env();
    let laptop = viewer_on(&proxy_to(&board), "laptop", &[]);
    laptop.make_repo_named("-ops");
    add(&board, &["--project=-ops"], "Dashed");
    reported(&board, "laptop");
    let (path, _, _) = ssh_stub(&board, &laptop);
    let argv = run_argv(&board, "#1", &[("--host", "laptop")], &["--scope=-ops"]);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let run = receipt(board.cli_env(&argv, None, &[("PATH", &path)]));
    assert_eq!(run["state"], "started");
    let agents = laptop.json(&["agent", "list", "--project=-ops"]);
    assert_eq!(agents.as_array().map(Vec::len), Some(1), "{agents}");
}

#[test]
fn cross_host_run_over_ssh_keeps_parent() {
    let board = board_env();
    board.make_repo();
    let laptop = viewer_on(&proxy_to(&board), "laptop", &[]);
    laptop.make_repo();
    add(&board, &["--project", "repo"], "Far child");
    reported(&board, "laptop");
    let lead = json_of(
        spawn_agent(
            &board,
            &["--card", "#1", "--role", "research", "--name", "lead"],
        ),
        &["agent", "new", "lead"],
    );
    let lead = lead["id"].as_str().unwrap();
    let (path, _, _) = ssh_stub(&board, &laptop);
    let argv = run_argv(&board, "#1", &[("--host", "laptop")], &[]);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let run = receipt(board.cli_env(&argv, Some(lead), &[("PATH", &path)]));
    let child = eventually("the far child on the card", || {
        chain(&board, "#1")
            .into_iter()
            .find(|s| s["id"] == run["session"]["id"] && s["host"] == "laptop")
    });
    assert_eq!(
        child["parent"],
        serde_json::json!({"host": nebula_core::host::hostname(), "id": lead})
    );
}

#[test]
fn cross_host_ssh_lost_exit_4() {
    let board = board_env();
    let agent = replay_agent("ask.replay");
    let laptop = viewer_on(
        &proxy_to(&board),
        "laptop",
        &[
            ("NEBULA_AGENT_CMD", agent.as_str()),
            ("NEBULA_STATUS_TICK_MS", "100"),
        ],
    );
    laptop.make_repo();
    add(&board, &["--project", "repo"], "Lost reply");
    reported(&board, "laptop");
    let (path, _, _) = ssh_stub(&board, &laptop);
    let lost = board.tmp.path().join("ssh-lost");
    std::fs::write(&lost, "").unwrap();
    let argv = run_argv(&board, "#1", &[("--host", "laptop")], &[]);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let cut = board.cli_env(&argv, None, &[("PATH", &path)]);
    assert_eq!(cut.status.code(), Some(4), "{}", stderr(&cut));
    let dispatch = stderr(&cut)
        .split("--dispatch ")
        .nth(1)
        .map(|rest| rest.split_whitespace().next().unwrap().to_string())
        .unwrap_or_else(|| panic!("no dispatch named: {}", stderr(&cut)));
    let agents = agents_of(&laptop);
    assert_eq!(agents.len(), 1);
    std::fs::remove_file(&lost).unwrap();
    let rerun = run_argv(
        &board,
        "#1",
        &[("--host", "laptop")],
        &["--dispatch", &dispatch],
    );
    let rerun: Vec<&str> = rerun.iter().map(String::as_str).collect();
    let run = receipt(board.cli_env(&rerun, None, &[("PATH", &path)]));
    assert_eq!(run["session"]["id"], agents[0]["id"]);
    assert_eq!(agents_of(&laptop).len(), 1);

    let session = run["session"]["id"].as_str().unwrap().to_string();
    let name = agents[0]["name"].as_str().unwrap().to_string();
    for text in ["go", "on"] {
        let sent = laptop.cli(&["agent", "send", &name, text, "--project", "repo"], None);
        assert!(sent.status.success(), "{}", stderr(&sent));
    }
    let at = eventually("the laptop session asking on the board", || {
        chain(&board, "#1")
            .iter()
            .find(|s| s["id"] == session.as_str() && s["status"] == "needs_feedback")
            .and_then(|s| s["status_changed_at"].as_i64())
    })
    .to_string();
    std::fs::write(&lost, "").unwrap();
    let approve = board.cli_env(
        &[
            "todo",
            "approve",
            "#1",
            "--session",
            &session,
            "--expected-status-at",
            &at,
            "--json",
        ],
        None,
        &[("PATH", &path)],
    );
    assert_eq!(approve.status.code(), Some(4), "{}", stderr(&approve));
    assert!(
        stderr(&approve).contains(&session) && stderr(&approve).contains(&at),
        "{}",
        stderr(&approve)
    );
    eventually("the laptop session moving on", || {
        (session_row(&laptop, &session)["status"] != "needs_feedback").then_some(())
    });
}

#[test]
fn cross_host_ssh_ambiguous_loss_exit_4() {
    let board = board_env();
    let laptop = viewer_on(&proxy_to(&board), "laptop", &[]);
    laptop.make_repo();
    reported(&board, "laptop");
    let (path, _, _) = ssh_stub(&board, &laptop);
    let lost = board.tmp.path().join("ssh-lost");
    let cases = [
        (
            "Aux channel",
            "echo 'channel 2: open failed: connect failed: Connection refused' >&2",
        ),
        ("Killed", "kill -9 $$"),
    ];
    for (n, (title, after)) in cases.into_iter().enumerate() {
        add(&board, &["--project", "repo"], title);
        std::fs::write(&lost, after).unwrap();
        let card = format!("#{}", n + 1);
        let set = [("--host", "laptop"), ("--role", "review")];
        let argv = run_argv(&board, &card, &set, &[]);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let cut = board.cli_env(&argv, None, &[("PATH", &path)]);
        assert_eq!(cut.status.code(), Some(4), "{title}: {}", stderr(&cut));
        assert!(
            stderr(&cut).contains("--dispatch "),
            "{title}: {}",
            stderr(&cut)
        );
        assert_eq!(agents_of(&laptop).len(), n + 1, "{title}");
    }
}

#[test]
fn cross_host_report_resent_on_connect() {
    let mut board = board_env();
    let here = viewer_on(&proxy_to(&board), "laptop", &[]);
    here.make_repo();
    add(&here, &["--project", "repo"], "Resend");
    json_of(
        spawn_agent(
            &here,
            &[
                "--card",
                "#1@boardhost",
                "--role",
                "worker",
                "--name",
                "worker",
            ],
        ),
        &["agent", "new", "worker"],
    );
    let alive = |board: &Env| chain(board, "#1").first().map(|s| s["alive"] == true);
    eventually("the worker live on the board", || {
        (alive(&board) == Some(true)).then_some(())
    });
    let forgotten = HostReport {
        host: "laptop".into(),
        writers: 0,
        writer_slots: None,
        checkouts: Vec::new(),
        sessions: Vec::new(),
    };
    board_op(&board, BoardOp::Report(forgotten)).expect("the board takes a host report");
    assert_eq!(alive(&board), Some(false));
    stop(&mut board);
    let board = board.start(&[]);
    here.json(&["todo", "show", "#1@boardhost", "--json"]);
    eventually(
        "the unchanged report sent again once the link is back",
        || (alive(&board) == Some(true)).then_some(()),
    );
}

fn cut_after(board: &Env, needle: &str, count: usize) -> (String, PathBuf) {
    let script = board.tmp.path().join("cut.py");
    std::fs::write(
        &script,
        r#"import os, socket, sys, threading
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
needle, count, down, once = sys.argv[2].encode(), int(sys.argv[3]), sys.argv[4], sys.argv[5]
armed = not os.path.exists(once)
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
seen = 0
while True:
    head = exact(4)
    body = exact(int.from_bytes(head, 'little'))
    os.write(1, head + body)
    if armed and needle in body:
        seen += 1
        if seen == count:
            open(once, 'w').close()
            open(down, 'w').close()
            os._exit(0)
"#,
    )
    .unwrap();
    let down = board.tmp.path().join("cut-down");
    let line = format!(
        "[ -e '{down}' ] && exit 1; exec python3 '{}' '{}' '{needle}' {count} '{down}' '{}'",
        script.display(),
        board.runtime_dir().join("daemon.sock").display(),
        board.tmp.path().join("cut-once").display(),
        down = down.display(),
    );
    (line, down)
}

#[test]
fn run_end_lost_exit_4() {
    let board = board_env();
    let (relay, down) = cut_after(&board, "Show", 1);
    let here = viewer(&relay);
    here.make_repo();
    add(&here, &["--project", "repo"], "Cut");
    let dispatch = TodoId::generate().0;
    let lost = run_card(&here, "#1@boardhost", &["--dispatch", &dispatch]);
    assert_eq!(lost.status.code(), Some(4), "{}", stderr(&lost));
    assert!(
        stderr(&lost).contains(&format!("--dispatch {dispatch}")),
        "{}",
        stderr(&lost)
    );
    let agents = agents_of(&here);
    assert_eq!(agents.len(), 1);
    std::fs::remove_file(&down).unwrap();
    let rerun = receipt(run_card(&here, "#1@boardhost", &["--dispatch", &dispatch]));
    assert_eq!(rerun["state"], "started");
    assert_eq!(rerun["session"]["id"], agents[0]["id"]);
    assert_eq!(agents_of(&here).len(), 1);
}

#[test]
fn run_dispatch_id_canonical() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Canonical");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &card, &dispatch, "main");
    let spawned = lost_spawn(&env, "#1", &dispatch.to_lowercase());
    let rerun = receipt(run_card(&env, "#1", &["--dispatch", &dispatch]));
    assert_eq!(rerun["session"]["id"], spawned["id"]);
    assert_eq!(agents_of(&env).len(), 1);
    let refused = spawn_agent(&env, &["--dispatch", "not-a-ulid", "--name", "odd"]);
    assert!(!refused.status.success());
    assert_eq!(agents_of(&env).len(), 1);
}

#[test]
fn run_replayed_spawn_names_the_live_agent() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Replay");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &card, &dispatch, "main");
    let first = lost_spawn(&env, "#1", &dispatch);
    let again = json_of(
        spawn_agent(
            &env,
            &[
                "--card",
                "#1",
                "--kind",
                "codex",
                "--role",
                "review",
                "--dispatch",
                &dispatch,
                "--name",
                "second",
            ],
        ),
        &["agent", "new", "second"],
    );
    assert_eq!(again, first);
    assert_eq!(agents_of(&env).len(), 1);
}

#[test]
fn run_rerun_bound_to_its_dispatch() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Bound");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &card, &dispatch, "main");
    for set in [
        ("--kind", "codex"),
        ("--host", "elsewhere"),
        ("--role", "review"),
        ("--worktree", "new"),
    ] {
        let refused = run_card_as(&env, "#1", &[set], &["--dispatch", &dispatch]);
        assert_eq!(
            refused.status.code(),
            Some(6),
            "{set:?}: {}",
            stderr(&refused)
        );
    }
    assert!(agents_of(&env).is_empty());
    env.json(&["todo", "edit", "#1", "--project", "web", "--json"]);
    let run = receipt(run_card(&env, "#1", &["--dispatch", &dispatch]));
    assert_eq!(run["state"], "started");
    let agents = agents_of(&env);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["id"], run["session"]["id"]);
}

#[test]
fn run_relaunches_after_failed_launch() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    env.board_local();
    let late = env.tmp.path().join("late-agent");
    let env = env.start(&[("NEBULA_AGENT_CMD", late.to_str().unwrap())]);
    env.make_repo();
    add(&env, &["--project", "repo"], "Late");
    let dispatch = TodoId::generate().0;
    let failed = run_card(&env, "#1", &["--dispatch", &dispatch]);
    assert!(!failed.status.success());
    assert!(
        stderr(&failed).contains(&format!("rerun with --dispatch {dispatch}")),
        "{}",
        stderr(&failed)
    );
    std::fs::write(&late, "#!/bin/sh\nexec /bin/sh\n").unwrap();
    std::fs::set_permissions(&late, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = receipt(run_card(&env, "#1", &["--dispatch", &dispatch]));
    assert_eq!(run["state"], "started");
    let agents = agents_of(&env);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["id"], run["session"]["id"]);
    eventually("the relaunched worker alive on the card", || {
        chain(&env, "#1")
            .iter()
            .any(|s| s["id"] == run["session"]["id"] && s["alive"] == true)
            .then_some(())
    });
}

#[test]
fn run_rerun_finds_agent_after_branch_rename() {
    let env = board_env();
    let repo = env.make_repo();
    let card = add(&env, &["--project", "repo"], "Renamed");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &card, &dispatch, "main");
    let spawned = lost_spawn(&env, "#1", &dispatch);
    git(&repo, &["branch", "-m", "main", "renamed"]);
    let rerun = receipt(run_card(&env, "#1", &["--dispatch", &dispatch]));
    assert_eq!(rerun["state"], "started");
    assert_eq!(rerun["session"]["id"], spawned["id"]);
    assert_eq!(agents_of(&env).len(), 1);
}

#[test]
fn run_pending_rechecks_card() {
    let env = board_env();
    env.make_repo();
    let shelved = add(&env, &["--project", "repo"], "Shelved");
    add(&env, &["--project", "repo"], "Blocker");
    let blocked = add(&env, &["--project", "repo"], "Blocked");
    let (first, second) = (TodoId::generate().0, TodoId::generate().0);
    begin_run(&env, &shelved, &first, "main");
    begin_run(&env, &blocked, &second, "main");
    env.json(&["todo", "archive", "#1", "--json"]);
    env.json(&["todo", "block", "#3", "--by", "#2", "--json"]);
    for (card, dispatch) in [("#1", &first), ("#3", &second)] {
        let refused = run_card(&env, card, &["--dispatch", dispatch]);
        assert_eq!(
            refused.status.code(),
            Some(6),
            "{card}: {}",
            stderr(&refused)
        );
    }
    assert!(agents_of(&env).is_empty());
}

#[test]
fn run_dispatch_on_another_card_conflict() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Mine");
    let theirs = add(&env, &["--project", "repo"], "Theirs");
    let dispatch = TodoId::generate().0;
    begin_run(&env, &theirs, &dispatch, "main");
    lost_spawn(&env, "#2", &dispatch);
    let refused = run_card(&env, "#1", &["--dispatch", &dispatch]);
    assert_eq!(refused.status.code(), Some(6), "{}", stderr(&refused));
    assert_eq!(agents_of(&env).len(), 1);
    assert!(chain(&env, "#1").is_empty());
    assert_eq!(stage_of(&env, "#1"), "backlog");
}

#[test]
fn run_project_missing_on_target_fails() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "web"], "Nowhere");
    let dispatch = TodoId::generate().0;
    let refused = run_card(&env, "#1", &["--dispatch", &dispatch]);
    assert_eq!(refused.status.code(), Some(6), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("web"), "{}", stderr(&refused));
    let failed: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(failed["state"], "failed");
    let rerun = run_card(&env, "#1", &["--dispatch", &dispatch]);
    assert_eq!(rerun.status.code(), Some(6), "{}", stderr(&rerun));
    assert!(agents_of(&env).is_empty());
}

#[test]
fn run_worktree_new() {
    let env = board_env();
    env.make_repo();
    add(&env, &["--project", "repo"], "Fresh");
    let twin = || format!("{}ABCDEF", &TodoId::generate().0[..20]);
    let branch_of = |run: &Value| {
        agents_of(&env)
            .into_iter()
            .find(|a| a["id"] == run["session"]["id"])
            .unwrap()["worktree"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (first, second) = (twin(), twin());
    let a = receipt(run_card_as(
        &env,
        "#1",
        &[("--worktree", "new")],
        &["--dispatch", &first],
    ));
    let b = receipt(run_card_as(
        &env,
        "#1",
        &[("--worktree", "new")],
        &["--dispatch", &second],
    ));
    assert_ne!(branch_of(&a), branch_of(&b));
    assert!(
        branch_of(&a).ends_with(&first.to_lowercase()),
        "{}",
        branch_of(&a)
    );
    let third = TodoId::generate().0;
    let argv = run_argv(
        &env,
        "#1",
        &[("--worktree", "new")],
        &["--dispatch", &third],
    );
    let children: Vec<Child> = (0..2).map(|_| env.spawn_cli(&argv)).collect();
    let receipts: Vec<Value> = children
        .into_iter()
        .map(|c| json_of(c.wait_with_output().unwrap(), &["todo", "run", "new"]))
        .collect();
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(receipts[0]["state"], "started");
    assert_eq!(agents_of(&env).len(), 3);
}

const SEEDED_TODO: &str = "01JD0000000000000000000T01";

/// Frozen schemas of released versions, so a migration test seeds the
/// schema users have, not the SQL under test.
fn seed_at(db: &Path, version: i64, rows: &str) {
    let schema = match version {
        9 => include_str!("fixtures/schema_v9.sql"),
        19 => include_str!("fixtures/schema_v19.sql"),
        28 => include_str!("fixtures/schema_v28.sql"),
        _ => panic!("no frozen schema for v{version}"),
    };
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(schema).unwrap();
    conn.pragma_update(None, "user_version", version).unwrap();
    conn.execute_batch(rows).unwrap();
}

fn seed_v28(db: &Path) {
    seed_at(db, 28, &format!(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO projects (id, workspace_id, name, repo_path, sort_order, created_at) VALUES ('p1', 'default', 'vela', '/tmp/vela', 0, 0);
         INSERT INTO worktrees (id, project_id, path, branch, is_main, created_at) VALUES ('w1', 'p1', '/tmp/vela-feature', 'feature', 0, 0);
         INSERT INTO notes (id, project_id, text, created_at) VALUES ('01JD0000000000000000000N01', 'p1', 'remember the laptop', 10);
         INSERT INTO todos (id, project_id, text, done, created_at) VALUES ('{SEEDED_TODO}', 'p1', 'ship it', 1, 20);
         INSERT INTO notes (id, todo_id, text, created_at) VALUES ('01JD0000000000000000000N02', '{SEEDED_TODO}', 'child', 30);
         INSERT INTO todos (id, project_id, text, done, created_at) VALUES ('01JD0000000000000000000T02', 'p1', 'plan the week', 0, 40);
         INSERT INTO todos (id, worktree_id, text, done, created_at) VALUES ('01JD0000000000000000000T03', 'w1', 'review it', 0, 50);
         INSERT INTO notes (id, worktree_id, text, created_at) VALUES ('01JD0000000000000000000N03', 'w1', 'worktree thought', 60);
         INSERT INTO links (id, worktree_id, url, created_at) VALUES ('01JD0000000000000000000L01', 'w1', 'https://vela.possu.dev', 1700000000000);
         INSERT INTO links (id, worktree_id, url, created_at) VALUES ('01JD0000000000000000000L02', 'w-gone', 'https://gone.example/pr/1', 1700000000001);"
    ));
}

#[test]
fn migration_v28_legacy_rows() {
    let env = Env::new();
    env.board_local();
    seed_v28(&env.data_dir().join("nebula.db"));
    let env = env.start(&[]);

    let feature = serde_json::json!({
        "host": nebula_core::host::hostname(),
        "branch": "feature",
        "path": "/tmp/vela-feature",
        "worktree": true,
    });
    let mut cards: Vec<(String, String, String, Value)> =
        env.json(&["todo", "list", "--all", "--include-home", "--json"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                assert_eq!(c["project"], "vela", "{c}");
                (
                    c["title"].as_str().unwrap().to_string(),
                    c["stage"].as_str().unwrap().to_string(),
                    c["id"].as_str().unwrap().to_string(),
                    c["checkout"].clone(),
                )
            })
            .collect();
    cards.sort_by(|a, b| a.0.cmp(&b.0));
    let shape: Vec<(&str, &str, &Value)> = cards
        .iter()
        .map(|(title, stage, _, checkout)| (title.as_str(), stage.as_str(), checkout))
        .collect();
    assert_eq!(
        shape,
        [
            ("plan the week", "backlog", &Value::Null),
            ("remember the laptop", "backlog", &Value::Null),
            ("review it", "backlog", &feature),
            ("ship it", "done", &Value::Null),
            ("worktree thought", "backlog", &feature),
        ]
    );
    assert_eq!(cards[0].2, "01JD0000000000000000000T02");
    assert_eq!(cards[2].2, "01JD0000000000000000000T03");
    assert_eq!(cards[3].2, SEEDED_TODO);

    let shown = env.json(&["todo", "show", SEEDED_TODO, "--json"]);
    let log: Vec<&str> = shown["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["text"].as_str().unwrap())
        .collect();
    assert_eq!(log, ["child"]);

    let artifacts = env.json(&["artifact", "list", "--all", "--include-home", "--json"]);
    let mut items = artifacts["items"].as_array().unwrap().clone();
    items.sort_by_key(|a| a["created_at"].as_i64());
    assert_eq!(items.len(), 2, "{artifacts}");
    let link = &items[0];
    assert_eq!(link["kind"], "link");
    assert_eq!(
        link["content"],
        serde_json::json!({"url": "https://vela.possu.dev"})
    );
    assert_eq!(link["created_at"], 1700000000000_i64);
    assert_eq!(link["project"], "vela");
    assert_eq!(link["checkout"], feature);
    let orphan = &items[1];
    assert_eq!(orphan["id"], "01JD0000000000000000000L02");
    assert_eq!(orphan["kind"], "link");
    assert_eq!(
        orphan["content"],
        serde_json::json!({"url": "https://gone.example/pr/1"})
    );
    assert_eq!(orphan["created_at"], 1700000000001_i64);
    assert_eq!(orphan["project"], Value::Null);
    assert_eq!(orphan["checkout"], Value::Null);
}

#[test]
fn bare_notes_is_refused_as_removed() {
    let env = Env::new().start(&[]);
    let dir = env.tmp.path().join("notes");
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &[
            "-c",
            "user.email=t@nebula.dev",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success());
    }

    let refused = env.cli(&["notes"], None);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("`nebula notes` was removed"),
        "{}",
        stderr(&refused)
    );
    let listed = env.cli(&["worktree", "list", "--project", "notes"], None);
    assert!(!listed.status.success(), "the refusal added a project");

    let added = env.cli(&["./notes"], None);
    assert!(added.status.success(), "{}", stderr(&added));
    let listed = env.cli(&["worktree", "list", "--project", "notes"], None);
    assert!(listed.status.success(), "{}", stderr(&listed));
}

fn migrated_cards(env: &Env) -> Vec<(String, String, Value, Value)> {
    let mut cards: Vec<(String, String, Value, Value)> =
        env.json(&["todo", "list", "--all", "--include-home", "--json"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["title"].as_str().unwrap().to_string(),
                    c["stage"].as_str().unwrap().to_string(),
                    c["project"].clone(),
                    c["checkout"].clone(),
                )
            })
            .collect();
    cards.sort_by(|a, b| a.0.cmp(&b.0));
    cards
}

fn main_checkout() -> Value {
    serde_json::json!({
        "host": nebula_core::host::hostname(),
        "branch": "main",
        "path": "/tmp/p",
        "worktree": false,
    })
}

#[test]
fn migration_v9_notes_reach_the_board() {
    let env = Env::new();
    env.board_local();
    seed_at(
        &env.data_dir().join("nebula.db"),
        9,
        "INSERT INTO projects (id, name, repo_path, sort_order, created_at) VALUES ('p1', 'p', '/tmp/p', 0, 0);
         INSERT INTO worktrees (id, project_id, path, branch, is_main, sort_order, created_at) VALUES ('w1', 'p1', '/tmp/p', 'main', 1, 0, 0);
         INSERT INTO todos (id, worktree_id, text, done, sort_order, created_at) VALUES ('t1', 'w1', 'old note', 1, 3, 0);",
    );
    let env = env.start(&[]);
    assert_eq!(
        migrated_cards(&env),
        [(
            "old note".to_string(),
            "backlog".to_string(),
            Value::from("p"),
            main_checkout()
        )]
    );
}

#[test]
fn migration_v19_notes_reach_the_board() {
    let env = Env::new();
    env.board_local();
    seed_at(
        &env.data_dir().join("nebula.db"),
        19,
        "INSERT INTO projects (id, name, repo_path, sort_order, created_at) VALUES ('p1', 'p', '/tmp/p', 0, 0);
         INSERT INTO worktrees (id, project_id, path, branch, is_main, sort_order, created_at) VALUES ('w1', 'p1', '/tmp/p', 'main', 1, 0, 0);
         INSERT INTO notes (id, project_id, text, done, sort_order, created_at) VALUES ('n1', 'p1', 'project note', 0, 0, 0);
         INSERT INTO notes (id, worktree_id, text, done, sort_order, created_at) VALUES ('n2', 'w1', 'worktree note', 1, 2, 0);",
    );
    let env = env.start(&[]);
    assert_eq!(
        migrated_cards(&env),
        [
            (
                "project note".to_string(),
                "backlog".to_string(),
                Value::from("p"),
                Value::Null
            ),
            (
                "worktree note".to_string(),
                "backlog".to_string(),
                Value::from("p"),
                main_checkout()
            ),
        ]
    );
}

fn decide(env: &Env, agent: Option<&str>, args: &[&str]) -> Value {
    let mut argv = vec!["decision", "add", "--json"];
    argv.extend_from_slice(args);
    json_of(env.cli(&argv, agent), &argv)
}

fn decision_titles(env: &Env, args: &[&str]) -> Vec<String> {
    let mut argv = vec!["decision", "list", "--json"];
    argv.extend_from_slice(args);
    let mut found = titles(&env.json(&argv)["items"]);
    found.sort();
    found
}

#[test]
fn decision_add_list_query() {
    let env = board_env();
    env.make_repo();
    let (card, agent) = carded_agent(&env, "main");
    let decided = decide(
        &env,
        Some(&agent),
        &[
            "Copy published files",
            "--why",
            "The checkout is disposable",
        ],
    );
    assert_eq!(decided["kind"], "decision");
    assert_eq!(decided["title"], "Copy published files");
    assert_eq!(
        decided["content"],
        serde_json::json!({"body": "The checkout is disposable"})
    );
    assert_eq!(decided["card"], card["id"]);
    assert_eq!(decided["project"], "repo");
    assert_eq!(decided["session"]["id"], agent.as_str());
    let id = decided["id"].as_str().unwrap();
    assert_eq!(env.json(&["decision", "show", id, "--json"]), decided);

    let other = decide(
        &env,
        None,
        &["Poll every three seconds", "--project", "web"],
    );
    assert_eq!(other["card"], Value::Null);
    assert_eq!(other["project"], "web");
    let link = publish(
        &env,
        None,
        &["https://example.com/repo", "--project", "repo"],
    );

    assert_eq!(
        decision_titles(&env, &["--all", "--query", "DISPOSABLE"]),
        ["Copy published files"]
    );
    assert_eq!(
        decision_titles(&env, &["--all", "--query", "three"]),
        ["Poll every three seconds"]
    );
    assert_eq!(
        decision_titles(&env, &["--project", "web"]),
        ["Poll every three seconds"]
    );
    assert_eq!(
        decision_titles(&env, &["--project", "repo"]),
        ["Copy published files"]
    );
    let not_one = env.cli(
        &["decision", "show", link["id"].as_str().unwrap(), "--json"],
        None,
    );
    assert_eq!(not_one.status.code(), Some(5), "{}", stderr(&not_one));
}

fn fake_workers_ai(env: &Env) -> PathBuf {
    let bin = env.tmp.path().join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let vector = vec!["0.03125"; 1024].join(",");
    let down = env.tmp.path().join("rag-down");
    let hang = env.tmp.path().join("rag-hang");
    let attempted = env.tmp.path().join("rag-attempted");
    let hung = env.tmp.path().join("rag-hung-pid");
    let script = format!(
        "#!/bin/sh\nbody=$(cat)\nif [ -e '{}' ]; then echo \"$body\" >> '{}.bodies'; echo $$ > '{}'; touch '{}'; exec sleep 30; fi\nif [ -e '{}' ]; then touch '{}'; printf '%s\\n400' '{{\"errors\":[{{\"message\":\"down\"}}]}}'; exit 0; fi\nprintf '%s\\n200' '{{\"result\":{{\"data\":[[{vector}]]}}}}'\n",
        hang.display(),
        hung.display(),
        hung.display(),
        attempted.display(),
        down.display(),
        attempted.display()
    );
    let curl = bin.join("curl");
    std::fs::write(&curl, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn rag_board(state: Option<&str>, envs: &[(&str, &str)]) -> Env {
    let env = Env::new();
    env.board_local();
    let bin = fake_workers_ai(&env);
    if let Some(state) = state {
        std::fs::write(env.tmp.path().join(state), "").unwrap();
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut all = vec![
        ("PATH", path.as_str()),
        ("CF_ACCOUNT_ID", "test"),
        ("CF_AI_TOKEN", "test"),
        ("NEBULA_RAG_COPY_MS", "100"),
    ];
    all.extend_from_slice(envs);
    env.start(&all)
}

fn rag_note(env: &Env, id: &str) -> i64 {
    poll_until(Duration::from_secs(10), || {
        env.json(&["decision", "show", id, "--json"])["meta"]["rag_note"].as_i64()
    })
    .unwrap_or_else(|| panic!("decision {id} never got its rag_note"))
}

fn rag_hit(env: &Env, text: &str, note: i64) -> Value {
    let search = Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["rag", "search", "--exact", "--ws", "/", "--json", text])
        .env("NEBULA_DATA_DIR", env.data_dir())
        .env("NEBULA_RAG_LOCAL", "1")
        .env_remove("NEBULA_RAG_ENV")
        .current_dir(env.tmp.path())
        .output()
        .unwrap();
    let hits = json_of(search, &["rag", "search", text]);
    hits.as_array()
        .unwrap()
        .iter()
        .find(|h| h["doc_id"].as_i64() == Some(note))
        .cloned()
        .unwrap_or_else(|| panic!("note {note} not among the hits for {text:?}: {hits}"))
}

fn pid_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn decision_reaches_rag() {
    let env = Env::new();
    env.board_local();
    let bin = fake_workers_ai(&env);
    let down = env.tmp.path().join("rag-down");
    std::fs::write(&down, "").unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let env = env.start(&[
        ("PATH", &path),
        ("CF_ACCOUNT_ID", "test"),
        ("CF_AI_TOKEN", "test"),
        ("NEBULA_RAG_COPY_MS", "100"),
    ]);

    let decided = decide(
        &env,
        None,
        &[
            "Copy published files",
            "--why",
            "The checkout is disposable",
            "--project",
            "repo",
        ],
    );
    let id = decided["id"].as_str().unwrap();
    let attempted = env.tmp.path().join("rag-attempted");
    poll_until(Duration::from_secs(10), || attempted.exists().then_some(()))
        .expect("the board never tried to copy the decision to RAG");
    let shown = env.json(&["decision", "show", id, "--json"]);
    assert_eq!(shown["meta"].get("rag_note"), None, "{shown}");

    std::fs::remove_file(&down).unwrap();
    let note = poll_until(Duration::from_secs(10), || {
        env.json(&["decision", "show", id, "--json"])["meta"]["rag_note"].as_i64()
    })
    .expect("the decision never got its rag_note once RAG answered");

    let search = Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args([
            "rag",
            "search",
            "--exact",
            "--ws",
            "/",
            "--json",
            "disposable",
        ])
        .env("NEBULA_DATA_DIR", env.data_dir())
        .env("NEBULA_RAG_LOCAL", "1")
        .env_remove("NEBULA_RAG_ENV")
        .current_dir(env.tmp.path())
        .output()
        .unwrap();
    let hits = json_of(search, &["rag", "search"]);
    let hit = hits
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["doc_id"].as_i64() == Some(note))
        .unwrap_or_else(|| panic!("note {note} not among {hits}"));
    assert_eq!(hit["ws"], "apps/repo/notes");
    assert!(
        hit["text"]
            .as_str()
            .unwrap()
            .contains("The checkout is disposable"),
        "{hit}"
    );
}

#[test]
fn away_digest_decisions() {
    let env = board_env();
    let before = decide(&env, None, &["Keep the counter", "--project", "repo"]);
    let since = before["created_at"].as_i64().unwrap();
    let after = decide(&env, None, &["Drop the cookie grant", "--project", "repo"]);
    let elsewhere = decide(&env, None, &["Ship slice two", "--project", "web"]);
    let since = since.to_string();
    let board = env.json(&[
        "todo",
        "board",
        "--project",
        "repo",
        "--away-since",
        &since,
        "--json",
    ]);
    assert_eq!(
        board["board"]["away"]["decisions"],
        serde_json::json!([after["id"]]),
        "{board} (outside the scope: {})",
        elsewhere["id"]
    );
}

#[test]
fn similar_decisions_keep_their_own_rag_notes() {
    let env = rag_board(None, &[]);
    let first = decide(
        &env,
        None,
        &[
            "Cache the index",
            "--why",
            "Cold starts are slow",
            "--project",
            "repo",
        ],
    );
    let first_note = rag_note(&env, first["id"].as_str().unwrap());
    let second = decide(
        &env,
        None,
        &[
            "Cache the index",
            "--why",
            "Disk reads dominate",
            "--project",
            "repo",
        ],
    );
    let second_note = rag_note(&env, second["id"].as_str().unwrap());
    assert_ne!(first_note, second_note);
    assert!(rag_hit(&env, "Cold starts are slow", first_note)["text"]
        .as_str()
        .unwrap()
        .contains("Cold starts are slow"));
    assert!(rag_hit(&env, "Disk reads dominate", second_note)["text"]
        .as_str()
        .unwrap()
        .contains("Disk reads dominate"));
}

#[test]
fn decision_on_a_card_takes_its_project() {
    let env = board_env();
    env.make_repo();
    let card = add(&env, &["--project", "repo"], "Publish");
    let home = add(&env, &["--home"], "Errands");
    let on_card = decide(&env, None, &["Copy published files", "--card", "#1"]);
    assert_eq!(on_card["card"], card["id"]);
    assert_eq!(on_card["project"], "repo");
    let on_home = decide(&env, None, &["Batch the errands", "--card", "#2"]);
    assert_eq!(on_home["card"], home["id"]);
    assert_eq!(on_home["project"], Value::Null);
    assert_eq!(
        decision_titles(&env, &["--project", "repo"]),
        ["Copy published files"]
    );
}

#[test]
fn decision_add_home_ignores_the_callers_checkout() {
    let env = board_env();
    env.make_repo();
    let (_, agent) = carded_agent(&env, "main");
    let decided = decide(
        &env,
        Some(&agent),
        &["--why=Nobody owns it yet", "--home", "Keep a shared inbox"],
    );
    assert_eq!(decided["card"], Value::Null);
    assert_eq!(decided["project"], Value::Null);
    assert_eq!(decided["title"], "Keep a shared inbox");
}

#[test]
fn decision_in_a_project_rag_cannot_name_still_reaches_rag() {
    let env = rag_board(None, &[]);
    let spaced = decide(
        &env,
        None,
        &[
            "Bill monthly",
            "--why",
            "Spaced project",
            "--project",
            "Client Portal",
        ],
    );
    let plain = decide(
        &env,
        None,
        &[
            "Bill yearly",
            "--why",
            "Plain project",
            "--project",
            "client-portal",
        ],
    );
    let spaced_ws = rag_hit(
        &env,
        "Spaced project",
        rag_note(&env, spaced["id"].as_str().unwrap()),
    )["ws"]
        .as_str()
        .unwrap()
        .to_string();
    let plain_ws = rag_hit(
        &env,
        "Plain project",
        rag_note(&env, plain["id"].as_str().unwrap()),
    )["ws"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(plain_ws, "apps/client-portal/notes");
    assert!(
        spaced_ws.starts_with("apps/client-portal-") && spaced_ws.ends_with("/notes"),
        "{spaced_ws}"
    );
}

#[test]
fn hung_rag_copy_is_killed_at_its_deadline() {
    let env = rag_board(Some("rag-hang"), &[("NEBULA_RAG_TIMEOUT_MS", "500")]);
    let decided = decide(
        &env,
        None,
        &[
            "Copy published files",
            "--why",
            "The checkout is disposable",
            "--project",
            "repo",
        ],
    );
    let hung = env.tmp.path().join("rag-hung-pid");
    let pid = poll_until(Duration::from_secs(10), || {
        std::fs::read_to_string(&hung).ok()
    })
    .expect("the board never tried to copy the decision to RAG");
    std::fs::remove_file(env.tmp.path().join("rag-hang")).unwrap();
    rag_note(&env, decided["id"].as_str().unwrap());
    assert!(
        !pid_alive(pid.trim()),
        "the hung rag child {pid} outlived its deadline"
    );
}

#[test]
fn an_unanswering_rag_ends_the_copy_tick_at_the_first_decision() {
    let env = rag_board(Some("rag-hang"), &[("NEBULA_RAG_TIMEOUT_MS", "300")]);
    decide(&env, None, &["First decision", "--project", "repo"]);
    decide(&env, None, &["Second decision", "--project", "repo"]);
    let bodies = env.tmp.path().join("rag-hung-pid.bodies");
    let tried = poll_until(Duration::from_secs(10), || {
        std::fs::read_to_string(&bodies)
            .ok()
            .filter(|tried| tried.matches("First decision").count() >= 8)
    })
    .expect("the board stopped retrying the first decision");
    assert!(!tried.contains("Second decision"), "{tried}");
}

#[test]
fn reading_a_pending_decision_does_not_retry_its_copy() {
    let env = rag_board(
        Some("rag-hang"),
        &[
            ("NEBULA_RAG_TIMEOUT_MS", "300"),
            ("NEBULA_RAG_COPY_MS", "600000"),
        ],
    );
    let decided = decide(&env, None, &["Read me", "--project", "repo"]);
    let id = decided["id"].as_str().unwrap();
    let bodies = env.tmp.path().join("rag-hung-pid.bodies");
    let attempts = || {
        std::fs::read_to_string(&bodies)
            .unwrap_or_default()
            .matches("Read me")
            .count()
            / 2
    };
    poll_until(Duration::from_secs(10), || (attempts() == 1).then_some(()))
        .expect("publishing never copied the decision");
    poll_until(Duration::from_secs(1), || None::<()>);
    for _ in 0..3 {
        env.json(&["decision", "show", id, "--json"]);
    }
    let retried = poll_until(Duration::from_secs(2), || (attempts() > 1).then_some(()));
    assert_eq!(retried, None, "a read woke the RAG copier");
}

#[test]
fn kill_stops_a_hung_rag_copy() {
    let mut env = rag_board(Some("rag-hang"), &[]);
    decide(&env, None, &["Copy published files", "--project", "repo"]);
    let hung = env.tmp.path().join("rag-hung-pid");
    let pid = poll_until(Duration::from_secs(10), || {
        std::fs::read_to_string(&hung).ok()
    })
    .expect("the board never tried to copy the decision to RAG");
    let killed = env.cli(&["kill"], None);
    assert!(killed.status.success(), "{}", stderr(&killed));
    let mut daemon = env.daemon.take().unwrap();
    let exited = poll_until(Duration::from_secs(5), || daemon.try_wait().unwrap());
    if exited.is_none() {
        let _ = daemon.kill();
        let _ = daemon.wait();
        panic!("the daemon outlived nebula kill while a rag copy hung");
    }
    assert!(
        poll_until(Duration::from_secs(5), || (!pid_alive(pid.trim()))
            .then_some(()))
        .is_some(),
        "the hung rag child {pid} outlived the daemon"
    );
}
