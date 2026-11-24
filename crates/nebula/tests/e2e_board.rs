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
    root.to_path_buf()
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
