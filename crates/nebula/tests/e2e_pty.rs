//! End-to-end daemon tests over the real IPC surface: entity CRUD, PTY
//! attach/detach with scrollback replay, git worktree ops, and persistence
//! across a daemon restart.

use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{
    AgentKind, ClientRequest, Entity, EntityId, ServerEvent, SessionRef, WriterClaim,
    PROTOCOL_VERSION,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

struct TestEnv {
    tmp: tempfile::TempDir,
    runtime_dir: PathBuf,
}

/// Session identity a test run inherits when started from inside a live
/// nebula session; a test daemon must not carry it.
const LIVE_SESSION_ENV: [&str; 3] = ["NEBULA_AGENT_ID", "NEBULA_API_URL", "NEBULA_API_TOKEN"];

/// A test daemon that is killed when dropped, so a test that panics before
/// its Shutdown cannot leave the daemon (and the fds it inherited) behind.
struct DaemonChild(std::process::Child);

impl std::ops::Deref for DaemonChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for DaemonChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl TestEnv {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let runtime_dir = tmp.path().join("rt");
        Self { tmp, runtime_dir }
    }

    fn sock(&self) -> PathBuf {
        self.runtime_dir.join("daemon.sock")
    }

    fn spawn_daemon(&self) -> DaemonChild {
        self.spawn_daemon_with_agent_cmd("/bin/sh") // no real claude in tests
    }

    fn spawn_daemon_with_agent_cmd(&self, agent_cmd: &str) -> DaemonChild {
        self.spawn_daemon_with(agent_cmd, &[])
    }

    fn spawn_daemon_with(&self, agent_cmd: &str, envs: &[(&str, &str)]) -> DaemonChild {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"));
        cmd.args(["daemon", "--foreground"])
            .env("NEBULA_RUNTIME_DIR", &self.runtime_dir)
            .env("NEBULA_DATA_DIR", self.tmp.path().join("data"))
            .env("SHELL", "/bin/sh")
            .env("NEBULA_AGENT_CMD", agent_cmd)
            .env("NEBULA_WORKTREE_SYNC_MS", "100") // fast external-worktree pickup
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for k in LIVE_SESSION_ENV {
            cmd.env_remove(k);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        DaemonChild(cmd.spawn().unwrap())
    }

    /// Daemon with no `NEBULA_AGENT_CMD` override, so agent spawns take the
    /// real login-shell path, and `$SHELL` set to `shell`. Lets a test decide
    /// what the daemon can find on PATH.
    fn spawn_daemon_with_shell(&self, shell: &Path) -> DaemonChild {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"));
        cmd.args(["daemon", "--foreground"])
            .env("NEBULA_RUNTIME_DIR", &self.runtime_dir)
            .env("NEBULA_DATA_DIR", self.tmp.path().join("data"))
            .env("SHELL", shell)
            .env("CLAUDE_CONFIG_DIR", self.tmp.path().join("claude"))
            .env("CODEX_HOME", self.tmp.path().join("codex"))
            .env_remove("NEBULA_AGENT_CMD")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for k in LIVE_SESSION_ENV {
            cmd.env_remove(k);
        }
        DaemonChild(cmd.spawn().unwrap())
    }

    /// A `$SHELL` that answers `-l -i -c` but sees no agent CLI on PATH.
    fn blind_shell(&self) -> PathBuf {
        let path = self.tmp.path().join("blind-shell.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\nPATH=/usr/bin:/bin\nexport PATH\nexec /bin/sh -c \"$4\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Write the daemon's `config.json` (read from `NEBULA_DATA_DIR`)
    /// before boot.
    fn write_config(&self, json: &str) {
        let data = self.tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("config.json"), json).unwrap();
    }

    /// A committed git repo to act as the project.
    fn make_repo(&self) -> PathBuf {
        let repo = self.tmp.path().join("repo");
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
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "test@nebula.dev"]);
        git(&["config", "user.name", "nebula-test"]);
        std::fs::write(repo.join("README.md"), "# test\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "init"]);
        repo
    }
}

fn commit_file(repo: &Path, path: &str, contents: &str) {
    std::fs::write(repo.join(path), contents).unwrap();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(git(&["add", path]), "git add {path} failed");
    assert!(
        git(&["commit", "-m", "add test fixture"]),
        "git commit failed"
    );
}

async fn connect(sock: &Path) -> UnixStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(sock).await {
            Ok(s) => return s,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(e) => panic!("daemon socket never appeared: {e}"),
        }
    }
}

async fn handshake(stream: &mut UnixStream) {
    write_frame(
        stream,
        &ClientRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .unwrap();
    match read_frame::<ServerEvent, _>(stream).await.unwrap() {
        Some(ServerEvent::HelloOk { .. }) => {}
        other => panic!("bad handshake reply: {other:?}"),
    }
}

/// Collect events until `pred` says done (returns all seen events).
async fn read_events_until(
    stream: &mut UnixStream,
    timeout: Duration,
    mut pred: impl FnMut(&[ServerEvent]) -> bool,
) -> Vec<ServerEvent> {
    let mut seen = Vec::new();
    let ok = tokio::time::timeout(timeout, async {
        loop {
            match read_frame::<ServerEvent, _>(stream).await.unwrap() {
                Some(ev) => {
                    seen.push(ev);
                    if pred(&seen) {
                        return;
                    }
                }
                None => panic!("daemon closed connection early"),
            }
        }
    })
    .await;
    assert!(ok.is_ok(), "timed out waiting for events; saw: {seen:#?}");
    seen
}

fn find_ack(events: &[ServerEvent], want_req: u64) -> Option<&ServerEvent> {
    events.iter().find(|e| {
        matches!(e, ServerEvent::Ack { req_id, .. } if *req_id == want_req)
            || matches!(e, ServerEvent::Error { req_id: Some(r), .. } if *r == want_req)
    })
}

fn collected_output(events: &[ServerEvent]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in events {
        match e {
            ServerEvent::Scrollback { data, .. } | ServerEvent::Output { data, .. } => {
                out.extend_from_slice(data)
            }
            _ => {}
        }
    }
    out
}

#[tokio::test]
async fn full_crud_attach_and_restart_persistence() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    match &events[0] {
        ServerEvent::Snapshot { projects, .. } => assert!(projects.is_empty()),
        other => panic!("expected snapshot first, got {other:?}"),
    }

    // ---- AddProject: creates project + main worktree row ----
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 1).is_some()
            && evs.iter().any(|e| {
                matches!(
                    e,
                    ServerEvent::EntityUpserted {
                        entity: Entity::Worktree(_)
                    }
                )
            })
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Project(project_id)),
        ..
    } = find_ack(&events, 1).unwrap()
    else {
        panic!("AddProject failed: {events:#?}");
    };
    let project_id = project_id.clone();
    let main_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.is_main => Some(w.clone()),
            _ => None,
        })
        .expect("main worktree upsert");
    assert_eq!(main_worktree.branch, "main");

    // ---- CreateTerminal + attach + echo through the PTY ----
    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 2,
            worktree: main_worktree.id.clone(),
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    let term_id = term_id.clone();

    let sref = SessionRef::Terminal(term_id);
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    let marker = "nebula_e2e_marker_4519";
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: format!("echo {marker}; pwd\n").into_bytes(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        let text = String::from_utf8_lossy(&collected_output(evs)).into_owned();
        text.matches(marker).count() >= 2
    })
    .await;
    // The shell runs in the worktree directory.
    let text = String::from_utf8_lossy(&collected_output(&events)).into_owned();
    assert!(
        text.contains("repo"),
        "terminal cwd should be the worktree: {text}"
    );

    // ---- CreateAgent (NEBULA_AGENT_CMD=/bin/sh stands in for claude) ----
    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 3,
            worktree: main_worktree.id.clone(),
            name: "agent-1".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 3).is_some()
    })
    .await;
    assert!(
        matches!(
            find_ack(&events, 3),
            Some(ServerEvent::Ack {
                created: Some(EntityId::Agent(_)),
                ..
            })
        ),
        "CreateAgent failed: {events:#?}"
    );

    // ---- CreateWorktree: real `git worktree add` on disk ----
    write_frame(
        &mut c,
        &ClientRequest::CreateWorktree {
            req_id: 4,
            project: project_id.clone(),
            branch: "feature-x".into(),
            base: Some("main".into()),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        find_ack(evs, 4).is_some()
            && evs.iter().any(|e| {
                matches!(e, ServerEvent::EntityUpserted { entity: Entity::Worktree(w) } if w.branch == "feature-x")
            })
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Worktree(feature_wt_id)),
        ..
    } = find_ack(&events, 4).unwrap()
    else {
        panic!("CreateWorktree failed: {events:#?}");
    };
    let feature_wt_id = feature_wt_id.clone();
    let feature_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.id == feature_wt_id => Some(w.clone()),
            _ => None,
        })
        .expect("worktree upsert carries its data");
    assert_eq!(feature_worktree.created_from.as_deref(), Some("main"));
    let feature_wt_path = feature_worktree.path;
    assert!(feature_wt_path.exists(), "worktree dir created on disk");

    // ---- DeleteWorktree removes it from disk ----
    write_frame(
        &mut c,
        &ClientRequest::DeleteWorktree {
            req_id: 5,
            id: feature_wt_id.clone(),
            force: true,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        find_ack(evs, 5).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, 5), Some(ServerEvent::Ack { .. })),
        "DeleteWorktree failed: {events:#?}"
    );
    assert!(!feature_wt_path.exists(), "worktree dir removed from disk");

    // ---- restart: tree persists, boot sweep marks nothing (agents fresh) ----
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);

    let mut daemon2 = env.spawn_daemon();
    let mut c2 = connect(&env.sock()).await;
    handshake(&mut c2).await;
    write_frame(&mut c2, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c2, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let ServerEvent::Snapshot {
        projects,
        worktrees,
        agents,
        terminals,
        ..
    } = &events[0]
    else {
        panic!("expected snapshot");
    };
    assert_eq!(projects.len(), 1, "project persisted");
    assert_eq!(worktrees.len(), 1, "only main worktree remains");
    assert_eq!(agents.len(), 1, "agent persisted");
    assert_eq!(agents[0].name, "agent-1");
    assert_eq!(terminals.len(), 1, "terminal persisted");
    assert!(!agents[0].alive, "no PTY after restart until reattach");

    // Reattach the persisted terminal: lazy respawn, cwd still the worktree.
    let sref2 = SessionRef::Terminal(terminals[0].id.clone());
    write_frame(
        &mut c2,
        &ClientRequest::Attach {
            session: sref2.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    let marker2 = "nebula_e2e_after_restart_8846";
    write_frame(
        &mut c2,
        &ClientRequest::Input {
            session: sref2,
            data: format!("echo {marker2}\n").into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c2, Duration::from_secs(5), |evs| {
        String::from_utf8_lossy(&collected_output(evs))
            .matches(marker2)
            .count()
            >= 2
    })
    .await;

    write_frame(&mut c2, &ClientRequest::Shutdown)
        .await
        .unwrap();
    wait_for_exit(&mut daemon2);
}

/// The daemon must answer a child's kitty-keyboard support query (nothing
/// else ever would — the child talks to a virtual terminal), track pushed
/// flags, and tell attached clients so they switch key encodings. This is
/// what makes Cmd/Option combos reach Claude Code.
#[tokio::test]
async fn kitty_keyboard_negotiation_passthrough() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 1).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Project(_)),
        ..
    } = find_ack(&events, 1).unwrap()
    else {
        panic!("AddProject failed: {events:#?}");
    };
    // AddProject's worktree upsert goes to subscribers only; fetch it via the DB
    // snapshot path instead: create the terminal against the main worktree id
    // that Subscribe would report. Simplest: subscribe now.
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let worktree_id = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::Snapshot { worktrees, .. } => worktrees.first().map(|w| w.id.clone()),
            _ => None,
        })
        .expect("main worktree in snapshot");

    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 2,
            worktree: worktree_id,
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    let sref = SessionRef::Terminal(term_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 100,
            rows: 30,
        },
    )
    .await
    .unwrap();
    // Attach reports the child's current (legacy) flags right away.
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::KittyFlags { flags: 0, .. }))
    })
    .await;

    // The child queries support and reads the daemon's reply off its own
    // stdin — the same detection recipe Claude Code uses. `tr` makes the
    // reply greppable in plain text.
    let probe = "stty -icanon -echo min 0 time 20; printf '\\033[?u'; sleep 1; \
                 printf 'REPLY:'; dd bs=64 count=1 2>/dev/null | tr '\\033' 'E'; echo; stty sane\n";
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: probe.into(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        String::from_utf8_lossy(&collected_output(evs)).contains("REPLY:E[?0u")
    })
    .await;

    // Pushing flags reaches the attached client…
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: b"printf '\\033[>1u'\n".to_vec(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::KittyFlags { flags: 1, .. }))
    })
    .await;

    // …survives a re-attach (fresh client learns the current mode)…
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 100,
            rows: 30,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::KittyFlags { .. }))
    })
    .await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::KittyFlags { flags: 1, .. })),
        "re-attach must report the pushed flags: {events:#?}"
    );

    // …and popping restores legacy.
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: b"printf '\\033[<u'\n".to_vec(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::KittyFlags { flags: 0, .. }))
    })
    .await;

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// True end-to-end status detection: the agent PTY (a /bin/sh stand-in for
/// claude) uses its *injected* NEBULA_* env to curl the daemon's hook
/// endpoint, exactly like the installed claude hooks would — and the
/// subscribed client sees StatusChanged.
#[tokio::test]
async fn hook_post_from_agent_pty_drives_status() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityUpserted {
                    entity: Entity::Worktree(_)
                }
            )
        })
    })
    .await;
    let worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } => Some(w.clone()),
            _ => None,
        })
        .unwrap();

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: "hooked".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();

    // Hook install happened at spawn: managed hooks exist in the worktree.
    let settings_path = repo.join(".claude/settings.local.json");
    assert!(settings_path.exists(), "hooks installed into worktree");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(settings["hooks"]["Stop"][0]["_nebulaManaged"]
        .as_bool()
        .unwrap());

    // Drive the shell inside the agent PTY to POST hooks with its own env.
    let sref = SessionRef::Agent(agent_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();
    let curl = |event: &str, body: &str| {
        format!(
            "curl -sS -m 3 -X POST -H \"Authorization: Bearer $NEBULA_API_TOKEN\" \
             -H 'Content-Type: application/json' -d '{body}' \
             \"$NEBULA_API_URL/api/hooks/claude?agentId=$NEBULA_AGENT_ID&hookEvent={event}\"\n"
        )
    };

    // UserPromptSubmit → running
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl("UserPromptSubmit", r#"{"session_id":"sess-1"}"#).into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Running, .. }
                if *agent == agent_id)
        })
    })
    .await;

    // Notification(permission_prompt) → needs_feedback
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl(
                "Notification",
                r#"{"session_id":"sess-1","notification_type":"permission_prompt"}"#,
            )
            .into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::NeedsFeedback, .. }
                if *agent == agent_id)
        })
    })
    .await;

    // A foreign session's Stop is ignored…
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl("Stop", r#"{"session_id":"someone-elses-claude"}"#).into_bytes(),
        },
    )
    .await
    .unwrap();
    // …while the owning session's Stop finishes the agent.
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl("Stop", r#"{"session_id":"sess-1"}"#).into_bytes(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Finished, .. }
                if *agent == agent_id)
        })
    })
    .await;
    // The foreign Stop must not have produced its own StatusChanged→Finished
    // before the NeedsFeedback→Finished one (i.e. exactly one Finished).
    let finished_count = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                ServerEvent::StatusChanged {
                    status: nebula_core::AgentStatus::Finished,
                    ..
                }
            )
        })
        .count();
    assert_eq!(finished_count, 1, "foreign-session Stop must be ignored");

    // Session id was captured for --resume.
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    let mut daemon2 = env.spawn_daemon();
    let mut c2 = connect(&env.sock()).await;
    handshake(&mut c2).await;
    write_frame(&mut c2, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c2, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let ServerEvent::Snapshot { agents, .. } = &events[0] else {
        panic!()
    };
    assert_eq!(
        agents[0].session_id.as_deref(),
        Some("sess-1"),
        "session id persisted"
    );
    write_frame(&mut c2, &ClientRequest::Shutdown)
        .await
        .unwrap();
    wait_for_exit(&mut daemon2);
}

/// cwd-based re-homing end to end: an agent created in the main checkout
/// posts a hook whose payload reports a cwd inside another worktree of the
/// same project (claude entered a worktree it created mid-conversation) —
/// the daemon re-homes the agent row there and broadcasts the upsert.
/// The cancel path. Claude Code fires NO hook when the user interrupts a
/// turn — `Stop` is documented not to run on a user interrupt, and the
/// `idle_prompt` notification that normally rescues a hookless turn end is
/// suppressed precisely because the user just pressed a key. What it does
/// still do is clear its OSC 9;4 progress bar, so nebula reads busy/idle
/// straight off the PTY. This drives the whole path — raw bytes out of the
/// child, through the pump's scanner, into the status machine — with no HTTP
/// hook involved at all.
#[tokio::test]
async fn pty_progress_sequence_drives_status_without_any_hook() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;
    let agent_id = create_agent_get_id(&mut c, &worktree.id, "cancelled", 2).await;

    let sref = SessionRef::Agent(agent_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();

    // Have the shell in the agent PTY emit exactly what Claude Code emits.
    // The command text is echoed back by the tty, which is the point: only
    // the real escape bytes may move the status, never a mention of them.
    // `\ddd` octal, not `\e` — POSIX printf, so this works under dash too.
    let emit = |state: &str| format!("printf '\\033]9;4;{state};\\007'\n").into_bytes();

    // Turn starts: progress goes indeterminate → running.
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: emit("3"),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Running, .. }
                if *agent == agent_id)
        })
    })
    .await;

    // User hits escape: the progress bar clears, and nothing else happens.
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: emit("0"),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Finished, .. }
                if *agent == agent_id)
        })
    })
    .await;

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// A shell terminal tab running an agent CLI by hand: the same OSC 9;4
/// progress bytes flip the TerminalTab's `busy` bit through terminal
/// upserts, so the TUI can light the tab like an agent's status dot.
#[tokio::test]
async fn pty_progress_marks_a_shell_terminal_busy() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    let term_id = term_id.clone();

    let sref = SessionRef::Terminal(term_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();

    let emit = |state: &str| format!("printf '\\033]9;4;{state};\\007'\n").into_bytes();
    let busy_upsert = |evs: &[ServerEvent], want: bool| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: nebula_core::Entity::Terminal(t) }
                if t.id == term_id && t.busy == want)
        })
    };

    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: emit("3"),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        busy_upsert(evs, true)
    })
    .await;

    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: emit("0"),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        busy_upsert(evs, false)
    })
    .await;

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// `nebula agent wait <name>` — the delegating session's blocking verb: it must
/// error out (nonzero) while the worker is still running past --timeout,
/// and unblock with the settled row as JSON the moment the worker leaves
/// running. Driven end-to-end through the real CLI binary against an
/// isolated daemon, status moved by the same OSC 9;4 progress bytes Claude
/// emits.
#[tokio::test]
async fn agent_wait_cli_blocks_until_worker_settles() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;
    let agent_id = create_agent_get_id(&mut c, &worktree.id, "worker", 2).await;

    let sref = SessionRef::Agent(agent_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();
    let emit = |state: &str| format!("printf '\\033]9;4;{state};\\007'\n").into_bytes();

    // Worker starts its turn: progress indeterminate → running.
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: emit("3"),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Running, .. }
                if *agent == agent_id)
        })
    })
    .await;

    let wait_cmd = |timeout: &str| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"));
        cmd.args([
            "agent",
            "wait",
            "worker",
            "--project",
            "repo",
            "--timeout",
            timeout,
        ])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
        cmd
    };

    // Still running past the deadline: nonzero exit, a clear message.
    let timed_out = wait_cmd("1").output().unwrap();
    assert!(
        !timed_out.status.success(),
        "wait must fail while the worker is running: {timed_out:?}"
    );
    let stderr = String::from_utf8_lossy(&timed_out.stderr);
    assert!(
        stderr.contains("timed out") && stderr.contains("worker"),
        "timeout message should name the still-running worker: {stderr}"
    );

    // Now block for real, and settle the worker while the CLI is waiting.
    let mut waiting = wait_cmd("30").spawn().unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: emit("0"),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Finished, .. }
                if *agent == agent_id)
        })
    })
    .await;

    let out = tokio::task::spawn_blocking(move || {
        let status = waiting.wait().unwrap();
        let mut stdout = Vec::new();
        use std::io::Read;
        waiting
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut stdout)
            .unwrap();
        (status, stdout)
    })
    .await
    .unwrap();
    assert!(
        out.0.success(),
        "wait should exit 0 once the worker settles"
    );
    let rows: serde_json::Value = serde_json::from_slice(&out.1).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "worker");
    assert_eq!(rows[0]["status"], "finished");
    assert_eq!(rows[0]["project"], "repo");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// `nebula agent wait --any` hands back the first worker to settle while the
/// others keep running; a plain multi-name wait still holds out for all.
#[tokio::test]
async fn agent_wait_any_returns_first_settled_worker() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;
    let w1 = create_agent_get_id(&mut c, &worktree.id, "w1", 2).await;
    let w2 = create_agent_get_id(&mut c, &worktree.id, "w2", 3).await;
    for id in [&w1, &w2] {
        attach_agent(&mut c, id).await;
        emit_progress(&mut c, id, "3", nebula_core::AgentStatus::Running).await;
    }

    let waiting = agent_cli(
        &env,
        &[
            "wait",
            "w1",
            "w2",
            "--any",
            "--timeout",
            "30",
            "--project",
            "repo",
        ],
        None,
    )
    .spawn()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    emit_progress(&mut c, &w1, "0", nebula_core::AgentStatus::Finished).await;

    let out = tokio::task::spawn_blocking(move || waiting.wait_with_output().unwrap())
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "--any should exit 0 on the first settle: {out:?}"
    );
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "only the settled worker is printed: {rows:?}"
    );
    assert_eq!(rows[0]["name"], "w1");
    assert_eq!(rows[0]["status"], "finished");

    let all = agent_cli(
        &env,
        &["wait", "w1", "w2", "--timeout", "1", "--project", "repo"],
        None,
    )
    .output()
    .unwrap();
    assert!(
        !all.status.success(),
        "a wait without --any needs every worker settled: {all:?}"
    );
    let stderr = String::from_utf8_lossy(&all.stderr);
    assert!(
        stderr.contains("timed out") && stderr.contains("w2"),
        "timeout should name the worker still running: {stderr}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// A session blocked in `nebula agent wait` comes back within seconds, with
/// exit code 3, when `nebula agent send` queues a message for it — instead
/// of sitting on its lead's message until the timeout.
#[tokio::test]
async fn agent_wait_returns_early_when_the_caller_gets_a_message() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;
    let lead = create_agent_get_id(&mut c, &worktree.id, "lead", 2).await;
    let worker = create_agent_get_id(&mut c, &worktree.id, "worker", 3).await;
    attach_agent(&mut c, &lead).await;
    attach_agent(&mut c, &worker).await;
    emit_progress(&mut c, &worker, "3", nebula_core::AgentStatus::Running).await;

    let waiting = agent_cli(
        &env,
        &["wait", "worker", "--timeout", "30", "--project", "repo"],
        Some(&lead),
    )
    .spawn()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let sent_at = std::time::Instant::now();
    let send = agent_cli(
        &env,
        &["send", "lead", "status?", "--project", "repo"],
        None,
    )
    .output()
    .unwrap();
    assert!(send.status.success(), "send failed: {send:?}");

    let out = tokio::task::spawn_blocking(move || waiting.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a queued message ends the wait with 3: {out:?}"
    );
    assert!(
        sent_at.elapsed() < Duration::from_secs(5),
        "the wait should notice the message within seconds, took {:?}",
        sent_at.elapsed()
    );
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows, serde_json::json!([]), "the worker is still running");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// A message that lands while `agent wait` is still connecting counts: the
/// wait's daemon connection is held at a gate (before handshake and
/// snapshot), the message is sent, and the released wait must exit 3.
#[tokio::test]
async fn agent_wait_counts_a_message_sent_while_it_starts() {
    let env = TestEnv::new();
    let LeadAndWorker {
        mut daemon,
        mut c,
        lead,
        ..
    } = lead_waiting_on_running_worker(&env).await;
    let (gate, real) = install_socket_gate(&env);

    let waiting = agent_cli(
        &env,
        &["wait", "worker", "--timeout", "10", "--project", "repo"],
        Some(&lead),
    )
    .spawn()
    .unwrap();
    let held = hold_next(&gate).await;
    pass_through(gate, real.clone());
    let mut send = agent_cli(
        &env,
        &["send", "lead", "status?", "--project", "repo"],
        None,
    );
    let send = tokio::task::spawn_blocking(move || send.output().unwrap())
        .await
        .unwrap();
    assert!(send.status.success(), "send failed: {send:?}");
    forward(held, real);

    let out = tokio::task::spawn_blocking(move || waiting.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a message sent during startup ends the wait with 3: {out:?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// Only a submitted Enter ends a wait: a pasted CRLF that is still draft
/// text must not, while the kitty-encoded Enter the TUI sends must.
#[tokio::test]
async fn agent_wait_ends_on_a_submitted_enter_not_on_a_pasted_newline() {
    let pasted = b"\x1b[200~first\r\nsecond\x1b[201~".to_vec();
    let kitty_enter = b"\x1b[13u".to_vec();
    let mut wrong = Vec::new();
    for (input, want) in [(pasted, 0), (kitty_enter, 3)] {
        let env = TestEnv::new();
        let LeadAndWorker {
            mut daemon,
            mut c,
            lead,
            worker,
            project,
        } = lead_waiting_on_running_worker(&env).await;
        let (gate, real) = install_socket_gate(&env);

        let waiting = agent_cli(
            &env,
            &["wait", "worker", "--timeout", "10", "--project", "repo"],
            Some(&lead),
        )
        .spawn()
        .unwrap();
        let held = hold_next(&gate).await;
        write_frame(
            &mut c,
            &ClientRequest::InputAcknowledged {
                req_id: 9,
                id: lead.clone(),
                project,
                data: input.clone(),
            },
        )
        .await
        .unwrap();
        let acked = read_events_until(&mut c, Duration::from_secs(5), |evs| {
            find_ack(evs, 9).is_some()
        })
        .await;
        assert!(
            matches!(find_ack(&acked, 9), Some(ServerEvent::Ack { .. })),
            "{acked:?}"
        );
        forward(held, real);
        if want == 0 {
            emit_progress(&mut c, &worker, "0", nebula_core::AgentStatus::Finished).await;
        }

        let out = tokio::task::spawn_blocking(move || waiting.wait_with_output().unwrap())
            .await
            .unwrap();
        if out.status.code() != Some(want) {
            wrong.push(format!(
                "input {:?} should end the wait with {want}: {out:?}",
                String::from_utf8_lossy(&input)
            ));
        }

        write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
        wait_for_exit(&mut daemon);
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A lead on the laptop steering a worker on findl: the send goes through
/// the viewer daemon's relay, and the wait running on the host must still
/// end with 3 while the worker it waits on keeps running.
#[tokio::test]
async fn agent_wait_on_a_host_returns_early_for_a_relayed_send() {
    let host_env = TestEnv::new();
    let repo = host_env.make_repo();
    let mut host_daemon = host_env.spawn_daemon();
    let mut h = connect(&host_env.sock()).await;
    handshake(&mut h).await;
    let worktree = add_project_get_main_worktree(&mut h, &repo).await;
    let lead = create_agent_get_id(&mut h, &worktree.id, "lead", 2).await;
    let worker = create_agent_get_id(&mut h, &worktree.id, "worker", 3).await;
    attach_agent(&mut h, &lead).await;
    attach_agent(&mut h, &worker).await;
    emit_progress(&mut h, &worker, "3", nebula_core::AgentStatus::Running).await;

    let proxy = format!(
        "NEBULA_RUNTIME_DIR='{}' NEBULA_DATA_DIR='{}' '{}' proxy",
        host_env.runtime_dir.display(),
        host_env.tmp.path().join("data").display(),
        env!("CARGO_BIN_EXE_nebula")
    );
    let env = TestEnv::new();
    let mut daemon = env.spawn_daemon_with("/bin/sh", &[("NEBULA_RELAY_CMD", proxy.as_str())]);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: Some("testhost".into()),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(15), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == lead && a.alive)
        })
    })
    .await;

    let waiting = agent_cli(
        &host_env,
        &["wait", "worker", "--timeout", "30", "--project", "repo"],
        Some(&lead),
    )
    .spawn()
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let send = agent_cli(
        &env,
        &["send", "lead", "status?", "--project", "repo@testhost"],
        None,
    )
    .output()
    .unwrap();
    assert!(send.status.success(), "relayed send failed: {send:?}");

    let out = tokio::task::spawn_blocking(move || waiting.wait_with_output().unwrap())
        .await
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a relayed message ends the host-side wait with 3: {out:?}"
    );
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows, serde_json::json!([]), "the worker is still running");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    write_frame(&mut h, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut host_daemon);
}

struct LeadAndWorker {
    daemon: DaemonChild,
    c: UnixStream,
    lead: nebula_core::AgentId,
    worker: nebula_core::AgentId,
    project: nebula_core::ProjectId,
}

/// A test daemon with agents `lead` and `worker` on project `repo`, both
/// attached on the returned connection, the worker mid-turn (running).
async fn lead_waiting_on_running_worker(env: &TestEnv) -> LeadAndWorker {
    let repo = env.make_repo();
    let daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;
    let lead = create_agent_get_id(&mut c, &worktree.id, "lead", 2).await;
    let worker = create_agent_get_id(&mut c, &worktree.id, "worker", 3).await;
    attach_agent(&mut c, &lead).await;
    attach_agent(&mut c, &worker).await;
    emit_progress(&mut c, &worker, "3", nebula_core::AgentStatus::Running).await;
    LeadAndWorker {
        daemon,
        c,
        lead,
        worker,
        project: worktree.project_id,
    }
}

/// Moves the daemon's socket aside (it keeps listening on the moved path)
/// and binds a gate in its place, so a test controls when a client's
/// connection reaches the daemon. Returns the gate and the daemon's path.
fn install_socket_gate(env: &TestEnv) -> (tokio::net::UnixListener, PathBuf) {
    let real = env.runtime_dir.join("daemon.real.sock");
    std::fs::rename(env.sock(), &real).unwrap();
    (tokio::net::UnixListener::bind(env.sock()).unwrap(), real)
}

/// The next client to connect through the gate, held back from the daemon.
async fn hold_next(gate: &tokio::net::UnixListener) -> UnixStream {
    let (held, _) = tokio::time::timeout(Duration::from_secs(10), gate.accept())
        .await
        .expect("no client connected through the gate")
        .unwrap();
    held
}

/// Every later client goes straight through to the daemon.
fn pass_through(gate: tokio::net::UnixListener, real: PathBuf) {
    tokio::spawn(async move {
        while let Ok((client, _)) = gate.accept().await {
            forward(client, real.clone());
        }
    });
}

fn forward(mut client: UnixStream, real: PathBuf) {
    tokio::spawn(async move {
        let mut daemon = UnixStream::connect(&real).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut client, &mut daemon).await;
    });
}

async fn attach_agent(c: &mut UnixStream, agent: &nebula_core::AgentId) {
    write_frame(
        c,
        &ClientRequest::Attach {
            session: SessionRef::Agent(agent.clone()),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();
}

/// Move an attached agent's status with the OSC 9;4 progress bytes Claude
/// emits ("3" starts a turn, "0" ends it).
async fn emit_progress(
    c: &mut UnixStream,
    agent: &nebula_core::AgentId,
    state: &str,
    want: nebula_core::AgentStatus,
) {
    write_frame(
        c,
        &ClientRequest::Input {
            session: SessionRef::Agent(agent.clone()),
            data: format!("printf '\\033]9;4;{state};\\007'\n").into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent: a, status, .. }
                if a == agent && *status == want)
        })
    })
    .await;
}

/// `nebula agent <args>` against the test daemon, run as the session
/// `caller` when given (as from inside it), else as no session.
fn agent_cli(
    env: &TestEnv,
    args: &[&str],
    caller: Option<&nebula_core::AgentId>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"));
    cmd.arg("agent")
        .args(args)
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for k in LIVE_SESSION_ENV {
        cmd.env_remove(k);
    }
    if let Some(id) = caller {
        cmd.env("NEBULA_AGENT_ID", id.as_str());
    }
    cmd
}

#[tokio::test]
async fn hook_cwd_rehomes_agent_to_other_worktree() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityUpserted {
                    entity: Entity::Worktree(_)
                }
            )
        })
    })
    .await;
    let main_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } => Some(w.clone()),
            _ => None,
        })
        .unwrap();

    // A second worktree — the one the agent will "enter".
    write_frame(
        &mut c,
        &ClientRequest::CreateWorktree {
            req_id: 2,
            project: main_worktree.project_id.clone(),
            branch: "feat".into(),
            base: None,
        },
    )
    .await
    .unwrap();
    // The upsert broadcast and the Ack ride different channels — wait for
    // the upsert itself.
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Worktree(w) }
                if w.branch == "feat")
        })
    })
    .await;
    let feat_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.branch == "feat" => Some(w.clone()),
            _ => None,
        })
        .expect("feat worktree upsert");

    // Agent lives in the main checkout.
    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 3,
            worktree: main_worktree.id.clone(),
            name: "mover".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 3).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 3).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();

    // POST a hook from inside the agent PTY whose payload reports the feat
    // worktree as cwd — exactly what claude sends after entering it.
    let sref = SessionRef::Agent(agent_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();
    let body = format!(
        r#"{{"session_id":"sess-1","cwd":"{}"}}"#,
        feat_worktree.path.display()
    );
    let curl = format!(
        "curl -sS -m 3 -X POST -H \"Authorization: Bearer $NEBULA_API_TOKEN\" \
         -H 'Content-Type: application/json' -d '{body}' \
         \"$NEBULA_API_URL/api/hooks/claude?agentId=$NEBULA_AGENT_ID&hookEvent=UserPromptSubmit\"\n"
    );
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl.into_bytes(),
        },
    )
    .await
    .unwrap();

    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && a.worktree_id == feat_worktree.id)
        })
    })
    .await;
    assert!(
        events.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && a.worktree_id == feat_worktree.id)
        }),
        "agent re-homed to the worktree its hook cwd reported: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// The inverse of the cwd-rehome test: a *user* move of a live agent must
/// relocate the process too. The PTY is killed and respawned in the target
/// checkout — left running in the old one, its hooks would keep reporting
/// the old cwd and the daemon would snap the row right back.
#[tokio::test]
async fn move_agent_respawns_live_session_in_target_worktree() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityUpserted {
                    entity: Entity::Worktree(_)
                }
            )
        })
    })
    .await;
    let main_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } => Some(w.clone()),
            _ => None,
        })
        .unwrap();

    write_frame(
        &mut c,
        &ClientRequest::CreateWorktree {
            req_id: 2,
            project: main_worktree.project_id.clone(),
            branch: "feat".into(),
            base: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Worktree(w) }
                if w.branch == "feat")
        })
    })
    .await;
    let feat_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.branch == "feat" => Some(w.clone()),
            _ => None,
        })
        .expect("feat worktree upsert");

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 3,
            worktree: main_worktree.id.clone(),
            name: "mover".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 3).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 3).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();
    let sref = SessionRef::Agent(agent_id.clone());

    // Sanity: the stand-in sh really runs in the main checkout ("…/repo",
    // never "repo-worktrees"). Suffix match sidesteps macOS /private
    // symlink canonicalization in pwd's output.
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: b"pwd\n".to_vec(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        String::from_utf8_lossy(&collected_output(evs)).contains("/repo\r")
    })
    .await;

    // The move: row re-homes AND the PTY comes back alive in the target.
    write_frame(
        &mut c,
        &ClientRequest::MoveAgent {
            req_id: 4,
            id: agent_id.clone(),
            worktree: feat_worktree.id.clone(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && a.worktree_id == feat_worktree.id && a.alive)
        })
    })
    .await;
    assert!(
        events.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && a.worktree_id == feat_worktree.id && a.alive)
        }),
        "moved agent re-homed and respawned alive: {events:#?}"
    );

    // The respawned process must actually sit in the feat checkout — that
    // is what keeps its hook cwds from re-homing the row back.
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: b"pwd\n".to_vec(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        String::from_utf8_lossy(&collected_output(evs)).contains("repo-worktrees/feat")
    })
    .await;

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// Codex mirror of the claude hook test: a codex-kind agent gets its hooks
/// installed into codex's home (not the worktree — one trust approval has
/// to cover every worktree), and posts to `/api/hooks/codex` drive the same
/// status machine (PermissionRequest is codex's native waiting signal).
#[tokio::test]
async fn codex_hooks_install_and_drive_status() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let codex_home = env.tmp.path().join("codex-home");
    // A worktree copy from an older nebula, alongside a foreign managed
    // group: the spawn must prune ours and leave theirs alone.
    let stale = repo.join(".codex");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(
        stale.join("hooks.json"),
        r#"{"hooks":{"Stop":[
            {"_nebulaManaged":true,"hooks":[{"type":"command",
              "command":"curl $NEBULA_API_URL/api/hooks/codex?agentId=$NEBULA_AGENT_ID"}]},
            {"_mcManaged":true,"hooks":[{"type":"command","command":"curl $MC_API_URL/x"}]}]}}"#,
    )
    .unwrap();
    let mut daemon =
        env.spawn_daemon_with("/bin/sh", &[("CODEX_HOME", codex_home.to_str().unwrap())]);

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityUpserted {
                    entity: Entity::Worktree(_)
                }
            )
        })
    })
    .await;
    let worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } => Some(w.clone()),
            _ => None,
        })
        .unwrap();

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: "codexed".into(),
            kind: AgentKind::Codex,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();

    // Codex hooks were installed into codex's home — and only codex-shaped
    // ones (no claude-specific Notification/AskUserQuestion groups).
    let hooks_path = codex_home.join("hooks.json");
    assert!(hooks_path.exists(), "codex hooks installed into codex home");
    let hooks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks_path).unwrap()).unwrap();
    assert!(hooks["hooks"]["Stop"][0]["_nebulaManaged"]
        .as_bool()
        .unwrap());
    assert!(hooks["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .contains("/api/hooks/codex?"));
    assert!(hooks["hooks"].get("Notification").is_none());
    assert!(hooks["hooks"].get("PreToolUse").is_none());

    // The stale worktree copy lost our group and kept the foreign one.
    let stale_hooks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(stale.join("hooks.json")).unwrap()).unwrap();
    let stop = stale_hooks["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 1, "only the foreign group survives: {stop:#?}");
    assert!(stop[0]["_mcManaged"].as_bool().unwrap());

    // Drive the shell inside the agent PTY to POST codex hooks with its env.
    let sref = SessionRef::Agent(agent_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 120,
            rows: 30,
        },
    )
    .await
    .unwrap();
    let curl = |event: &str, body: &str| {
        format!(
            "curl -sS -m 3 -X POST -H \"Authorization: Bearer $NEBULA_API_TOKEN\" \
             -H 'Content-Type: application/json' -d '{body}' \
             \"$NEBULA_API_URL/api/hooks/codex?agentId=$NEBULA_AGENT_ID&hookEvent={event}\"\n"
        )
    };

    // UserPromptSubmit → running
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl("UserPromptSubmit", r#"{"session_id":"codex-sess-1"}"#).into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Running, .. }
                if *agent == agent_id)
        })
    })
    .await;

    // PermissionRequest (codex's native waiting signal) → needs_feedback
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl("PermissionRequest", r#"{"session_id":"codex-sess-1"}"#).into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::NeedsFeedback, .. }
                if *agent == agent_id)
        })
    })
    .await;

    // Stop → finished
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: curl("Stop", r#"{"session_id":"codex-sess-1"}"#).into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::StatusChanged { agent, status: nebula_core::AgentStatus::Finished, .. }
                if *agent == agent_id)
        })
    })
    .await;

    // Kind and session id survive a daemon restart (feeds `codex resume`).
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    let mut daemon2 = env.spawn_daemon();
    let mut c2 = connect(&env.sock()).await;
    handshake(&mut c2).await;
    write_frame(&mut c2, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c2, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let ServerEvent::Snapshot { agents, .. } = &events[0] else {
        panic!()
    };
    assert_eq!(agents[0].kind, AgentKind::Codex, "kind persisted");
    assert_eq!(
        agents[0].session_id.as_deref(),
        Some("codex-sess-1"),
        "codex session id persisted"
    );
    write_frame(&mut c2, &ClientRequest::Shutdown)
        .await
        .unwrap();
    wait_for_exit(&mut daemon2);
}

#[tokio::test]
async fn external_worktrees_are_adopted_and_dropped() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 1).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, 1), Some(ServerEvent::Ack { .. })),
        "AddProject failed: {events:#?}"
    );

    // A worktree created behind nebula's back — exactly what an agent (or a
    // human in another shell) does.
    let wt_path = env.tmp.path().join("repo-worktrees").join("agent-branch");
    let git_worktree = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .arg("worktree")
            .args(args)
            .arg(&wt_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(
        git_worktree(&["add", "-b", "agent-branch"]),
        "external git worktree add failed"
    );

    // The auto-sync adopts it without any client request.
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| matches!(
            e,
            ServerEvent::EntityUpserted { entity: Entity::Worktree(w) } if w.branch == "agent-branch"
        ))
    })
    .await;
    let adopted = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.branch == "agent-branch" => Some(w.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!adopted.is_main, "adopted checkout is not the main row");
    assert!(
        adopted.path.exists(),
        "adopted row points at the real checkout"
    );

    // Removing it externally drops the row too (nothing lives there).
    assert!(
        git_worktree(&["remove", "--force"]),
        "external git worktree remove failed"
    );
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityRemoved { id: EntityId::Worktree(id) } if *id == adopted.id
            )
        })
    })
    .await;

    // Switching branches on the root checkout renames the main row in place
    // (the probe watches .git/HEAD, not just the worktrees registry).
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["checkout", "-b", "renamed-root"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success(),
        "git checkout -b renamed-root failed"
    );
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityUpserted { entity: Entity::Worktree(w) }
                    if w.is_main && w.branch == "renamed-root"
            )
        })
    })
    .await;
    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::EntityUpserted { entity: Entity::Worktree(w) }
                if w.is_main && w.branch == "renamed-root"
        )),
        "main row should refresh to the new branch: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// `nebula upgrade` daemon handoff: with a live session the old daemon is
/// left running (restart is the user's call); once idle, the upgrade shuts
/// it down so the next launch spawns the new binary.
#[tokio::test]
async fn upgrade_shuts_down_idle_daemon_but_spares_live_sessions() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 1).is_some()
            && evs.iter().any(|e| {
                matches!(
                    e,
                    ServerEvent::EntityUpserted {
                        entity: Entity::Worktree(_)
                    }
                )
            })
    })
    .await;
    let main_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.is_main => Some(w.clone()),
            _ => None,
        })
        .expect("main worktree upsert");

    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 2,
            worktree: main_worktree.id.clone(),
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    let term_id = term_id.clone();

    // Stub installer: the upgrade command runs it, then handles the daemon.
    let script = env.tmp.path().join("stub-install.sh");
    std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
    let run_upgrade = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(["upgrade", "--force"])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .env("NEBULA_INSTALL_URL", format!("file://{}", script.display()))
            .output()
            .unwrap()
    };

    // A live terminal PTY keeps the daemon alive through the upgrade.
    let out = run_upgrade();
    assert!(
        out.status.success(),
        "upgrade failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("1 live session"),
        "expected live-session note, got: {stdout}"
    );
    assert!(
        daemon.try_wait().unwrap().is_none(),
        "daemon must keep running while sessions are live"
    );

    // Exit the shell; the daemon marks the terminal dead and is now idle.
    let sref = SessionRef::Terminal(term_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref,
            data: b"exit\n".to_vec(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityUpserted { entity: Entity::Terminal(t) }
                    if t.id == term_id && !t.alive
            )
        })
    })
    .await;

    let out = run_upgrade();
    assert!(
        out.status.success(),
        "idle upgrade failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no live sessions"),
        "expected idle-shutdown note, got: {stdout}"
    );
    wait_for_exit(&mut daemon);
}

/// AddProject with `create_missing` makes the directory and `git init`s it;
/// with `git_init_on_create: false` in config.json the directory is still
/// created but adding fails (not a git repository).
#[tokio::test]
async fn add_project_creates_missing_dir_and_inits() {
    let env = TestEnv::new();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;

    let new_dir = env.tmp.path().join("brand-new-project");
    assert!(!new_dir.exists());
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: new_dir.clone(),
            name: None,
            create_missing: true,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 1).is_some()
    })
    .await;
    assert!(
        matches!(
            find_ack(&events, 1),
            Some(ServerEvent::Ack {
                created: Some(EntityId::Project(_)),
                ..
            })
        ),
        "AddProject with create_missing failed: {events:#?}"
    );
    assert!(new_dir.join(".git").is_dir(), "git init ran in the new dir");

    // Opt out of git init via config: the dir is created, the add errors.
    std::fs::write(
        env.tmp.path().join("data").join("config.json"),
        r#"{"git_init_on_create": false}"#,
    )
    .unwrap();
    let bare_dir = env.tmp.path().join("bare-new-project");
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 2,
            path: bare_dir.clone(),
            name: None,
            create_missing: true,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, 2), Some(ServerEvent::Error { .. })),
        "expected not-a-git-repo error: {events:#?}"
    );
    assert!(bare_dir.is_dir(), "dir created even without git init");
    assert!(!bare_dir.join(".git").exists(), "git init skipped");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// Subscribe + AddProject boilerplate; returns the main worktree row.
async fn add_project_get_main_worktree(c: &mut UnixStream, repo: &Path) -> nebula_core::Worktree {
    write_frame(c, &ClientRequest::Subscribe).await.unwrap();
    read_events_until(c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    write_frame(
        c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.to_path_buf(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(c, Duration::from_secs(5), |evs| {
        find_ack(evs, 1).is_some()
            && evs.iter().any(|e| {
                matches!(
                    e,
                    ServerEvent::EntityUpserted {
                        entity: Entity::Worktree(w)
                    } if w.is_main
                )
            })
    })
    .await;
    events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.is_main => Some(w.clone()),
            _ => None,
        })
        .expect("main worktree upsert")
}

/// PrewarmAgent boots the CLI while the user is "typing the name"; the
/// following CreateAgent must adopt that already-running PTY (its slow boot
/// output is already in scrollback) and replay the hooks it fired before the
/// row existed (SessionStart → stored resume session id).
#[tokio::test]
async fn prewarmed_session_is_adopted_by_create_agent() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    // Stand-in agent CLI with a deliberately slow boot: posts SessionStart
    // (like claude does), sleeps, prints a marker, then becomes a shell. If
    // adoption works, the marker is in scrollback the moment we attach; a
    // cold spawn at CreateAgent time couldn't print it for another 3s.
    let script = env.tmp.path().join("slow-agent.sh");
    std::fs::write(
        &script,
        concat!(
            "#!/bin/sh\n",
            "echo $$ >> \"$(dirname \"$0\")/warm-pids\"\n",
            "curl -sS -m 3 -X POST -H \"Authorization: Bearer $NEBULA_API_TOKEN\" \\\n",
            "  -H 'Content-Type: application/json' -d '{\"session_id\":\"warm-sid-99\"}' \\\n",
            "  \"$NEBULA_API_URL/api/hooks/claude?agentId=$NEBULA_AGENT_ID&hookEvent=SessionStart\" \\\n",
            "  >/dev/null 2>&1\n",
            "sleep 3\n",
            "echo PREWARM_READY\n",
            "exec /bin/sh\n",
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    let prewarm = ClientRequest::PrewarmAgent {
        worktree: worktree.id.clone(),
        kind: AgentKind::Claude,
        model: None,
        effort: None,
    };
    // Kind picked — warm the CLI. No reply expected.
    write_frame(&mut c, &prewarm).await.unwrap();
    // "User types the name": long enough for the warm boot to finish.
    tokio::time::sleep(Duration::from_millis(4500)).await;
    let branch = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "warm-blocked"])
        .status()
        .unwrap();
    assert!(branch.success());
    write_frame(
        &mut c,
        &ClientRequest::ResolveBranchCheckout {
            req_id: 901,
            project: worktree.project_id.clone(),
            branch: "warm-blocked".into(),
        },
    )
    .await
    .unwrap();
    let switched = read_events_until(&mut c, Duration::from_secs(10), |e| {
        find_ack(e, 901).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&switched, 901), Some(ServerEvent::Ack { .. })),
        "{switched:?}"
    );
    assert!(!repo.with_file_name("repo-worktrees").exists());
    let head = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "--show-current"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "warm-blocked");
    let pids = std::fs::read_to_string(env.tmp.path().join("warm-pids")).unwrap();
    let stopped = pids.lines().next().unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", stopped])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!alive.success(), "the switch stopped the warm PTY");

    write_frame(&mut c, &prewarm).await.unwrap();
    tokio::time::sleep(Duration::from_millis(4500)).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: "warm-agent".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();

    // Attach: the boot marker must already be there (window far shorter
    // than the script's 3s boot, so a cold spawn cannot pass).
    let sref = SessionRef::Agent(agent_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 100,
            rows: 30,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(2), |evs| {
        String::from_utf8_lossy(&collected_output(evs)).contains("PREWARM_READY")
    })
    .await;
    drop(events);

    // The adopted PTY is interactive.
    let marker = "adopted_marker_7731";
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: format!("echo {marker}\n").into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        String::from_utf8_lossy(&collected_output(evs))
            .matches(marker)
            .count()
            >= 2
    })
    .await;

    // The SessionStart the warm CLI posted before the row existed was
    // buffered and replayed: the agent row carries the resume session id.
    let mut c2 = connect(&env.sock()).await;
    handshake(&mut c2).await;
    write_frame(&mut c2, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c2, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let ServerEvent::Snapshot { agents, .. } = &events[0] else {
        panic!("expected snapshot");
    };
    let agent = agents.iter().find(|a| a.id == agent_id).expect("agent row");
    assert_eq!(agent.name, "warm-agent");
    assert!(agent.alive, "adopted session is live");
    assert_eq!(
        agent.session_id.as_deref(),
        Some("warm-sid-99"),
        "buffered SessionStart replayed at adoption"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// A prewarmed CLI that dies immediately (the "claude/codex not installed"
/// shape) must not poison creation: CreateAgent quietly falls back to a
/// fresh spawn and still succeeds.
#[tokio::test]
async fn dead_prewarm_falls_back_to_cold_spawn() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let script = env.tmp.path().join("dying-agent.sh");
    std::fs::write(&script, "#!/bin/sh\nexit 127\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    write_frame(
        &mut c,
        &ClientRequest::PrewarmAgent {
            worktree: worktree.id.clone(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
        },
    )
    .await
    .unwrap();
    // Give the warm spawn time to die.
    tokio::time::sleep(Duration::from_millis(1000)).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: "fallback-agent".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    assert!(
        matches!(
            find_ack(&events, 2),
            Some(ServerEvent::Ack {
                created: Some(EntityId::Agent(_)),
                ..
            })
        ),
        "CreateAgent must survive a dead prewarm: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// `kill -0` liveness probe (also true for an unreaped zombie).
/// An agent CLI that isn't installed must be refused up front. Before this
/// check the create "succeeded": the login shell printed `command not found`
/// into a PTY that died at once, leaving a dead session row indistinguishable
/// from a fresh one.
#[tokio::test]
async fn create_agent_refuses_when_the_cli_is_not_installed() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let shell = env.blind_shell();
    let mut daemon = env.spawn_daemon_with_shell(&shell);

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    for (req_id, kind, want) in [
        (10u64, AgentKind::Claude, "claude"),
        (11, AgentKind::Codex, "codex"),
        // Cursor's binary is `cursor-agent`; the message must name that, not
        // the kind, or the user goes looking for the wrong thing to install.
        (12, AgentKind::Cursor, "cursor-agent"),
        (13, AgentKind::Pi, "pi"),
    ] {
        write_frame(
            &mut c,
            &ClientRequest::CreateAgent {
                expected_branch: None,
                req_id,
                worktree: worktree.id.clone(),
                name: format!("agent-{req_id}"),
                kind,
                model: None,
                effort: None,
                auto_title: false,
                prompt: None,
                writer: WriterClaim::Off,
            },
        )
        .await
        .unwrap();
        let events = read_events_until(&mut c, Duration::from_secs(20), |evs| {
            evs.iter().any(|e| {
                matches!(e, ServerEvent::Error { req_id: Some(r), .. } if *r == req_id)
                    || matches!(e, ServerEvent::Ack { req_id: r, .. } if *r == req_id)
            })
        })
        .await;
        let message = events
            .iter()
            .find_map(|e| match e {
                ServerEvent::Error {
                    req_id: Some(r),
                    message,
                } if *r == req_id => Some(message.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{kind:?} create must be refused, got: {events:#?}"));
        assert!(
            message.starts_with(&format!("{want} was not found on your PATH")),
            "{kind:?}: {message}"
        );
    }

    // And no half-created rows left behind in the Sessions column.
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let agents = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::Snapshot { agents, .. } => Some(agents.clone()),
            _ => None,
        })
        .expect("snapshot");
    assert!(agents.is_empty(), "refused creates left rows: {agents:#?}");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// The mirror of the refusal: when the CLI *is* on the login shell's PATH the
/// check must stay out of the way — guards against the probe being so strict
/// (or so slow) that it blocks legitimate creates.
#[tokio::test]
async fn create_agent_succeeds_when_the_cli_is_on_the_login_shell_path() {
    let env = TestEnv::new();
    let repo = env.make_repo();

    // A stub `claude` that just sits there, reachable only via this shell.
    let bin = env.tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("claude");
    std::fs::write(&stub, "#!/bin/sh\nsleep 60\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let shell = env.tmp.path().join("seeing-shell.sh");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\nPATH={}:/usr/bin:/bin\nexport PATH\nexec /bin/sh -c \"$4\"\n",
            bin.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut daemon = env.spawn_daemon_with_shell(&shell);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 7,
            worktree: worktree.id.clone(),
            name: "real-agent".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(20), |evs| {
        find_ack(evs, 7).is_some()
            || evs.iter().any(|e| {
                matches!(
                    e,
                    ServerEvent::Error {
                        req_id: Some(7),
                        ..
                    }
                )
            })
    })
    .await;
    assert!(
        matches!(
            find_ack(&events, 7),
            Some(ServerEvent::Ack {
                created: Some(EntityId::Agent(_)),
                ..
            })
        ),
        "an installed CLI must still create: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

fn pid_alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn wait_pid_dead(pid: i32, timeout: Duration, what: &str) {
    let deadline = tokio::time::Instant::now() + timeout;
    while pid_alive(pid) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} (pid {pid}) still running after {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn create_agent_get_id(
    c: &mut UnixStream,
    worktree: &nebula_core::WorktreeId,
    name: &str,
    req_id: u64,
) -> nebula_core::AgentId {
    write_frame(
        c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id,
            worktree: worktree.clone(),
            name: name.into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(c, Duration::from_secs(5), |evs| {
        find_ack(evs, req_id).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(id)),
        ..
    } = find_ack(&events, req_id).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    id.clone()
}

/// Poll a pidfile the fake agent writes on boot.
async fn read_pidfile(path: &Path) -> i32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Ok(pid) = s.trim().parse() {
                return pid;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pidfile {path:?} never appeared"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Archiving or deleting an agent must kill its CLI process — sessions must
/// not keep burning memory/CPU once the user has put them away.
#[tokio::test]
async fn archive_and_delete_kill_the_agent_process() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let pid_dir = env.tmp.path().join("pids");
    std::fs::create_dir_all(&pid_dir).unwrap();
    // Stand-in CLI: record the pid, then exec into a long sleep (same pid).
    let script = env.tmp.path().join("agent.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > '{}'/$NEBULA_AGENT_ID.pid\nexec sleep 600\n",
            pid_dir.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    let a1 = create_agent_get_id(&mut c, &worktree.id, "to-archive", 2).await;
    let a2 = create_agent_get_id(&mut c, &worktree.id, "to-delete", 3).await;
    let pid1 = read_pidfile(&pid_dir.join(format!("{}.pid", a1.0))).await;
    let pid2 = read_pidfile(&pid_dir.join(format!("{}.pid", a2.0))).await;
    assert!(pid_alive(pid1) && pid_alive(pid2), "fake CLIs should be up");

    // ---- archive kills the CLI and broadcasts archived + not-alive ----
    write_frame(
        &mut c,
        &ClientRequest::ArchiveAgent {
            req_id: 4,
            id: a1.clone(),
        },
    )
    .await
    .unwrap();
    // The Ack and the EntityUpserted broadcast race on the client stream —
    // wait for both.
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 4).is_some()
            && evs.iter().any(|e| {
                matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                    if a.id == a1 && a.archived)
            })
    })
    .await;
    let archived = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(a),
            } if a.id == a1 && a.archived => Some(a.clone()),
            _ => None,
        })
        .expect("archive upsert");
    assert!(
        !archived.alive,
        "archived agent should not be alive: {archived:?}"
    );
    wait_pid_dead(pid1, Duration::from_secs(5), "archived agent CLI").await;
    assert!(pid_alive(pid2), "the other agent must be untouched");

    // ---- delete kills the CLI too ----
    write_frame(
        &mut c,
        &ClientRequest::DeleteAgent {
            req_id: 5,
            id: a2.clone(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 5).is_some()
            && evs
                .iter()
                .any(|e| matches!(e, ServerEvent::EntityRemoved { id: EntityId::Agent(id) } if *id == a2))
    })
    .await;
    wait_pid_dead(pid2, Duration::from_secs(5), "deleted agent CLI").await;

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// A wedged CLI that ignores SIGHUP still gets cleared on archive: the kill
/// watchdog SIGKILLs its whole process group (grandchildren included) after
/// the grace period.
#[tokio::test]
async fn archive_sigkills_an_agent_that_ignores_sighup() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let dir = env.tmp.path().to_path_buf();
    // HUP-immune stand-in with a background child; `trap '' HUP` is inherited
    // by `sleep`, so neither dies from the polite signal alone.
    let script = dir.join("stubborn-agent.sh");
    std::fs::write(
        &script,
        format!(
            concat!(
                "#!/bin/sh\n",
                "trap '' HUP\n",
                "echo $$ > '{d}/agent.pid'\n",
                "sleep 600 &\n",
                "echo $! > '{d}/child.pid'\n",
                "wait\n",
            ),
            d = dir.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: "stubborn".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();
    let shell_pid = read_pidfile(&dir.join("agent.pid")).await;
    let child_pid = read_pidfile(&dir.join("child.pid")).await;

    write_frame(
        &mut c,
        &ClientRequest::ArchiveAgent {
            req_id: 3,
            id: agent_id,
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 3).is_some()
    })
    .await;
    // SIGHUP alone can't clear these; the ~3s watchdog escalation must.
    wait_pid_dead(shell_pid, Duration::from_secs(10), "HUP-immune agent CLI").await;
    wait_pid_dead(child_pid, Duration::from_secs(10), "agent CLI's grandchild").await;

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// PrewarmWorktreeSessions is a retired request: the daemon still accepts
/// it from older clients but boots nothing, so a dead session stays dead.
#[tokio::test]
async fn prewarm_worktree_sessions_boots_nothing() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    let term_id = term_id.clone();

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    let mut daemon2 = env.spawn_daemon();
    let mut c2 = connect(&env.sock()).await;
    handshake(&mut c2).await;
    write_frame(
        &mut c2,
        &ClientRequest::PrewarmWorktreeSessions {
            worktree: worktree.id.clone(),
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c2,
        &ClientRequest::RenameTerminal {
            req_id: 3,
            id: term_id.clone(),
            name: "still-dead".into(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c2, Duration::from_secs(5), |evs| {
        find_ack(evs, 3).is_some()
    })
    .await;

    let mut c3 = connect(&env.sock()).await;
    handshake(&mut c3).await;
    write_frame(&mut c3, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c3, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let Some(ServerEvent::Snapshot { terminals, .. }) = events
        .iter()
        .find(|e| matches!(e, ServerEvent::Snapshot { .. }))
    else {
        panic!("expected snapshot");
    };
    let term = terminals
        .iter()
        .find(|t| t.id == term_id)
        .expect("terminal row");
    assert!(
        !term.alive,
        "the retired prewarm booted a session: {term:#?}"
    );

    write_frame(&mut c3, &ClientRequest::Shutdown)
        .await
        .unwrap();
    wait_for_exit(&mut daemon2);
}

/// The idle reaper kills sessions in worktrees no client is looking at once
/// they age past `session_idle_timeout` — but spares terminals with a
/// command still running, and never touches an attached session no matter
/// how long it idles. A reaped agent revives on the next attach.
#[tokio::test]
async fn idle_sessions_reap_unwatched_but_spare_busy_and_attached() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    env.write_config(r#"{"session_idle_timeout": "2s"}"#);
    let mut daemon = env.spawn_daemon_with("/bin/sh", &[("NEBULA_IDLE_REAP_MS", "200")]);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: "idler".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();

    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 3,
            worktree: worktree.id.clone(),
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 3).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 3).unwrap()
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    let term_id = term_id.clone();

    // A pinned agent: idle and unwatched like the idler, but marked "never
    // kill" (schedules and background jobs are invisible to the status
    // machine, so pinning is the user's protection).
    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 4,
            worktree: worktree.id.clone(),
            name: "keeper".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 4).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(pinned_id)),
        ..
    } = find_ack(&events, 4).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let pinned_id = pinned_id.clone();
    write_frame(
        &mut c,
        &ClientRequest::SetAgentPinned {
            req_id: 5,
            id: pinned_id.clone(),
            pinned: true,
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 5).is_some()
    })
    .await;

    // Give the terminal a running command, then stop looking at anything.
    let term_sref = SessionRef::Terminal(term_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: term_sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: term_sref.clone(),
            data: b"sleep 30\n".to_vec(),
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::Detach {
            session: term_sref.clone(),
        },
    )
    .await
    .unwrap();

    // Unwatched: the idle agent is reaped after ~2s…
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && !a.alive)
        })
    })
    .await;
    // …while the terminal's sleep and the keeper's pin keep them alive.
    let term_reaped = |evs: &[ServerEvent]| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Terminal(t) }
                if t.id == term_id && !t.alive)
        })
    };
    let pinned_reaped = |evs: &[ServerEvent]| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == pinned_id && !a.alive)
        })
    };
    assert!(!term_reaped(&events), "busy terminal spared: {events:#?}");
    assert!(!pinned_reaped(&events), "pinned agent spared: {events:#?}");

    // Attaching revives the agent; an attached session then idles forever.
    let agent_sref = SessionRef::Agent(agent_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: agent_sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && a.alive)
        })
    })
    .await;
    // Well past the 2s timeout with sweeps every 200ms.
    tokio::time::sleep(Duration::from_secs(4)).await;
    write_frame(
        &mut c,
        &ClientRequest::RenameAgent {
            req_id: 6,
            id: agent_id.clone(),
            name: "still-here".into(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 6).is_some()
    })
    .await;
    assert!(
        !events.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && !a.alive)
        }),
        "attached agent never reaped: {events:#?}"
    );
    assert!(
        !term_reaped(&events),
        "busy terminal still spared: {events:#?}"
    );
    assert!(
        !pinned_reaped(&events),
        "pinned agent still spared: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// "In view" is the attached session, not its whole worktree: workers pile
/// up on the primary, so a sibling the user isn't attached to is reaped once
/// it idles past the timeout while the attached one stays up.
#[tokio::test]
async fn idle_reaper_kills_unattached_sibling_on_viewed_worktree() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    env.write_config(r#"{"session_idle_timeout": "2s"}"#);
    let mut daemon = env.spawn_daemon_with("/bin/sh", &[("NEBULA_IDLE_REAP_MS", "200")]);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    let mut ids = Vec::new();
    for (req_id, name) in [(2, "watched"), (3, "sibling")] {
        write_frame(
            &mut c,
            &ClientRequest::CreateAgent {
                expected_branch: None,
                req_id,
                worktree: worktree.id.clone(),
                name: name.into(),
                kind: AgentKind::Claude,
                model: None,
                effort: None,
                auto_title: false,
                prompt: None,
                writer: WriterClaim::Off,
            },
        )
        .await
        .unwrap();
        let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
            find_ack(evs, req_id).is_some()
        })
        .await;
        let ServerEvent::Ack {
            created: Some(EntityId::Agent(id)),
            ..
        } = find_ack(&events, req_id).unwrap()
        else {
            panic!("CreateAgent failed: {events:#?}");
        };
        ids.push(id.clone());
    }
    let (watched_id, sibling_id) = (ids[0].clone(), ids[1].clone());

    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: SessionRef::Agent(watched_id.clone()),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();

    let reaped = |evs: &[ServerEvent], id: &nebula_core::AgentId| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if &a.id == id && !a.alive)
        })
    };
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        reaped(evs, &sibling_id)
    })
    .await;
    assert!(
        !reaped(&events, &watched_id),
        "attached agent never reaped: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// A reaped agent keeps its last screen: ReadSession serves the saved text,
/// marked stale, instead of "not running".
#[tokio::test]
async fn read_session_serves_the_last_screen_of_a_reaped_agent() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    env.write_config(r#"{"session_idle_timeout": "2s"}"#);
    let mut daemon = env.spawn_daemon_with("/bin/sh", &[("NEBULA_IDLE_REAP_MS", "200")]);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;
    let agent_id = create_agent_get_id(&mut c, &worktree.id, "worker", 2).await;
    let sref = SessionRef::Agent(agent_id.clone());

    let marker = "nebula_reaped_screen_marker";
    {
        let mut viewer = connect(&env.sock()).await;
        handshake(&mut viewer).await;
        write_frame(
            &mut viewer,
            &ClientRequest::Attach {
                session: sref.clone(),
                from_seq: None,
                cols: 80,
                rows: 24,
            },
        )
        .await
        .unwrap();
        write_frame(
            &mut viewer,
            &ClientRequest::Input {
                session: sref.clone(),
                data: format!("echo {marker}\n").into_bytes(),
            },
        )
        .await
        .unwrap();
        read_events_until(&mut viewer, Duration::from_secs(5), |evs| {
            String::from_utf8_lossy(&collected_output(evs))
                .matches(marker)
                .count()
                >= 2
        })
        .await;
    }

    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && !a.alive)
        })
    })
    .await;

    write_frame(
        &mut c,
        &ClientRequest::ReadSession {
            req_id: 9,
            session: sref.clone(),
            lines: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 9).is_some()
            || evs
                .iter()
                .any(|e| matches!(e, ServerEvent::SessionText { req_id: 9, .. }))
    })
    .await;
    let text = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::SessionText {
                req_id: 9, text, ..
            } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("reaped agent has no saved screen: {events:#?}"));
    assert!(text.contains(marker), "saved screen has the echo: {text:?}");
    assert!(
        text.lines()
            .next()
            .is_some_and(|l| l.contains("not running")),
        "saved screen is marked stale: {text:?}"
    );
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["agent", "read", "--project", "repo", "--", "worker"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "`nebula agent read` on a reaped agent: {cli:?}"
    );
    assert!(
        String::from_utf8_lossy(&cli.stdout).contains(marker),
        "`nebula agent read` prints the saved screen: {cli:?}"
    );

    write_frame(
        &mut c,
        &ClientRequest::DeleteAgent {
            req_id: 10,
            id: agent_id.clone(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 10).is_some()
    })
    .await;
    write_frame(
        &mut c,
        &ClientRequest::ReadSession {
            req_id: 11,
            session: sref.clone(),
            lines: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 11).is_some()
            || evs
                .iter()
                .any(|e| matches!(e, ServerEvent::SessionText { req_id: 11, .. }))
    })
    .await;
    assert!(
        matches!(find_ack(&events, 11), Some(ServerEvent::Error { .. })),
        "a deleted agent's saved screen is gone: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// ReadSession renders a live session's ring to plain text daemon-side
/// (`nebula agent read`): the marker echoed into the PTY comes back in
/// SessionText, `lines` tails it, and a session with no live PTY errors.
#[tokio::test]
async fn read_session_returns_screen_text() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 1).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, 1), Some(ServerEvent::Ack { .. })),
        "AddProject failed: {events:#?}"
    );

    // The main worktree upsert goes to subscribers only; fetch it from the
    // snapshot instead (same as the kitty test).
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let worktree_id = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::Snapshot { worktrees, .. } => {
                worktrees.iter().find(|w| w.is_main).map(|w| w.id.clone())
            }
            _ => None,
        })
        .expect("snapshot has the main worktree");

    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 2,
            worktree: worktree_id,
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    let sref = SessionRef::Terminal(term_id.clone());

    // Attach spawns the PTY; echo a marker through it.
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    let marker = "nebula_read_session_marker";
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: format!("echo {marker}\n").into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        String::from_utf8_lossy(&collected_output(evs))
            .matches(marker)
            .count()
            >= 2
    })
    .await;

    // The rendered text carries the marker; a 1-line tail is 1 line.
    write_frame(
        &mut c,
        &ClientRequest::ReadSession {
            req_id: 3,
            session: sref.clone(),
            lines: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::SessionText { req_id: 3, .. }))
    })
    .await;
    let text = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::SessionText {
                req_id: 3, text, ..
            } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(text.contains(marker), "screen text has the echo: {text:?}");

    write_frame(
        &mut c,
        &ClientRequest::ReadSession {
            req_id: 4,
            session: sref.clone(),
            lines: Some(1),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::SessionText { req_id: 4, .. }))
    })
    .await;
    let tail = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::SessionText {
                req_id: 4, text, ..
            } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        tail.lines().count(),
        1,
        "lines:1 tails to one line: {tail:?}"
    );

    // A session with no live PTY answers with an Error, not a spawn.
    write_frame(
        &mut c,
        &ClientRequest::ReadSession {
            req_id: 5,
            session: SessionRef::Agent(nebula_core::AgentId("no-such-agent".into())),
            lines: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 5).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, 5), Some(ServerEvent::Error { .. })),
        "dead session should error: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

fn wait_for_exit(daemon: &mut std::process::Child) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match daemon.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "daemon exited with {status:?}");
                return;
            }
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            None => {
                let _ = daemon.kill();
                panic!("daemon did not exit after Shutdown");
            }
        }
    }
}

/// Poll the env dump the fake agent CLI writes on boot, returning the
/// NEBULA_* variables the real CLI's hooks (and `nebula rename`) would see.
async fn read_env_file(path: &Path) -> std::collections::HashMap<String, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            let map: std::collections::HashMap<String, String> = s
                .lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            if map.contains_key("NEBULA_API_URL") && map.contains_key("NEBULA_API_TOKEN") {
                return map;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "agent env dump {path:?} never appeared"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Raw HTTP POST to the daemon's hook receiver, standing in for the curl
/// one-liner the installed hook runs; returns (status, body) — the body is
/// what the hook would pipe into the CLI's stdout.
async fn hook_post(port: u16, path_query: &str, token: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let payload = r#"{"session_id":"s1"}"#;
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let req = format!(
        "POST {path_query} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status: u16 = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// The whole auto-title loop over real processes: a default-named agent's
/// UserPromptSubmit hook response carries the titling instruction, the
/// `nebula rename` CLI (what the model runs) applies it exactly once and
/// broadcasts the new name, and afterwards the instruction stops and a
/// retitle attempt is declined without failing.
#[tokio::test]
async fn auto_title_instruction_and_rename_flow() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let env_dir = env.tmp.path().join("agent-env");
    std::fs::create_dir_all(&env_dir).unwrap();
    // Stand-in CLI: capture the NEBULA_* env its hooks would use, then park.
    let script = env.tmp.path().join("agent.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nenv | grep '^NEBULA_' > '{}'/$NEBULA_AGENT_ID.env\nexec sleep 600\n",
            env_dir.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let worktree = add_project_get_main_worktree(&mut c, &repo).await;

    // Created with the accepted default name → auto-title pending.
    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: None,
            req_id: 2,
            worktree: worktree.id.clone(),
            name: "agent-1".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: true,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();

    let agent_env = read_env_file(&env_dir.join(format!("{}.env", agent_id.0))).await;
    let port: u16 = agent_env["NEBULA_API_URL"]
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let token = agent_env["NEBULA_API_TOKEN"].clone();
    let submit_path = format!(
        "/api/hooks/claude?agentId={}&hookEvent=UserPromptSubmit",
        agent_id.0
    );

    // First prompt on the untitled session: instruction rides the response.
    let (status, body) = hook_post(port, &submit_path, &token).await;
    assert_eq!(status, 200);
    assert_eq!(body, nebula_daemon::hooks::auto_title_injection());

    // The model obeys — `nebula rename` runs with the session's env.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["rename", "Fix", "Login", "Redirect"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .env("NEBULA_AGENT_ID", &agent_id.0)
        .output()
        .unwrap();
    assert!(out.status.success(), "rename failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Fix Login Redirect"), "stdout: {stdout}");
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) }
                if a.id == agent_id && a.name == "Fix Login Redirect")
        })
    })
    .await;

    // Titled now: the next prompt injects nothing.
    let (status, body) = hook_post(port, &submit_path, &token).await;
    assert_eq!((status, body.as_str()), (200, ""));

    // A repeat attempt is declined as a settled answer (exit 0), not a fault.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["rename", "Another", "Title"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .env("NEBULA_AGENT_ID", &agent_id.0)
        .output()
        .unwrap();
    assert!(out.status.success(), "declined rename must exit 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("already has a title"), "stdout: {stdout}");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// `nebula worktree delete` — the CLI must go through the daemon's delete
/// flow so the git checkout and nebula's worktree row disappear together
/// (raw `git worktree remove` leaves a ghost row in the Worktrees panel).
#[tokio::test]
async fn worktree_delete_cli_removes_checkout_and_row() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();

    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let _main = add_project_get_main_worktree(&mut c, &repo).await;

    let run_cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap()
    };

    let out = run_cli(&["worktree", "new", "scratch", "--project", "repo"]);
    assert!(out.status.success(), "worktree new failed: {out:?}");
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Worktree(w) }
                if w.branch == "scratch")
        })
    })
    .await;
    let path = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.branch == "scratch" => Some(w.path.clone()),
            _ => None,
        })
        .unwrap();
    assert!(path.exists(), "checkout should exist at {path:?}");

    let out = run_cli(&["worktree", "delete", "scratch", "--project", "repo"]);
    assert!(out.status.success(), "worktree delete failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"deleted\":true"), "stdout: {stdout}");
    read_events_until(&mut c, Duration::from_secs(10), |evs| {
        evs.iter().any(|e| {
            matches!(
                e,
                ServerEvent::EntityRemoved {
                    id: EntityId::Worktree(_)
                }
            )
        })
    })
    .await;
    assert!(!path.exists(), "checkout should be gone from {path:?}");

    // The primary checkout is refused with a clear message.
    let out = run_cli(&["worktree", "delete", "main", "--project", "repo"]);
    assert!(!out.status.success(), "deleting main must fail: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("primary checkout"),
        "stderr should explain the refusal: {stderr}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// A fresh checkout with the same package-manager lockfile should reuse the
/// primary checkout's top-level dependencies without running an install.
#[tokio::test]
async fn worktree_new_seeds_node_modules_when_lockfile_matches() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    commit_file(&repo, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    std::fs::create_dir(repo.join("node_modules")).unwrap();
    std::fs::write(repo.join("node_modules/seeded-package"), "from primary\n").unwrap();

    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let _main = add_project_get_main_worktree(&mut c, &repo).await;

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["worktree", "new", "seeded", "--project", "repo"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "worktree new failed: {out:?}");

    let seeded = repo
        .parent()
        .unwrap()
        .join("repo-worktrees/seeded/node_modules/seeded-package");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !seeded.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(std::fs::read_to_string(&seeded).unwrap(), "from primary\n");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// Dependency state from a different lockfile must never leak into the new
/// checkout, even when the primary checkout has `node_modules` available.
#[tokio::test]
async fn worktree_new_skips_node_modules_when_lockfile_differs() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    commit_file(&repo, "pnpm-lock.yaml", "lockfileVersion: '8.0'\n");
    std::fs::write(repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    std::fs::create_dir(repo.join("node_modules")).unwrap();
    std::fs::write(repo.join("node_modules/primary-only"), "must not copy\n").unwrap();

    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let _main = add_project_get_main_worktree(&mut c, &repo).await;

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["worktree", "new", "mismatch", "--project", "repo"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "worktree new failed: {out:?}");

    let node_modules = repo
        .parent()
        .unwrap()
        .join("repo-worktrees/mismatch/node_modules");
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    assert!(
        !node_modules.exists(),
        "different lockfiles must leave node_modules absent"
    );
}

/// Users can disable dependency seeding globally without changing normal
/// worktree creation behavior.
#[tokio::test]
async fn worktree_new_skips_node_modules_when_seeding_is_disabled() {
    let env = TestEnv::new();
    env.write_config(r#"{"seed_node_modules": false}"#);
    let repo = env.make_repo();
    commit_file(&repo, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    std::fs::create_dir(repo.join("node_modules")).unwrap();
    std::fs::write(repo.join("node_modules/primary-only"), "must not copy\n").unwrap();

    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let _main = add_project_get_main_worktree(&mut c, &repo).await;

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["worktree", "new", "disabled", "--project", "repo"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "worktree new failed: {out:?}");

    let node_modules = repo
        .parent()
        .unwrap()
        .join("repo-worktrees/disabled/node_modules");
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    assert!(
        !node_modules.exists(),
        "disabled seeding must leave node_modules absent"
    );
}

/// `nebula add <dir>` and the bare `nebula <dir>` shorthand: the one-shot CLI
/// The relay: a second daemon plays the remote host (reached through
/// `nebula proxy` instead of ssh). Adding `host:/path` here mirrors the
/// host's project, a terminal created and attached through this daemon
/// runs *there*, and it outlives this daemon — the promise the whole
/// design exists for.
#[tokio::test]
async fn relay_mirrors_a_host_and_its_sessions_outlive_the_viewer() {
    let host_env = TestEnv::new();
    let repo = host_env.make_repo();
    let mut host_daemon = host_env.spawn_daemon();
    let proxy = format!(
        "NEBULA_RUNTIME_DIR='{}' NEBULA_DATA_DIR='{}' '{}' proxy",
        host_env.runtime_dir.display(),
        host_env.tmp.path().join("data").display(),
        env!("CARGO_BIN_EXE_nebula")
    );

    let env = TestEnv::new();
    let relay_env = [("NEBULA_RELAY_CMD", proxy.as_str())];
    let mut daemon = env.spawn_daemon_with("/bin/sh", &relay_env);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    // Anchor the host's repo here. The host daemon adopts it on the
    // relay's request and its rows arrive as upserts, stamped with the host.
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: repo.clone(),
            name: None,
            create_missing: false,
            host: Some("testhost".into()),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(15), |evs| {
        find_ack(evs, 1).is_some()
            && evs.iter().any(|e| {
                matches!(e, ServerEvent::EntityUpserted { entity: Entity::Worktree(w) } if w.is_main)
            })
    })
    .await;
    assert!(
        matches!(find_ack(&events, 1), Some(ServerEvent::Ack { .. })),
        "{events:#?}"
    );
    let project = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Project(p),
            } => Some(p.clone()),
            _ => None,
        })
        .expect("mirrored project upsert");
    assert_eq!(project.host.as_deref(), Some("testhost"));
    // The host reports its own spelling of the root (macOS: /private/var
    // for /var); the same directory either way.
    assert_eq!(project.repo_path.file_name(), repo.file_name());
    let main_worktree = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Worktree(w),
            } if w.is_main => Some(w.clone()),
            _ => None,
        })
        .unwrap();

    // Collect's ownership check must reach the source daemon and correlate
    // its response, rather than consult the viewer's cached mirror.
    write_frame(
        &mut c,
        &ClientRequest::InspectCollect {
            req_id: 80,
            worktree: main_worktree.id.clone(),
            branch: "main".into(),
        },
    )
    .await
    .unwrap();
    let inspected = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 80).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&inspected, 80), Some(ServerEvent::Ack { .. })),
        "{inspected:?}"
    );

    // A terminal on the mirrored worktree: created by the host daemon,
    // attached through ours, echoing through two daemons and a pipe.
    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id: 2,
            worktree: main_worktree.id.clone(),
            name: Some("relayed".into()),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let ServerEvent::Ack {
        created: Some(EntityId::Terminal(term_id)),
        ..
    } = find_ack(&events, 2).unwrap()
    else {
        panic!("CreateTerminal over the relay failed: {events:#?}");
    };
    let term_id = term_id.clone();
    write_frame(
        &mut c,
        &ClientRequest::InspectCollect {
            req_id: 81,
            worktree: main_worktree.id.clone(),
            branch: "main".into(),
        },
    )
    .await
    .unwrap();
    let inspected = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        find_ack(evs, 81).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&inspected, 81), Some(ServerEvent::Error { message, .. }) if message.contains("source has busy")),
        "{inspected:?}"
    );
    let sref = SessionRef::Terminal(term_id.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    let marker = "relay_marker_7731";
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: sref.clone(),
            data: format!("echo {marker}\n").into_bytes(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        let text = String::from_utf8_lossy(&collected_output(evs)).into_owned();
        text.matches(marker).count() >= 2
    })
    .await;
    assert!(!events.is_empty());

    // The viewer goes away (laptop closed). The session is the host's and
    // stays alive.
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    let mut h = connect(&host_env.sock()).await;
    handshake(&mut h).await;
    write_frame(&mut h, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut h, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let alive = events.iter().any(|e| {
        matches!(e, ServerEvent::Snapshot { terminals, .. }
            if terminals.iter().any(|t| t.id == term_id && t.alive))
    });
    assert!(
        alive,
        "the host keeps the session after the viewer's daemon exits: {events:#?}"
    );

    // Back (laptop open): a fresh daemon mirrors the same rows and an
    // attach replays the ring — the marker is still on screen.
    let mut daemon = env.spawn_daemon_with("/bin/sh", &relay_env);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(15), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Terminal(t) } if t.id == term_id)
                || matches!(e, ServerEvent::Snapshot { terminals, .. } if terminals.iter().any(|t| t.id == term_id))
        })
    })
    .await;
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: sref.clone(),
            from_seq: None,
            cols: 80,
            rows: 24,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        String::from_utf8_lossy(&collected_output(evs)).contains(marker)
    })
    .await;
    assert!(!events.is_empty());

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    write_frame(&mut h, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut host_daemon);
}

/// Relays follow the anchors: a second `host:/path` added while the link
/// is up gets mirrored without a restart, and removing a mirrored project
/// row detaches the anchor here — the host keeps its project.
#[tokio::test]
async fn relay_follows_anchor_adds_and_removes() {
    let host_env = TestEnv::new();
    let repo_a = host_env.make_repo();
    let repo_b = host_env.tmp.path().join("repo-b");
    std::fs::create_dir_all(&repo_b).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.email=e@x",
            "-c",
            "user.name=n",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repo_b)
            .args(&args)
            .status()
            .unwrap()
            .success());
    }
    let mut host_daemon = host_env.spawn_daemon();
    let proxy = format!(
        "NEBULA_RUNTIME_DIR='{}' NEBULA_DATA_DIR='{}' '{}' proxy",
        host_env.runtime_dir.display(),
        host_env.tmp.path().join("data").display(),
        env!("CARGO_BIN_EXE_nebula")
    );
    let env = TestEnv::new();
    let relay_env = [("NEBULA_RELAY_CMD", proxy.as_str())];
    let mut daemon = env.spawn_daemon_with("/bin/sh", &relay_env);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    let add = |req_id: u64, path: &PathBuf| ClientRequest::AddProject {
        req_id,
        path: path.clone(),
        name: None,
        create_missing: false,
        host: Some("testhost".into()),
    };
    let mirrored_named = |evs: &[ServerEvent], name: &str| {
        evs.iter().find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Project(p),
            } if p.name == name && p.host.is_some() => Some(p.clone()),
            _ => None,
        })
    };
    write_frame(&mut c, &add(1, &repo_a)).await.unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(15), |evs| {
        mirrored_named(evs, "repo").is_some()
    })
    .await;
    assert!(mirrored_named(&events, "repo").is_some());

    // Second anchor on the same host, link already up.
    write_frame(&mut c, &add(2, &repo_b)).await.unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(15), |evs| {
        mirrored_named(evs, "repo-b").is_some()
    })
    .await;
    let b = mirrored_named(&events, "repo-b").unwrap();

    // Removing the mirrored row detaches the anchor: its rows leave, the
    // host still has the project.
    write_frame(
        &mut c,
        &ClientRequest::RemoveProject {
            req_id: 3,
            id: b.id.clone(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        find_ack(evs, 3).is_some()
            && evs.iter().any(|e| matches!(e, ServerEvent::EntityRemoved { id: EntityId::Project(p) } if *p == b.id))
    })
    .await;
    assert!(
        matches!(find_ack(&events, 3), Some(ServerEvent::Ack { .. })),
        "{events:#?}"
    );
    let mut h = connect(&host_env.sock()).await;
    handshake(&mut h).await;
    write_frame(&mut h, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let events = read_events_until(&mut h, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { projects, .. }
            if projects.iter().any(|p| p.id == b.id))),
        "the host keeps its project: {events:#?}"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
    write_frame(&mut h, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut host_daemon);
}

/// A remote project whose host ssh can't reach: the anchor row is stored
/// (the relay keeps retrying in the background), nothing is created on
/// this machine, and the panels show no phantom rows for it.
#[tokio::test]
async fn remote_add_project_unreachable_host_is_an_anchor_only() {
    let env = TestEnv::new();
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    let remote_path = env.tmp.path().join("never-created-here");
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 1,
            path: remote_path.clone(),
            name: None,
            create_missing: true,
            host: Some("nobody@nebula-e2e.invalid".into()),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(30), |evs| {
        find_ack(evs, 1).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, 1), Some(ServerEvent::Ack { .. })),
        "{events:#?}"
    );
    assert!(
        !remote_path.exists(),
        "create_missing never applies to a remote path"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            ServerEvent::EntityUpserted {
                entity: Entity::Project(_)
            }
        )),
        "no local project row is announced for a remote anchor"
    );

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// resolves the path against its own cwd (the daemon's differs), registers
/// the repo over IPC, and surfaces daemon rejections as nonzero exits.
#[tokio::test]
async fn cli_add_project() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;

    let run_cli = |args: &[&str], cwd: &Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .current_dir(cwd)
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .output()
            .unwrap()
    };

    // `nebula add .` from inside the repo: cwd-relative resolution, project
    // named after the directory.
    let out = run_cli(&["add", "."], &repo);
    assert!(out.status.success(), "add . failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("added project"), "stdout: {stdout}");
    let canon = repo.canonicalize().unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Project(p) }
                if p.repo_path == canon && p.name == "repo")
        })
    })
    .await;

    // The same repo again: the daemon's dedupe comes back as a failure.
    let out = run_cli(&["add", repo.to_str().unwrap()], env.tmp.path());
    assert!(!out.status.success(), "duplicate add must fail: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already added"), "stderr: {stderr}");

    // Dedupe is per-workspace: the same repo is welcome in a second
    // workspace (workspaces are free-form groupings the user curates).
    write_frame(
        &mut c,
        &ClientRequest::AddWorkspace {
            req_id: 91,
            name: "second".into(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Ack { req_id: 91, .. }))
    })
    .await;
    let ws_id = events
        .iter()
        .find_map(|e| match e {
            ServerEvent::Ack {
                req_id: 91,
                created: Some(EntityId::Workspace(id)),
            } => Some(id.clone()),
            _ => None,
        })
        .expect("AddWorkspace ack carries the new workspace id");
    write_frame(
        &mut c,
        &ClientRequest::OpenWorkspace {
            req_id: 92,
            id: ws_id,
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter()
            .any(|e| matches!(e, ServerEvent::Ack { req_id: 92, .. }))
    })
    .await;
    let out = run_cli(&["add", repo.to_str().unwrap()], env.tmp.path());
    assert!(
        out.status.success(),
        "same repo in another workspace must succeed: {out:?}"
    );
    // …but only once per workspace.
    let out = run_cli(&["add", repo.to_str().unwrap()], env.tmp.path());
    assert!(
        !out.status.success(),
        "duplicate within the second workspace must fail: {out:?}"
    );

    // Bare `nebula <dir>` shorthand on a second repo.
    let repo2 = env.tmp.path().join("repo2");
    std::fs::create_dir_all(&repo2).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo2)
            .args(&args)
            .env("GIT_AUTHOR_NAME", "nebula-test")
            .env("GIT_AUTHOR_EMAIL", "test@nebula.dev")
            .env("GIT_COMMITTER_NAME", "nebula-test")
            .env("GIT_COMMITTER_EMAIL", "test@nebula.dev")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }
    let out = run_cli(&[repo2.to_str().unwrap()], env.tmp.path());
    assert!(out.status.success(), "bare add failed: {out:?}");
    let canon2 = repo2.canonicalize().unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Project(p) }
                if p.repo_path == canon2 && p.name == "repo2")
        })
    })
    .await;

    // A directory that isn't a git repo is rejected by the daemon.
    let plain = env.tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let out = run_cli(&["add", plain.to_str().unwrap()], env.tmp.path());
    assert!(!out.status.success(), "non-repo add must fail: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not a git repository"), "stderr: {stderr}");

    // A path that doesn't exist fails client-side, before any IPC.
    let out = run_cli(&["add", "does-not-exist"], env.tmp.path());
    assert!(!out.status.success(), "missing dir must fail: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not exist"), "stderr: {stderr}");

    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// The real source daemon must correlate a handoff with its new hook turn,
/// reject stale revisions, and preserve the PTY on feedback and timeout.
#[tokio::test]
async fn collect_guarded_handoff_uses_new_source_turn_and_preserves_sessions() {
    use nebula_core::AgentStatus;
    use std::os::unix::fs::PermissionsExt;
    for mode in ["complete", "feedback", "no-hook"] {
        let env = TestEnv::new();
        let repo = env.make_repo();
        let script = env.tmp.path().join("collect-agent.sh");
        std::fs::write(&script, r##"#!/bin/sh
post() {
  curl -sf -o /dev/null -X POST -H "Authorization: Bearer $NEBULA_API_TOKEN" -H 'Content-Type: application/json' \
    -d '{"session_id":"collect-fixture"}' "$NEBULA_API_URL/api/hooks/claude?agentId=$NEBULA_AGENT_ID&hookEvent=$1"
}
post Stop
while IFS= read -r line; do
  case "$line" in
    *no-hook*) continue ;;
    *feedback*) post UserPromptSubmit; post PermissionRequest ;;
    *) printf '\033]9;4;1;0\007'; sleep 0.1; post UserPromptSubmit; post Stop ;;
  esac
done
"##).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());
        let mut c = connect(&env.sock()).await;
        handshake(&mut c).await;
        let wt = add_project_get_main_worktree(&mut c, &repo).await;
        let id = create_agent_get_id(&mut c, &wt.id, "collect-owner", 2).await;
        let owner = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                write_frame(&mut c, &ClientRequest::Subscribe)
                    .await
                    .unwrap();
                let events = read_events_until(&mut c, Duration::from_secs(2), |evs| {
                    evs.iter()
                        .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
                })
                .await;
                let found = events.into_iter().find_map(|e| match e {
                    ServerEvent::Snapshot { agents, .. } => agents
                        .into_iter()
                        .find(|a| a.id == id && a.status == AgentStatus::Finished),
                    _ => None,
                });
                if let Some(a) = found {
                    break a;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("fake source owner never settled");
        // A stale mirrored revision must refuse without sending any prompt.
        write_frame(
            &mut c,
            &ClientRequest::CollectHandoff {
                req_id: 10,
                id: id.clone(),
                worktree: wt.id.clone(),
                branch: "main".into(),
                expected_status_changed_at: owner.status_changed_at - 1,
                prompt: "complete".into(),
                timeout_ms: 500,
            },
        )
        .await
        .unwrap();
        let stale = read_events_until(&mut c, Duration::from_secs(2), |evs| {
            evs.iter().any(|e| {
                matches!(
                    e,
                    ServerEvent::Error {
                        req_id: Some(10),
                        ..
                    } | ServerEvent::Ack { req_id: 10, .. }
                )
            })
        })
        .await;
        assert!(
            stale.iter().any(|e| matches!(
                e,
                ServerEvent::Error {
                    req_id: Some(10),
                    ..
                }
            )),
            "{stale:?}"
        );
        write_frame(
            &mut c,
            &ClientRequest::CollectHandoff {
                req_id: 11,
                id: id.clone(),
                worktree: wt.id.clone(),
                branch: "main".into(),
                expected_status_changed_at: owner.status_changed_at,
                prompt: mode.into(),
                timeout_ms: 1000,
            },
        )
        .await
        .unwrap();
        let result = read_events_until(&mut c, Duration::from_secs(3), |evs| {
            evs.iter().any(|e| {
                matches!(
                    e,
                    ServerEvent::Error {
                        req_id: Some(11),
                        ..
                    } | ServerEvent::Ack { req_id: 11, .. }
                )
            })
        })
        .await;
        assert_eq!(
            matches!(find_ack(&result, 11), Some(ServerEvent::Ack { .. })),
            mode == "complete",
            "{mode}: {result:?}"
        );
        write_frame(&mut c, &ClientRequest::Subscribe)
            .await
            .unwrap();
        let after = read_events_until(&mut c, Duration::from_secs(2), |evs| {
            evs.iter()
                .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
        })
        .await;
        assert!(
            after.iter().any(
                |e| matches!(e, ServerEvent::Snapshot { agents, worktrees, .. } if
            agents.iter().any(|a| a.id == id && a.alive && !a.archived && a.worktree_id == wt.id)
            && worktrees.iter().any(|w| w.id == wt.id))
            ),
            "{after:?}"
        );
        // This is only the isolated fixture daemon, never the user's daemon.
        write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
        wait_for_exit(&mut child);
    }
}

/// Real CLI -> daemon -> Git -> PTY regression. Every path and process is disposable.
#[tokio::test]
async fn branch_sessions_keep_independent_execution_directories() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["branch", "other"]);
    git(&["checkout", "-b", "feature"]);
    commit_file(&repo, "README.md", "feature content\n");
    std::fs::write(repo.join("dirty.txt"), "keep me").unwrap();
    let head = git(&["rev-parse", "HEAD"]);
    let _daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 800,
            path: repo.clone(),
            name: Some("isolation".into()),
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let evs = read_events_until(&mut c, Duration::from_secs(10), |e| {
        find_ack(e, 800).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&evs, 800), Some(ServerEvent::Ack { .. })),
        "{evs:?}"
    );
    let Some(ServerEvent::Ack {
        created: Some(EntityId::Project(project)),
        ..
    }) = find_ack(&evs, 800)
    else {
        panic!("{evs:?}")
    };
    let project = project.clone();
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |e| {
        e.iter().any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let new_agent = |branch: &str, name: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args([
                "agent",
                "new",
                "--project",
                "isolation",
                "--worktree",
                branch,
                "--name",
                name,
            ])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .env_remove("NEBULA_AGENT_ID")
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "agent on {branch}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        SessionRef::Agent(nebula_core::AgentId(json["id"].as_str().unwrap().into()))
    };
    // Independent directories require explicit worktree intent.
    let explicit = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["switch", "main", "--worktree", "--project", "isolation"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
        .env_remove("NEBULA_AGENT_ID")
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let feature = new_agent("primary", "feature-worker");
    let main = new_agent("main", "main-worker");
    assert_eq!(git(&["rev-parse", "HEAD"]), head);
    assert_eq!(git(&["branch", "--show-current"]), "feature");
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "feature content\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("dirty.txt")).unwrap(),
        "keep me"
    );
    for (session, expected, file) in [
        (&feature, "feature content", "feature-write"),
        (&main, "# test", "main-write"),
    ] {
        write_frame(
            &mut c,
            &ClientRequest::Attach {
                session: session.clone(),
                from_seq: None,
                cols: 100,
                rows: 24,
            },
        )
        .await
        .unwrap();
        write_frame(&mut c, &ClientRequest::Input {
            session: session.clone(),
            data: format!("cat README.md; echo own > {file}; git add {file}; git commit -m {file}; printf 'done-%s\\n' {file}\n").into_bytes(),
        }).await.unwrap();
        let marker = format!("done-{file}");
        let evs = read_events_until(&mut c, Duration::from_secs(10), |e| {
            String::from_utf8_lossy(&collected_output(e)).contains(&marker)
        })
        .await;
        assert!(
            String::from_utf8_lossy(&collected_output(&evs)).contains(expected),
            "{evs:?}"
        );
        write_frame(
            &mut c,
            &ClientRequest::Detach {
                session: session.clone(),
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(git(&["show", "feature:feature-write"]), "own");
    assert_eq!(git(&["show", "main:main-write"]), "own");
    assert!(!repo.join("main-write").exists());
    let count = git(&["worktree", "list", "--porcelain"])
        .matches("worktree ")
        .count();
    new_agent("main", "another-main-worker");
    assert_eq!(
        git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        count
    );

    // Status never permits moving files under a live process, including a finished agent.
    for (i, event, status) in [
        (0, "UserPromptSubmit", nebula_core::AgentStatus::Running),
        (
            1,
            "PermissionRequest",
            nebula_core::AgentStatus::NeedsFeedback,
        ),
        (2, "Stop", nebula_core::AgentStatus::Finished),
    ] {
        let curl = format!("curl -sS -m 3 -X POST -H \"Authorization: Bearer $NEBULA_API_TOKEN\" -H 'Content-Type: application/json' -d '{{}}' \"$NEBULA_API_URL/api/hooks/claude?agentId=$NEBULA_AGENT_ID&hookEvent={event}\"\n");
        write_frame(
            &mut c,
            &ClientRequest::Input {
                session: feature.clone(),
                data: curl.into_bytes(),
            },
        )
        .await
        .unwrap();
        let SessionRef::Agent(feature_id) = &feature else {
            panic!()
        };
        let events = read_events_until(&mut c, Duration::from_secs(10), |e| e.iter().any(|e| matches!(e, ServerEvent::StatusChanged { agent, status: s, .. } if agent == feature_id && *s == status))).await;
        assert!(events.iter().any(|e| matches!(e, ServerEvent::StatusChanged { agent, status: s, .. } if agent == feature_id && *s == status)));
        new_agent("main", &format!("main-at-status-{i}"));
        write_frame(
            &mut c,
            &ClientRequest::ResolveBranchCheckout {
                req_id: 810 + i,
                project: project.clone(),
                branch: "other".into(),
            },
        )
        .await
        .unwrap();
        let evs = read_events_until(&mut c, Duration::from_secs(10), |e| {
            find_ack(e, 810 + i).is_some()
        })
        .await;
        let refusal = format!(
            "primary has sessions on \"feature\"; switching to \"other\" stops feature-worker (agent, {})",
            status.as_str().replace('_', " ")
        );
        assert!(
            matches!(find_ack(&evs, 810 + i), Some(ServerEvent::Error { message, .. }) if *message == refusal),
            "must protect {status:?}: {evs:?}"
        );
        assert_eq!(git(&["branch", "--show-current"]), "feature");
    }
    // Simulate a legacy/external Git switch only in this disposable fixture.
    // A restart error must leave the existing PTY intact, while stopped resume refuses.
    git(&["checkout", "other"]);
    let SessionRef::Agent(feature_id) = &feature else {
        panic!()
    };
    write_frame(
        &mut c,
        &ClientRequest::RestartAgent {
            req_id: 820,
            id: feature_id.clone(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |e| {
        find_ack(e, 820).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, 820), Some(ServerEvent::Error { message, .. }) if message.contains("expects branch \"feature\"")),
        "{events:?}"
    );
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: feature.clone(),
            from_seq: None,
            cols: 100,
            rows: 24,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: feature.clone(),
            data: b"printf 'still-%s\n' alive\n".to_vec(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |e| {
        String::from_utf8_lossy(&collected_output(e)).contains("still-alive")
    })
    .await;
    assert!(String::from_utf8_lossy(&collected_output(&events)).contains("still-alive"));
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
}

/// Branch intent must never turn a parked branch or unsafe primary into a cd to a WT.
#[tokio::test]
async fn branch_primary_cli_switch_and_agent_destinations() {
    let env = TestEnv::new();
    let repo = env.make_repo().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["checkout", "-b", "feature"]);
    commit_file(&repo, "README.md", "feature files\n");
    git(&["checkout", "main"]);
    git(&["branch", "agent-branch", "feature"]);
    git(&["branch", "busy-target"]);
    let _daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    write_frame(
        &mut c,
        &ClientRequest::AddProject {
            req_id: 900,
            path: repo.clone(),
            name: Some("primary-policy".into()),
            create_missing: false,
            host: None,
        },
    )
    .await
    .unwrap();
    let evs = read_events_until(&mut c, Duration::from_secs(10), |e| {
        find_ack(e, 900).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&evs, 900), Some(ServerEvent::Ack { .. })),
        "{evs:?}"
    );
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .args(["--project", "primary-policy"])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .env_remove("NEBULA_AGENT_ID")
            .current_dir(&repo)
            .output()
            .unwrap()
    };
    let success = |out: std::process::Output| {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let refusal = |out: std::process::Output| {
        assert!(
            !out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(out.stdout.is_empty(), "refusal must not emit a destination");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("cd '"));
    };
    // Conflicting tracked edits: no directory, row, HEAD change or cd.
    std::fs::write(repo.join("README.md"), "dirty primary\n").unwrap();
    let dirty = cli(&["switch", "feature"]);
    assert!(
        String::from_utf8_lossy(&dirty.stderr).contains(
            "primary has 1 uncommitted file on \"main\"; commit them before switching to \"feature\""
        ),
        "{}",
        String::from_utf8_lossy(&dirty.stderr)
    );
    refusal(dirty);
    refusal(cli(&[
        "agent",
        "new",
        "--worktree",
        "agent-branch",
        "--name",
        "dirty",
    ]));
    assert_eq!(git(&["branch", "--show-current"]), "main");
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "dirty primary\n"
    );
    assert_eq!(
        git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    assert!(!repo.with_file_name("repo-worktrees").exists());
    git(&["restore", "README.md"]);
    let switched = success(cli(&["switch", "feature"]));
    assert_eq!(switched["path"], repo.to_str().unwrap());
    assert_eq!(switched["checked_out_in_primary"], true);
    assert_eq!(git(&["branch", "--show-current"]), "feature");
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "feature files\n"
    );
    success(cli(&["switch", "main"]));
    let linked = success(cli(&["switch", "feature", "--worktree"]));
    let linked_path = linked["path"].as_str().unwrap();
    assert_ne!(linked_path, repo.to_str().unwrap());
    refusal(cli(&["switch", "feature"]));
    assert_eq!(git(&["branch", "--show-current"]), "main");
    assert_eq!(
        success(cli(&["switch", "feature", "--worktree"]))["path"],
        linked_path
    );
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let evs = read_events_until(&mut c, Duration::from_secs(5), |e| {
        e.iter().any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    let linked_id = evs
        .iter()
        .find_map(|e| match e {
            ServerEvent::Snapshot { worktrees, .. } => worktrees
                .iter()
                .find(|w| !w.is_main && w.branch == "feature")
                .map(|w| w.id.clone()),
            _ => None,
        })
        .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            req_id: 903,
            worktree: linked_id,
            expected_branch: None,
            name: None,
        },
    )
    .await
    .unwrap();
    let created = read_events_until(&mut c, Duration::from_secs(5), |e| {
        find_ack(e, 903).is_some()
    })
    .await;
    let Some(ServerEvent::Ack {
        created: Some(EntityId::Terminal(linked_tab)),
        ..
    }) = find_ack(&created, 903)
    else {
        panic!("{created:?}")
    };
    let linked_tab = linked_tab.clone();
    for target in ["busy-target", "main"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args([
                "worktree",
                "checkout",
                target,
                "--project",
                "primary-policy",
            ])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .env("NEBULA_AGENT_ID", format!("term:{linked_tab}"))
            .current_dir(linked_path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "a caller in a linked checkout: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(git(&["branch", "--show-current"]), target);
    }
    write_frame(
        &mut c,
        &ClientRequest::CloseTerminal {
            req_id: 904,
            id: linked_tab,
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |e| {
        find_ack(e, 904).is_some()
    })
    .await;
    // Missing --worktree selector means branch intent, not permission to create a WT.
    let agent = success(cli(&[
        "agent",
        "new",
        "--worktree",
        "agent-branch",
        "--name",
        "primary-agent",
    ]));
    assert_eq!(git(&["branch", "--show-current"]), "agent-branch");
    assert_eq!(
        git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        2
    );
    let session = SessionRef::Agent(nebula_core::AgentId(agent["id"].as_str().unwrap().into()));
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: session.clone(),
            from_seq: None,
            cols: 100,
            rows: 24,
        },
    )
    .await
    .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: session.clone(),
            data: b"pwd > actual-cwd; cat README.md > actual-files; printf 'cwd-%s\\n' ready\n"
                .to_vec(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |e| {
        String::from_utf8_lossy(&collected_output(e)).contains("cwd-ready")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(repo.join("actual-cwd"))
            .unwrap()
            .trim(),
        repo.to_str().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("actual-files")).unwrap(),
        "feature files\n"
    );
    let SessionRef::Agent(agent_id) = &session else {
        unreachable!()
    };
    let refusal_text = "primary has sessions on \"agent-branch\"; switching to \"busy-target\" stops primary-agent (agent, fresh)";
    for (args, hint) in [
        (
            &["switch", "busy-target"][..],
            "rerun with --stop-busy to stop them and switch",
        ),
        (
            &["worktree", "checkout", "busy-target"][..],
            "rerun with --stop-busy to stop them and switch",
        ),
        (
            &[
                "agent",
                "new",
                "--worktree",
                "busy-target",
                "--name",
                "blocked",
            ][..],
            "run `nebula switch 'busy-target' --project 'primary-policy' --stop-busy` first, then retry",
        ),
    ] {
        let busy = cli(args);
        assert_eq!(
            String::from_utf8_lossy(&busy.stderr).trim(),
            format!("Error: {refusal_text}; {hint}")
        );
        refusal(busy);
    }
    assert_eq!(git(&["branch", "--show-current"]), "agent-branch");
    success(cli(&["switch", "busy-target", "--stop-busy"]));
    assert_eq!(git(&["branch", "--show-current"]), "busy-target");
    assert_eq!(
        git(&["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        2
    );
    write_frame(&mut c, &ClientRequest::Subscribe)
        .await
        .unwrap();
    let evs = read_events_until(&mut c, Duration::from_secs(5), |e| {
        e.iter().any(|e| matches!(e, ServerEvent::Snapshot { .. }))
    })
    .await;
    assert!(evs
        .iter()
        .any(|e| matches!(e, ServerEvent::Snapshot { worktrees, .. } if worktrees.len() == 2)));
    let stopped = evs
        .iter()
        .find_map(|e| match e {
            ServerEvent::Snapshot { agents, .. } => agents
                .iter()
                .find(|a| SessionRef::Agent(a.id.clone()) == session),
            _ => None,
        })
        .expect("the stopped agent keeps its row");
    assert!(
        !stopped.alive,
        "the confirmed switch stopped the busy agent"
    );
    assert_eq!(stopped.branch.as_deref(), Some("agent-branch"));
    success(cli(&["switch", "agent-branch"]));
    let upserted = |evs: &[ServerEvent]| {
        evs.iter().rev().find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(a),
            } if a.id == *agent_id => Some((a.status, a.alive)),
            _ => None,
        })
    };
    write_frame(
        &mut c,
        &ClientRequest::RestartAgent {
            req_id: 905,
            id: agent_id.clone(),
        },
    )
    .await
    .unwrap();
    let restarted = read_events_until(&mut c, Duration::from_secs(5), |e| {
        upserted(e).is_some_and(|(_, alive)| alive)
    })
    .await;
    assert_eq!(
        upserted(&restarted),
        Some((nebula_core::AgentStatus::Fresh, true)),
        "restart"
    );
    success(cli(&["switch", "busy-target", "--stop-busy"]));
    success(cli(&["switch", "agent-branch"]));
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: session.clone(),
            from_seq: None,
            cols: 100,
            rows: 24,
        },
    )
    .await
    .unwrap();
    let resumed = read_events_until(&mut c, Duration::from_secs(5), |e| {
        upserted(e).is_some_and(|(_, alive)| alive)
    })
    .await;
    assert_eq!(
        upserted(&resumed),
        Some((nebula_core::AgentStatus::Fresh, true)),
        "resume on attach"
    );
    // A real terminal must remain in primary: failed branch switches queue no cd.
    let primary = evs
        .iter()
        .find_map(|e| match e {
            ServerEvent::Snapshot { worktrees, .. } => worktrees.iter().find(|w| w.is_main),
            _ => None,
        })
        .unwrap();
    write_frame(
        &mut c,
        &ClientRequest::CreateTerminal {
            req_id: 902,
            worktree: primary.id.clone(),
            expected_branch: Some("agent-branch".into()),
            name: None,
        },
    )
    .await
    .unwrap();
    let created = read_events_until(&mut c, Duration::from_secs(5), |e| {
        find_ack(e, 902).is_some()
    })
    .await;
    let Some(ServerEvent::Ack {
        created: Some(EntityId::Terminal(terminal)),
        ..
    }) = find_ack(&created, 902)
    else {
        panic!("{created:?}")
    };
    let shell = SessionRef::Terminal(terminal.clone());
    write_frame(
        &mut c,
        &ClientRequest::Attach {
            session: shell.clone(),
            from_seq: None,
            cols: 100,
            rows: 24,
        },
    )
    .await
    .unwrap();
    let failed = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args(["switch", "feature", "--project", "primary-policy"])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
        .env("NEBULA_AGENT_ID", format!("term:{terminal}"))
        .current_dir(&repo)
        .output()
        .unwrap();
    refusal(failed);
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: shell,
            data: format!(
                "pwd > '{}'; printf 'shell-%s\\n' checked\n",
                repo.join("shell-cwd").display()
            )
            .into_bytes(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(10), |e| {
        String::from_utf8_lossy(&collected_output(e)).contains("shell-checked")
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(repo.join("shell-cwd"))
            .unwrap()
            .trim(),
        repo.to_str().unwrap()
    );
    for target in ["busy-target", "agent-branch"] {
        let moved = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args([
                "worktree",
                "checkout",
                target,
                "--project",
                "primary-policy",
                "--stop-busy",
            ])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .env("NEBULA_AGENT_ID", format!("term:{terminal}"))
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            moved.status.success(),
            "{}",
            String::from_utf8_lossy(&moved.stderr)
        );
        assert_eq!(git(&["branch", "--show-current"]), target);
        write_frame(&mut c, &ClientRequest::Subscribe)
            .await
            .unwrap();
        let evs = read_events_until(&mut c, Duration::from_secs(5), |e| {
            e.iter().any(|e| matches!(e, ServerEvent::Snapshot { .. }))
        })
        .await;
        let caller = evs
            .iter()
            .find_map(|e| match e {
                ServerEvent::Snapshot { terminals, .. } => {
                    terminals.iter().find(|t| &t.id == terminal)
                }
                _ => None,
            })
            .unwrap();
        assert!(
            caller.alive,
            "worktree checkout stopped the shell that ran it"
        );
    }
    // Retained session history must not make a vanished checkout a cd target.
    let parked_agent = success(cli(&[
        "agent",
        "new",
        "--worktree",
        "feature",
        "--name",
        "parked",
    ]));
    let parked_session = SessionRef::Agent(nebula_core::AgentId(
        parked_agent["id"].as_str().unwrap().into(),
    ));
    write_frame(
        &mut c,
        &ClientRequest::Input {
            session: parked_session,
            data: b"exit\n".to_vec(),
        },
    )
    .await
    .unwrap();
    std::fs::rename(linked_path, repo.with_file_name("preserved-checkout")).unwrap();
    refusal(cli(&["switch", "feature", "--worktree"]));
    refusal(cli(&["switch", "feature"]));
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
}

#[tokio::test]
async fn a_parked_agent_resumes_its_conversation_once_primary_is_back_on_its_branch() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let repo = env.make_repo().canonicalize().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    git(&["branch", "other"]);
    let bin = env.tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let spawns = env.tmp.path().join("claude-spawns");
    let stub = bin.join("claude");
    std::fs::write(
        &stub,
        format!(
            concat!(
                "#!/bin/sh\n",
                "echo \"$*\" >> '{}'\n",
                "curl -sS -m 3 -X POST -H \"Authorization: Bearer $NEBULA_API_TOKEN\" \\\n",
                "  -H 'Content-Type: application/json' -d '{{\"session_id\":\"sid-parked\"}}' \\\n",
                "  \"$NEBULA_API_URL/api/hooks/claude?agentId=$NEBULA_AGENT_ID&hookEvent=SessionStart\" \\\n",
                "  >/dev/null 2>&1\n",
                "exec sleep 60\n",
            ),
            spawns.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shell = env.tmp.path().join("seeing-shell.sh");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\nPATH={}:/usr/bin:/bin\nexport PATH\nexec /bin/sh -c \"$4\"\n",
            bin.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _daemon = env.spawn_daemon_with_shell(&shell);
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let primary = add_project_get_main_worktree(&mut c, &repo).await;

    write_frame(
        &mut c,
        &ClientRequest::CreateAgent {
            expected_branch: Some("main".into()),
            req_id: 2,
            worktree: primary.id.clone(),
            name: "parked".into(),
            kind: AgentKind::Claude,
            model: None,
            effort: None,
            auto_title: false,
            prompt: None,
            writer: WriterClaim::Off,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        find_ack(evs, 2).is_some()
    })
    .await;
    let Some(ServerEvent::Ack {
        created: Some(EntityId::Agent(agent_id)),
        ..
    }) = find_ack(&events, 2)
    else {
        panic!("CreateAgent failed: {events:#?}");
    };
    let agent_id = agent_id.clone();
    let agent_row = |evs: &[ServerEvent]| {
        evs.iter().rev().find_map(|e| match e {
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(a),
            } if a.id == agent_id => Some(a.clone()),
            _ => None,
        })
    };
    let mut stored = None;
    for _ in 0..50 {
        let mut snap = connect(&env.sock()).await;
        handshake(&mut snap).await;
        write_frame(&mut snap, &ClientRequest::Subscribe)
            .await
            .unwrap();
        let evs = read_events_until(&mut snap, Duration::from_secs(5), |evs| {
            evs.iter()
                .any(|e| matches!(e, ServerEvent::Snapshot { .. }))
        })
        .await;
        stored = evs.iter().find_map(|e| match e {
            ServerEvent::Snapshot { agents, .. } => agents
                .iter()
                .find(|a| a.id == agent_id)
                .and_then(|a| a.session_id.clone()),
            _ => None,
        });
        if stored.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(stored.as_deref(), Some("sid-parked"));

    let mut req_id = 10;
    let mut checkout = async |c: &mut UnixStream, branch: &str, stop_busy: bool| {
        req_id += 1;
        write_frame(
            c,
            &ClientRequest::CheckoutPrimary {
                req_id,
                project: primary.project_id.clone(),
                branch: branch.into(),
                mover: None,
                stop_busy,
            },
        )
        .await
        .unwrap();
        let evs = read_events_until(c, Duration::from_secs(10), |e| {
            find_ack(e, req_id).is_some()
        })
        .await;
        assert!(
            matches!(find_ack(&evs, req_id), Some(ServerEvent::Ack { .. })),
            "switch to {branch}: {evs:?}"
        );
        evs
    };
    let parked = checkout(&mut c, "other", true).await;
    let row = agent_row(&parked).expect("the parked agent row");
    assert!(!row.alive);
    assert_eq!(row.branch.as_deref(), Some("main"));

    let session = SessionRef::Agent(agent_id.clone());
    let attach = ClientRequest::Attach {
        session: session.clone(),
        from_seq: None,
        cols: 100,
        rows: 24,
    };
    write_frame(&mut c, &attach).await.unwrap();
    let refused = read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| matches!(e, ServerEvent::Error { .. }))
    })
    .await;
    assert!(
        refused
            .iter()
            .any(|e| matches!(e, ServerEvent::Error { message, .. }
            if message.starts_with("attach: session expects branch \"main\""))),
        "{refused:?}"
    );

    checkout(&mut c, "main", false).await;
    assert_eq!(git(&["branch", "--show-current"]), "main");
    write_frame(&mut c, &attach).await.unwrap();
    let resumed = read_events_until(&mut c, Duration::from_secs(10), |evs| {
        agent_row(evs).is_some_and(|a| a.alive)
    })
    .await;
    assert_eq!(agent_row(&resumed).unwrap().worktree_id, primary.id);
    let mut spawned = String::new();
    for _ in 0..50 {
        spawned = std::fs::read_to_string(&spawns).unwrap();
        if spawned.lines().count() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        spawned.lines().collect::<Vec<_>>(),
        [
            "--dangerously-skip-permissions",
            "--resume sid-parked --dangerously-skip-permissions"
        ]
    );
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
}

/// The SSH boundary runs locally; Git and the owning daemon remain real.
/// A projected host-only repo path catches accidental Git on the CLI machine.
#[tokio::test]
async fn branch_primary_remote_switch_uses_owner_and_rejects_stale_paths() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let repo = env.make_repo();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "feature"])
        .status()
        .unwrap()
        .success());
    let mut daemon = env.spawn_daemon();
    let mut control = connect(&env.sock()).await;
    handshake(&mut control).await;
    add_project_get_main_worktree(&mut control, &repo).await;
    let bin = env.tmp.path().join("transport-bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::write(
        bin.join("ssh"),
        "#!/bin/sh\nfor arg do script=$arg; done\nexec /bin/sh -c \"$script\"\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    for stale in [false, true] {
        let proxy_dir = env
            .tmp
            .path()
            .join(if stale { "stale-proxy" } else { "host-proxy" });
        std::fs::create_dir(&proxy_dir).unwrap();
        let listener = tokio::net::UnixListener::bind(proxy_dir.join("daemon.sock")).unwrap();
        let mut upstream = connect(&env.sock()).await;
        let missing = env.tmp.path().join("missing-checkout");
        let host_repo = env.tmp.path().join("host-only-repository");
        let proxy = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            while let Some(request) = read_frame::<ClientRequest, _>(&mut client).await.unwrap() {
                assert!(
                    !matches!(request, ClientRequest::Input { .. }),
                    "remote destination must not cd the local terminal"
                );
                write_frame(&mut upstream, &request).await.unwrap();
                let events = read_events_until(&mut upstream, Duration::from_secs(10), |events| {
                    match &request {
                        ClientRequest::Hello { .. } => events
                            .iter()
                            .any(|e| matches!(e, ServerEvent::HelloOk { .. })),
                        ClientRequest::Subscribe => events
                            .iter()
                            .any(|e| matches!(e, ServerEvent::Snapshot { .. })),
                        ClientRequest::CheckoutPrimary { req_id, .. } => {
                            find_ack(events, *req_id).is_some()
                        }
                        other => panic!("unexpected request {other:?}"),
                    }
                })
                .await;
                for mut event in events {
                    if let ServerEvent::Snapshot {
                        projects,
                        worktrees,
                        ..
                    } = &mut event
                    {
                        for p in projects {
                            p.host = Some("fixture-host".into());
                            p.repo_path = host_repo.clone();
                        }
                        if stale {
                            for w in worktrees {
                                w.path = missing.clone();
                            }
                        }
                    }
                    write_frame(&mut client, &event).await.unwrap();
                }
            }
        });
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_nebula"));
        command.args(["switch", "feature", "--project", "repo@fixture-host"]);
        if stale {
            command.arg("--worktree");
        }
        let output = command
            .env("NEBULA_RUNTIME_DIR", &proxy_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .env("NEBULA_AGENT_ID", "term:local-shell")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .current_dir(&repo)
            .output()
            .await
            .unwrap();
        proxy.await.unwrap();
        if stale {
            assert!(
                !output.status.success(),
                "stale remote row accepted: {output:?}"
            );
            assert!(output.stdout.is_empty());
        } else {
            assert!(output.status.success(), "{output:?}");
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["checked_out_in_primary"], true);
            assert_eq!(result["cd"], false);
            let head = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["branch", "--show-current"])
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "feature");
            assert!(!repo.with_file_name("repo-worktrees").exists());
        }
    }
    write_frame(&mut control, &ClientRequest::Shutdown)
        .await
        .unwrap();
    wait_for_exit(&mut daemon.0);
}

/// A protocol proxy orders real daemon operations around the CLI's snapshots.
/// No Git/session implementation is substituted: both switches and the spawn
/// request reach the real disposable daemon.
#[tokio::test]
async fn branch_session_cli_rejects_a_checkout_switched_after_resolution() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    for args in [&["branch", "other"][..], &["checkout", "-b", "feature"][..]] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    let _daemon = env.spawn_daemon();
    let mut control = connect(&env.sock()).await;
    handshake(&mut control).await;
    let primary = add_project_get_main_worktree(&mut control, &repo).await;
    let project = primary.project_id.clone();
    let proxy_dir = env.tmp.path().join("px");
    std::fs::create_dir(&proxy_dir).unwrap();
    let listener = tokio::net::UnixListener::bind(proxy_dir.join("daemon.sock")).unwrap();
    let mut upstream = connect(&env.sock()).await;
    let proxy =
        tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut resolved = false;
            loop {
                let request: ClientRequest = read_frame(&mut client).await.unwrap().unwrap();
                let switch = match &request {
                    ClientRequest::ResolveBranchCheckout { .. } => Some("main"),
                    ClientRequest::Subscribe if resolved => Some("other"),
                    _ => None,
                };
                if let Some(branch) = switch {
                    write_frame(
                        &mut control,
                        &ClientRequest::CheckoutPrimary {
                            req_id: 990,
                            project: project.clone(),
                            branch: branch.into(),
                            mover: None,
                            stop_busy: false,
                        },
                    )
                    .await
                    .unwrap();
                    let events = read_events_until(&mut control, Duration::from_secs(10), |e| {
                        find_ack(e, 990).is_some()
                    })
                    .await;
                    assert!(
                        matches!(find_ack(&events, 990), Some(ServerEvent::Ack { .. })),
                        "{events:?}"
                    );
                }
                write_frame(&mut upstream, &request).await.unwrap();
                let events = read_events_until(&mut upstream, Duration::from_secs(10), |events| {
                    match &request {
                        ClientRequest::Hello { .. } => events
                            .iter()
                            .any(|e| matches!(e, ServerEvent::HelloOk { .. })),
                        ClientRequest::Subscribe => events
                            .iter()
                            .any(|e| matches!(e, ServerEvent::Snapshot { .. })),
                        ClientRequest::ResolveBranchCheckout { req_id, .. }
                        | ClientRequest::CreateAgent { req_id, .. } => {
                            find_ack(events, *req_id).is_some()
                        }
                        other => panic!("unexpected CLI request: {other:?}"),
                    }
                })
                .await;
                for event in events {
                    write_frame(&mut client, &event).await.unwrap();
                }
                if matches!(request, ClientRequest::ResolveBranchCheckout { .. }) {
                    resolved = true;
                }
                if matches!(request, ClientRequest::CreateAgent { .. }) {
                    break;
                }
            }
            control
        });
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args([
            "agent",
            "new",
            "--project",
            "repo",
            "--worktree",
            "main",
            "--name",
            "race",
        ])
        .env("NEBULA_RUNTIME_DIR", &proxy_dir)
        .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
        .env_remove("NEBULA_AGENT_ID")
        .current_dir(&repo)
        .output()
        .await
        .unwrap();
    let mut control = proxy.await.unwrap();
    write_frame(&mut control, &ClientRequest::Shutdown)
        .await
        .unwrap();
    assert!(
        !output.status.success(),
        "a branch request must not become a physical selector: {output:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("expects branch \"main\""),
        "{output:?}"
    );
}

/// Fixtures only: no real agent, trust choice, user process or checkout.
#[tokio::test]
async fn mobile_keys_and_id_deletion_use_the_exact_pty() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let repo = env.make_repo();
    let capture = env.tmp.path().join("keys");
    let ready = env.tmp.path().join("ready");
    let script = env.tmp.path().join("recorder");
    std::fs::write(&script, format!("#!/usr/bin/python3\nimport os, tty\ntty.setraw(0)\nopen({:?}, 'w').close()\nwhile True:\n with open({:?}, 'ab', buffering=0) as f: f.write(os.read(0, 1024))\n", ready, capture)).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let main = add_project_get_main_worktree(&mut c, &repo).await;
    let id = create_agent_get_id(&mut c, &main.id, "fixture", 90).await;
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap()
    };
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready.exists());
    let result = cli(&[
        "agent",
        "key",
        "--project",
        "repo",
        "--",
        id.as_str(),
        "Space",
    ]);
    assert!(!result.status.success());
    for key in ["Up", "Down", "Left", "Right", "Enter", "Escape", "Tab"] {
        let result = cli(&["agent", "key", "--project", "repo", "--", id.as_str(), key]);
        assert!(result.status.success(), "key {key}: {result:?}");
    }
    let expected = b"\x1b[A\x1b[B\x1b[D\x1b[C\r\x1b\t";
    for _ in 0..100 {
        if std::fs::read(&capture).unwrap_or_default().len() >= expected.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read(&capture).unwrap(), expected);
    // A caller cannot use a valid ID to delete a session in another project.
    write_frame(
        &mut c,
        &ClientRequest::DeleteAgentScoped {
            req_id: 91,
            id: id.clone(),
            project: nebula_core::ProjectId("other-project".into()),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |events| {
        events.iter().any(|e| {
            matches!(
                e,
                ServerEvent::Error {
                    req_id: Some(91),
                    ..
                }
            )
        })
    })
    .await;
    write_frame(
        &mut c,
        &ClientRequest::RenameAgent {
            req_id: 92,
            id: id.clone(),
            name: "renamed fixture".into(),
        },
    )
    .await
    .unwrap();
    read_events_until(&mut c, Duration::from_secs(5), |events| {
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Ack { req_id: 92, .. }))
    })
    .await;
    let result = cli(&["agent", "delete-id", "--project", "repo", "--", id.as_str()]);
    assert!(result.status.success(), "{result:?}");
    let result = cli(&[
        "agent",
        "key",
        "--project",
        "repo",
        "--",
        id.as_str(),
        "Enter",
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("no longer available"));
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

#[tokio::test]
async fn mobile_safe_removal_preserves_dirty_unmerged_occupied_and_protected_checkouts() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let main = add_project_get_main_worktree(&mut c, &repo).await;
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap()
    };
    let result = cli(&["worktree", "new", "fixture", "--project", "repo"]);
    assert!(result.status.success(), "{result:?}");
    let result = cli(&["worktree", "list", "--project", "repo"]);
    let trees: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let tree = trees
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["branch"] == "fixture")
        .unwrap();
    let id = tree["id"].as_str().unwrap();
    let path = tree["path"].as_str().unwrap();
    let remove = || {
        cli(&[
            "worktree",
            "delete-safe",
            "--project",
            "repo",
            "--",
            id,
            "fixture",
            path,
        ])
    };
    let result = cli(&[
        "worktree",
        "delete-safe",
        "--project",
        "repo",
        "--",
        main.id.as_str(),
        "main",
        main.path.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("protected"));
    let result = cli(&[
        "worktree",
        "delete-safe",
        "--project",
        "repo",
        "--",
        id,
        "stale-branch",
        path,
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("changed"));
    std::fs::write(Path::new(path).join("untracked"), "keep me").unwrap();
    let result = remove();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Dirty checkout"));
    commit_file(Path::new(path), "untracked", "keep this commit");
    let result = remove();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Unmerged commits"));
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["merge", "--ff-only", "fixture"])
        .status()
        .unwrap();
    assert!(status.success());
    let agent = create_agent_get_id(
        &mut c,
        &nebula_core::WorktreeId(id.into()),
        "fixture agent",
        93,
    )
    .await;
    let result = remove();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Sessions still exist"));
    assert!(Path::new(path).join("untracked").exists());
    let result = cli(&[
        "agent",
        "delete-id",
        "--project",
        "repo",
        "--",
        agent.as_str(),
    ]);
    assert!(result.status.success(), "{result:?}");
    // Claude's fixture hook installer left ignored configuration: preserve-by-default applies to it too.
    let result = remove();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("Dirty checkout"));
    std::fs::remove_dir_all(Path::new(path).join(".claude")).unwrap();
    let result = cli(&[
        "worktree",
        "delete-safe",
        "--check",
        "--project",
        "repo",
        "--",
        id,
        "fixture",
        path,
    ]);
    assert!(result.status.success(), "{result:?}");
    assert!(Path::new(path).exists(), "check is side-effect-free");
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "lock", path])
        .status()
        .unwrap()
        .success());
    let result = remove();
    assert!(
        !result.status.success(),
        "safe removal must not silently unlock and retry: {result:?}"
    );
    assert!(Path::new(path).exists());
    let listing = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&listing.stdout).contains("locked"));
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "unlock", path])
        .status()
        .unwrap()
        .success());
    let result = remove();
    assert!(result.status.success(), "{result:?}");
    assert!(!Path::new(path).exists());
    assert!(repo.exists());
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["show-ref", "--verify", "refs/heads/fixture"])
        .status()
        .unwrap();
    assert!(status.success(), "branch is always kept");
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon);
}

/// Use literal peer versions so a simultaneous client/server constant change cannot mask a mismatch.
#[tokio::test]
async fn protocol29_handshake_accepts_repeated_probes_and_rejects_protocol28() {
    let env = TestEnv::new();
    let _daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    for _ in 0..2 {
        write_frame(
            &mut c,
            &ClientRequest::Hello {
                protocol_version: 29,
            },
        )
        .await
        .unwrap();
        let reply =
            tokio::time::timeout(Duration::from_secs(3), read_frame::<ServerEvent, _>(&mut c))
                .await
                .unwrap()
                .unwrap();
        assert!(
            matches!(
                reply,
                Some(ServerEvent::HelloOk {
                    protocol_version: 29,
                    ..
                })
            ),
            "{reply:?}"
        );
    }
    let mut old = connect(&env.sock()).await;
    write_frame(
        &mut old,
        &ClientRequest::Hello {
            protocol_version: 28,
        },
    )
    .await
    .unwrap();
    let reply = tokio::time::timeout(
        Duration::from_secs(3),
        read_frame::<ServerEvent, _>(&mut old),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(
            reply,
            Some(ServerEvent::Incompatible {
                daemon_protocol_version: 29
            })
        ),
        "{reply:?}"
    );
}

/// A fake agent CLI that records every byte its PTY hands it. `raw` picks
/// the tty mode a running CLI uses; without it the tty stays in line mode,
/// as it is while a CLI is still booting.
fn input_recorder(env: &TestEnv, raw: bool) -> (PathBuf, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let capture = env.tmp.path().join("input");
    let ready = env.tmp.path().join("ready");
    let script = env.tmp.path().join("recorder");
    let mode = if raw { "tty.setraw(0)\n" } else { "" };
    std::fs::write(
        &script,
        format!(
            "#!/usr/bin/python3\nimport os, tty\n{mode}open({ready:?}, 'w').close()\nwhile True:\n with open({capture:?}, 'ab', buffering=0) as f: f.write(os.read(0, 65536))\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, ready, capture)
}

/// 4,200 bytes on one line, every word distinct so a cut shows where it fell.
fn long_single_line_message() -> String {
    (0..700)
        .map(|i| format!("w{i:04}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A tty hands a raw-mode reader a long line in pieces (1,022 bytes each on
/// macOS), and Claude Code keeps only the last piece of an unbracketed burst.
/// A bracketed paste is what lets the receiver put the pieces back together.
#[tokio::test]
async fn agent_send_delivers_a_long_line_as_one_bracketed_paste() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let (script, ready, capture) = input_recorder(&env, true);
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let main = add_project_get_main_worktree(&mut c, &repo).await;
    create_agent_get_id(&mut c, &main.id, "fixture", 90).await;
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready.exists());

    let long = long_single_line_message();
    assert_eq!(long.len(), 4199);
    let mut expected = Vec::new();
    for (text, on_the_wire) in [
        ("short line", "short line\r".to_string()),
        ("two\nlines", "\x1b[200~two\nlines\x1b[201~\r".to_string()),
        (long.as_str(), format!("\x1b[200~{long}\x1b[201~\r")),
    ] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args([
                "agent",
                "send",
                "--paste",
                "--project",
                "repo",
                "--",
                "fixture",
                text,
            ])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        expected.extend_from_slice(on_the_wire.as_bytes());
        for _ in 0..100 {
            if std::fs::read(&capture).unwrap_or_default().len() >= expected.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            String::from_utf8_lossy(&std::fs::read(&capture).unwrap_or_default()),
            String::from_utf8_lossy(&expected)
        );
    }
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}

/// A Claude worker reads a pasted message as quoted content, not as an
/// instruction, so a send that would go as a paste is refused unless asked for.
#[tokio::test]
async fn agent_send_refuses_paste_sized_text_and_sends_a_file_as_a_one_line_pointer() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let (script, ready, capture) = input_recorder(&env, true);
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let main = add_project_get_main_worktree(&mut c, &repo).await;
    create_agent_get_id(&mut c, &main.id, "fixture", 90).await;
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready.exists());
    let send = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(["agent", "send", "--project", "repo"])
            .args(args)
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap()
    };

    let long = long_single_line_message();
    for text in [long.as_str(), "two\nlines"] {
        let result = send(&["--", "fixture", text]);
        assert_eq!(result.status.code(), Some(2), "{result:?}");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains(".scratch/") && stderr.contains("Read <path> and act on it."),
            "{result:?}"
        );
        assert_eq!(String::from_utf8_lossy(&result.stdout), "");
    }

    let scratch = repo.join(".scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let message = scratch.join("msg-fixture-1.md");
    std::fs::write(&message, &long).unwrap();
    for refused in [scratch.join("missing.md"), scratch.clone()] {
        let result = send(&["fixture", "--file", refused.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(2), "{result:?}");
    }
    assert_eq!(std::fs::read(&capture).unwrap_or_default(), b"");

    let result = send(&["fixture", "--file", message.to_str().unwrap()]);
    assert!(result.status.success(), "{result:?}");
    let expected = format!(
        "Read {} and act on it.\r",
        std::fs::canonicalize(&message).unwrap().display()
    );
    for _ in 0..100 {
        if std::fs::read(&capture).unwrap_or_default().len() >= expected.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        String::from_utf8_lossy(&std::fs::read(&capture).unwrap_or_default()),
        expected
    );
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}

#[tokio::test]
async fn agent_new_writer_exits_2_naming_the_live_holder_unless_forced() {
    use std::os::unix::fs::PermissionsExt;
    let env = TestEnv::new();
    let repo = env.make_repo();
    let script = env.tmp.path().join("idle-agent");
    std::fs::write(&script, "#!/bin/sh\nexec sleep 60\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    add_project_get_main_worktree(&mut c, &repo).await;
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .args(["--project", "repo"])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap()
    };
    let spawn = |name: &str, flags: &[&str]| {
        let mut args = vec!["agent", "new", "--worktree", "main", "--name", name];
        args.extend_from_slice(flags);
        cli(&args)
    };

    let holder = spawn("holder", &["--writer"]);
    assert!(holder.status.success(), "{holder:?}");
    let show = cli(&["agent", "show", "holder"]);
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(shown["writer"], true, "{show:?}");

    let second = spawn("second", &["--writer"]);
    assert_eq!(second.status.code(), Some(2), "{second:?}");
    assert_eq!(String::from_utf8_lossy(&second.stdout), "");
    assert!(
        String::from_utf8_lossy(&second.stderr).contains(
            "\"holder\" is the writer on main; wait for it, archive it, or pass --force-writer after the user approves two writers"
        ),
        "{second:?}"
    );

    for (name, flags) in [
        ("forced", &["--writer", "--force-writer"][..]),
        ("reviewer", &[]),
    ] {
        let out = spawn(name, flags);
        assert!(out.status.success(), "{name}: {out:?}");
    }
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}

/// A tty still in line mode silently drops whatever does not fit its input
/// queue, so the write "succeeds" for text the CLI will never read.
#[tokio::test]
async fn agent_send_refuses_a_long_message_the_tty_would_drop() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let (script, ready, capture) = input_recorder(&env, false);
    let mut daemon = env.spawn_daemon_with_agent_cmd(script.to_str().unwrap());
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let main = add_project_get_main_worktree(&mut c, &repo).await;
    create_agent_get_id(&mut c, &main.id, "fixture", 90).await;
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready.exists());

    let result = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args([
            "agent",
            "send",
            "--paste",
            "--project",
            "repo",
            "--",
            "fixture",
            &long_single_line_message(),
        ])
        .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
        .output()
        .unwrap();
    assert!(!result.status.success(), "{result:?}");
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("line mode"),
        "{result:?}"
    );
    assert_eq!(String::from_utf8_lossy(&result.stdout), "");
    assert_eq!(std::fs::read(&capture).unwrap_or_default(), b"");
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}

async fn create_terminal_get_id(
    c: &mut UnixStream,
    worktree: &nebula_core::WorktreeId,
    req_id: u64,
) -> nebula_core::TerminalId {
    write_frame(
        c,
        &ClientRequest::CreateTerminal {
            expected_branch: None,
            req_id,
            worktree: worktree.clone(),
            name: None,
        },
    )
    .await
    .unwrap();
    let events = read_events_until(c, Duration::from_secs(5), |evs| {
        find_ack(evs, req_id).is_some()
    })
    .await;
    let Some(ServerEvent::Ack {
        created: Some(EntityId::Terminal(id)),
        ..
    }) = find_ack(&events, req_id)
    else {
        panic!("CreateTerminal failed: {events:#?}");
    };
    id.clone()
}

async fn exit_terminal(c: &mut UnixStream, id: &nebula_core::TerminalId) {
    write_frame(
        c,
        &ClientRequest::Input {
            session: SessionRef::Terminal(id.clone()),
            data: b"exit\n".to_vec(),
        },
    )
    .await
    .unwrap();
    read_events_until(c, Duration::from_secs(5), |evs| {
        evs.iter().any(|e| {
            matches!(e,
        ServerEvent::EntityUpserted { entity: Entity::Terminal(t) } if t.id == *id && !t.alive)
        })
    })
    .await;
}

async fn archive_agent(c: &mut UnixStream, id: &nebula_core::AgentId, req_id: u64) {
    write_frame(
        c,
        &ClientRequest::ArchiveAgent {
            req_id,
            id: id.clone(),
        },
    )
    .await
    .unwrap();
    let events = read_events_until(c, Duration::from_secs(5), |evs| {
        find_ack(evs, req_id).is_some()
    })
    .await;
    assert!(
        matches!(find_ack(&events, req_id), Some(ServerEvent::Ack { .. })),
        "ArchiveAgent failed: {events:#?}"
    );
}

fn new_worktree(cli: &dyn Fn(&[&str]) -> std::process::Output, branch: &str) -> (String, String) {
    let result = cli(&["worktree", "new", branch, "--project", "repo"]);
    assert!(result.status.success(), "{result:?}");
    let result = cli(&["worktree", "list", "--project", "repo"]);
    let trees: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let tree = trees
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["branch"] == branch)
        .unwrap();
    (
        tree["id"].as_str().unwrap().to_string(),
        tree["path"].as_str().unwrap().to_string(),
    )
}

fn worktree_ids(cli: &dyn Fn(&[&str]) -> std::process::Output) -> Vec<String> {
    let result = cli(&["worktree", "list", "--project", "repo"]);
    let trees: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    trees
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn safe_removal_takes_archived_sessions_only_with_the_flag_and_never_live_ones() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    add_project_get_main_worktree(&mut c, &repo).await;
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap()
    };
    let (id, path) = new_worktree(&cli, "fixture");
    let wid = nebula_core::WorktreeId(id.clone());
    let agent = create_agent_get_id(&mut c, &wid, "archived agent", 2).await;
    archive_agent(&mut c, &agent, 3).await;
    let _ = std::fs::remove_dir_all(Path::new(&path).join(".claude"));
    let term = create_terminal_get_id(&mut c, &wid, 4).await;
    let remove = |flags: &[&str]| {
        let mut args = vec!["worktree", "delete-safe", "--project", "repo"];
        args.extend_from_slice(flags);
        args.extend_from_slice(&["--", id.as_str(), "fixture", path.as_str()]);
        cli(&args)
    };

    let result = remove(&["--with-archived"]);
    assert!(
        !result.status.success(),
        "a live terminal always refuses: {result:?}"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("1 live"),
        "{result:?}"
    );

    exit_terminal(&mut c, &term).await;
    let result = remove(&[]);
    assert!(!result.status.success(), "{result:?}");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("1 archived session(s) and 1 finished terminal(s)")
            && stderr.contains("--with-archived"),
        "{stderr}"
    );

    let result = remove(&["--check", "--with-archived"]);
    assert!(result.status.success(), "{result:?}");
    let stdout = String::from_utf8_lossy(&result.stdout);
    for criterion in [
        "files: ok",
        "base: ok — main (from created_from `main`)",
        "merged: ok — ancestor",
        "sessions: ok",
    ] {
        assert!(
            stdout.contains(criterion),
            "missing {criterion:?} in {stdout}"
        );
    }
    assert!(Path::new(&path).exists(), "check is side-effect-free");

    let result = remove(&["--with-archived"]);
    assert!(result.status.success(), "{result:?}");
    assert!(!Path::new(&path).exists());
    read_events_until(&mut c, Duration::from_secs(5), |evs| {
        evs.iter().any(
            |e| matches!(e, ServerEvent::EntityRemoved { id: EntityId::Agent(a) } if *a == agent),
        ) && evs.iter().any(
            |e| matches!(e, ServerEvent::EntityRemoved { id: EntityId::Terminal(t) } if *t == term),
        )
    })
    .await;
    assert!(!worktree_ids(&cli).contains(&id));
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}

#[tokio::test]
async fn prune_drops_ghost_rows_but_never_one_with_a_live_session() {
    let env = TestEnv::new();
    let repo = env.make_repo();
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    add_project_get_main_worktree(&mut c, &repo).await;
    let cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(args)
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .output()
            .unwrap()
    };
    let (ghost, ghost_path) = new_worktree(&cli, "ghost");
    let (busy, busy_path) = new_worktree(&cli, "busy");
    let agent = create_agent_get_id(
        &mut c,
        &nebula_core::WorktreeId(ghost.clone()),
        "archived agent",
        2,
    )
    .await;
    archive_agent(&mut c, &agent, 3).await;
    create_terminal_get_id(&mut c, &nebula_core::WorktreeId(busy.clone()), 4).await;
    for path in [&ghost_path, &busy_path] {
        std::fs::remove_dir_all(path).unwrap();
    }
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "prune"])
        .status()
        .unwrap()
        .success());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        worktree_ids(&cli).contains(&ghost),
        "the sync keeps a ghost row that sessions point at"
    );

    let result = cli(&["worktree", "prune", "--check", "--project", "repo"]);
    assert!(
        !result.status.success(),
        "a kept ghost row is a refusal: {result:?}"
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("ghost ghost") && stdout.contains("--with-archived"),
        "{stdout}"
    );
    assert!(
        stdout.contains("ghost busy") && stdout.contains("1 live"),
        "{stdout}"
    );

    let result = cli(&[
        "worktree",
        "prune",
        "--check",
        "--with-archived",
        "--project",
        "repo",
    ]);
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains("ghost ghost") && stdout.contains("go with it"),
        "{stdout}"
    );
    assert!(
        worktree_ids(&cli).contains(&ghost),
        "check is side-effect-free"
    );

    let result = cli(&["worktree", "prune", "--with-archived", "--project", "repo"]);
    assert!(
        !result.status.success(),
        "the live row is still refused: {result:?}"
    );
    let ids = worktree_ids(&cli);
    assert!(
        !ids.contains(&ghost),
        "the archived-only ghost row is dropped"
    );
    assert!(
        ids.contains(&busy),
        "a row with a live session is never dropped"
    );
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}

fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn version_build_id() -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .arg("--version")
        .output()
        .unwrap();
    let line = String::from_utf8(out.stdout).unwrap();
    let id = line.trim().rsplit_once('(').unwrap().1;
    id.strip_suffix(')').unwrap().to_string()
}

#[tokio::test]
async fn version_and_daemon_report_the_git_build() {
    let head = std::process::Command::new("git")
        .args([
            "-C",
            env!("CARGO_MANIFEST_DIR"),
            "rev-parse",
            "--short=7",
            "HEAD",
        ])
        .output()
        .unwrap();
    let sha = String::from_utf8(head.stdout).unwrap().trim().to_string();
    let version = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
        .arg("--version")
        .output()
        .unwrap();
    let version = String::from_utf8(version.stdout).unwrap();
    let prefix = format!("nebula {} ({sha}", env!("CARGO_PKG_VERSION"));
    let suffix = version
        .trim()
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("`{version}` should start with `{prefix}`"));
    assert!(suffix == ")" || suffix == "-dirty)", "{version}");

    let env = TestEnv::new();
    let daemon_build = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .arg("_daemon-build")
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .output()
            .unwrap()
    };
    assert!(!daemon_build().status.success(), "no daemon, no build");
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let out = daemon_build();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        version_build_id()
    );
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}

/// The SSH boundary runs locally against a fake host home; the skills check
/// is the real sync-skills script, on two disposable homes.
#[tokio::test]
async fn remote_status_reports_binary_and_skills_drift() {
    let env = TestEnv::new();
    let mut daemon = env.spawn_daemon();
    let mut c = connect(&env.sock()).await;
    handshake(&mut c).await;
    let laptop = env.tmp.path().join("laptop");
    let host = env.tmp.path().join("host");
    let put = |home: &Path, rel: &str, body: &str| {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    put(&laptop, ".claude/skills/same/SKILL.md", "same");
    put(&host, ".claude/skills/same/SKILL.md", "same");
    put(&laptop, ".claude/skills/edited/SKILL.md", "new text");
    put(&host, ".claude/skills/edited/SKILL.md", "old text");
    put(&host, ".claude/skills/removed/SKILL.md", "gone locally");
    put(&laptop, ".claude/skills/tool/node_modules/x.js", "excluded");
    put(&laptop, ".claude/CLAUDE.md", "rules");
    put(&host, ".claude/CLAUDE.md", "rules");
    // The sync rewrites a laptop-home link onto the host's home: not drift.
    std::os::unix::fs::symlink(
        "/Users/me/.agents/skills/linked",
        laptop.join(".claude/skills/linked"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        host.join(".agents/skills/linked"),
        host.join(".claude/skills/linked"),
    )
    .unwrap();
    // Same content, different mtime: not drift.
    let old = std::fs::File::options()
        .write(true)
        .open(host.join(".claude/skills/same/SKILL.md"))
        .unwrap();
    old.set_modified(std::time::SystemTime::UNIX_EPOCH).unwrap();

    let bin = env.tmp.path().join("laptop-bin");
    write_executable(
        &bin.join("ssh"),
        &format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) shift 2;; --) shift; break;; -*) shift;; *) break;; esac; done\n\
             shift\ncd '{h}' && HOME='{h}' exec /bin/sh -c \"$*\"\n",
            h = host.display()
        ),
    );
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sync-skills.sh"),
        bin.join("nebula-sync-skills"),
    )
    .unwrap();
    let host_nebula = |id: &str| {
        write_executable(
            &host.join(".local/bin/nebula"),
            &format!(
                "#!/bin/sh\ncase \"$1\" in --version) echo 'nebula 0.6.0 ({id})';; _daemon-build) echo {id};; esac\n"
            ),
        )
    };
    let status = || {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(["remote", "fixture-host", "status"])
            .env("NEBULA_RUNTIME_DIR", &env.runtime_dir)
            .env("NEBULA_DATA_DIR", env.tmp.path().join("data"))
            .env("HOME", &laptop)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let local = version_build_id();

    host_nebula("0000000");
    let out = status();
    assert!(
        out.contains("fixture-host: daemon: running (build 0000000)\n"),
        "{out}"
    );
    assert!(
        out.contains(&format!("binary: 0000000 vs local {local} (differs)\n")),
        "{out}"
    );
    assert!(out.contains("skills: 2 files differ\n"), "{out}");

    host_nebula(&local);
    std::fs::remove_dir_all(host.join(".claude/skills/removed")).unwrap();
    put(&host, ".claude/skills/edited/SKILL.md", "new text");
    let out = status();
    let verdict = if local.ends_with("-dirty") {
        "unknown"
    } else {
        "same"
    };
    assert!(
        out.contains(&format!("binary: {local} vs local {local} ({verdict})\n")),
        "{out}"
    );
    assert!(out.contains("skills: in sync\n"), "{out}");
    write_frame(&mut c, &ClientRequest::Shutdown).await.unwrap();
    wait_for_exit(&mut daemon.0);
}
