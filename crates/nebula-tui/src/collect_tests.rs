use super::*;
use nebula_core::{AgentId, ProjectId, WorktreeId};
use std::process::Command;

struct LocalHost;
impl HostRunner for LocalHost {
    fn run(&self, script: &str) -> Result<String> {
        let out = Command::new("sh").args(["-c", script]).output()?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8(out.stdout)?)
    }
    fn push_env(
        &self,
        _: &Path,
        _: &Path,
        _: &[PathBuf],
        _: bool,
    ) -> Result<nebula_core::envfiles::Pushed> {
        bail!("collect must not carry env over SSH")
    }
    fn carry_scratch(&self, _: &Path, _: &Path, _: bool) -> Result<()> {
        bail!("collect must not import remote context")
    }
    fn mirror_skills(&self) -> Result<String> {
        bail!("collect must not run scripts")
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    local: PathBuf,
    remote: PathBuf,
    snapshot: Snapshot,
}
impl Sessions for Fixture {
    async fn snapshot(&mut self) -> Result<Snapshot> {
        Ok(self.snapshot.clone())
    }
    async fn inspect(&mut self, _: &Worktree) -> Result<()> {
        Ok(())
    }
    async fn handoff(&mut self, _: &Agent, _: &str, _: Duration) -> Result<()> {
        bail!("unexpected prompt")
    }
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let origin = dir.path().join("origin.git");
        git(
            dir.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        )
        .unwrap();
        let local = dir.path().join("project");
        let remote = dir.path().join("remote");
        git(
            dir.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                local.to_str().unwrap(),
            ],
        )
        .unwrap();
        git(&local, &["config", "user.email", "test@example.test"]).unwrap();
        git(&local, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(local.join(".gitignore"), ".env*\n.scratch/\n").unwrap();
        git(&local, &["add", ".gitignore"]).unwrap();
        git(&local, &["commit", "-qm", "base"]).unwrap();
        git(&local, &["push", "-qu", "origin", "main"]).unwrap();
        git(
            dir.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                remote.to_str().unwrap(),
            ],
        )
        .unwrap();
        git(&remote, &["config", "user.email", "test@example.test"]).unwrap();
        git(&remote, &["config", "user.name", "Test"]).unwrap();
        git(&remote, &["switch", "-c", "feature"]).unwrap();
        std::fs::write(remote.join("result.txt"), "remote work").unwrap();
        git(&remote, &["add", "result.txt"]).unwrap();
        git(&remote, &["commit", "-qm", "feature"]).unwrap();
        let project = |id: &str, path: &Path, host: Option<&str>| Project {
            id: ProjectId(id.into()),
            name: "project".into(),
            workspace_id: Default::default(),
            repo_path: path.into(),
            sort_order: 0,
            divider_after: false,
            divider_label: None,
            divider_before: false,
            divider_before_label: None,
            host: host.map(str::to_owned),
        };
        let checkout = |id: &str, project: &str, path: &Path, branch: &str| Worktree {
            id: WorktreeId(id.into()),
            project_id: ProjectId(project.into()),
            path: path.into(),
            branch: branch.into(),
            is_main: true,
            created_from: None,
            pinned: false,
            for_branch: false,
            sort_order: 0,
        };
        let snapshot = Snapshot {
            projects: vec![
                project("local", &local, None),
                project("remote", &remote, Some("findl")),
            ],
            worktrees: vec![
                checkout("local-root", "local", &local, "main"),
                checkout("remote-root", "remote", &remote, "feature"),
            ],
            ..Default::default()
        };
        Self {
            _dir: dir,
            local,
            remote,
            snapshot,
        }
    }
    fn options(&self, mode: Mode) -> CollectOpts {
        CollectOpts {
            selector: "feature".into(),
            mode: Some(mode),
            project: Some("project".into()),
            host: None,
            agent: None,
            dry_run: false,
            timeout: Duration::from_secs(1),
        }
    }
    async fn run(&mut self, opts: &CollectOpts) -> Report {
        let mut r = Report {
            dry_run: opts.dry_run,
            ..Default::default()
        };
        if let Err(e) = collect(opts, self, &LocalHost, &mut r).await {
            r.refusal = Some(format!("{e:#}"));
        }
        r
    }
    fn owner(&mut self, status: AgentStatus) {
        self.snapshot.agents.push(Agent {
            id: AgentId("owner".into()),
            worktree_id: WorktreeId("remote-root".into()),
            name: "Owner".into(),
            status,
            archived: false,
            archived_at: 0,
            pinned: false,
            status_changed_at: 100,
            kind: Default::default(),
            model: None,
            effort: None,
            session_id: None,
            sort_order: 0,
            alive: true,
            branch: Some("feature".into()),
            writer: false,
        });
    }
}

#[tokio::test]
async fn collect_worktree_seeds_only_local_env_and_repeated_collection_preserves_both_checkouts() {
    let mut f = Fixture::new();
    std::fs::write(f.local.join(".env"), "LOCAL=value").unwrap();
    std::fs::write(f.remote.join(".env"), "REMOTE=secret").unwrap();
    let o = f.options(Mode::Worktree);
    let first = f.run(&o).await;
    assert!(first.refusal.is_none(), "{first:?}");
    let destination = first.destination.unwrap();
    assert_eq!(
        std::fs::read_to_string(destination.path.join(".env")).unwrap(),
        "LOCAL=value"
    );
    let second = f.run(&o).await;
    assert!(second.refusal.is_none(), "{second:?}");
    assert_eq!(second.destination.unwrap().path, destination.path);
    assert_eq!(second.source.unwrap().sha, destination.sha);
    assert_eq!(
        git(&f.local, &["branch", "--show-current"]).unwrap(),
        "main"
    );
    assert_eq!(
        git(&f.remote, &["branch", "--show-current"]).unwrap(),
        "feature"
    );
}

#[tokio::test]
async fn collect_dry_run_refuses_diverged_history_without_fetching_or_switching() {
    let mut f = Fixture::new();
    git(&f.local, &["switch", "-c", "feature"]).unwrap();
    std::fs::write(f.local.join("local.txt"), "local work").unwrap();
    git(&f.local, &["add", "local.txt"]).unwrap();
    git(&f.local, &["commit", "-qm", "local"]).unwrap();
    let before = git(&f.local, &["rev-parse", "HEAD"]).unwrap();
    let mut o = f.options(Mode::Branch);
    o.dry_run = true;
    let report = f.run(&o).await;
    assert!(
        report
            .refusal
            .as_deref()
            .is_some_and(|r| r.contains("diverged")),
        "{report:?}"
    );
    assert_eq!(git(&f.local, &["rev-parse", "HEAD"]).unwrap(), before);
    assert!(!f.local.join(".git/FETCH_HEAD").exists());
}

#[tokio::test]
async fn collect_refuses_a_parked_live_local_writer_even_when_its_checkout_changed_branch() {
    let mut f = Fixture::new();
    f.owner(AgentStatus::Finished);
    let mut local = f.snapshot.agents[0].clone();
    local.id = AgentId("local-writer".into());
    local.worktree_id = WorktreeId("local-root".into());
    local.branch = Some("feature".into());
    f.snapshot.agents.push(local);
    let report = f.run(&f.options(Mode::Worktree)).await;
    assert!(
        report
            .refusal
            .as_deref()
            .is_some_and(|s| s.contains("local") && s.contains("sessions")),
        "{report:?}"
    );
}

#[tokio::test]
async fn collect_ignores_stale_relay_turns_and_waits_for_correlated_source_ack() {
    let mut f = Fixture::new();
    f.owner(AgentStatus::Finished);
    let owner = f.snapshot.agents[0].clone();
    for success in [false, true] {
        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let mut daemon = DaemonSessions::new(Connection {
            stream: client,
            daemon_pid: 0,
        });
        let peer_owner = owner.clone();
        let peer = tokio::spawn(async move {
            let req = read_frame::<ClientRequest, _>(&mut server)
                .await
                .unwrap()
                .unwrap();
            let ClientRequest::CollectHandoff { req_id, .. } = req else {
                panic!("expected guarded source request");
            };
            // A complete OLD remote turn still in relay transit after the request.
            for (status, stamp) in [(AgentStatus::Running, 101), (AgentStatus::Finished, 102)] {
                write_frame(
                    &mut server,
                    &ServerEvent::StatusChanged {
                        agent: peer_owner.id.clone(),
                        status,
                        changed_at: stamp,
                    },
                )
                .await
                .unwrap();
            }
            let reply = if success {
                ServerEvent::Ack {
                    req_id,
                    created: None,
                }
            } else {
                ServerEvent::Error {
                    req_id: Some(req_id),
                    message: "source owner changed; refusing stale handoff".into(),
                }
            };
            write_frame(&mut server, &reply).await.unwrap();
        });
        let result = daemon
            .handoff(&owner, "commit explicit files", Duration::from_millis(30))
            .await;
        assert_eq!(result.is_ok(), success, "{result:?}");
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn collect_branch_uses_primary_and_preserves_finished_remote_session() {
    let mut f = Fixture::new();
    f.owner(AgentStatus::Finished);
    let o = f.options(Mode::Branch);
    let r = f.run(&o).await;
    assert!(r.refusal.is_none(), "{r:?}");
    assert_eq!(
        r.destination.as_ref().unwrap().path,
        f.local.canonicalize().unwrap()
    );
    assert_eq!(
        git(&f.local, &["branch", "--show-current"]).unwrap(),
        "feature"
    );
    assert_eq!(
        r.source.as_ref().unwrap().sha,
        r.destination.as_ref().unwrap().sha
    );
    assert_eq!(f.snapshot.agents[0].worktree_id.as_str(), "remote-root");
    assert!(!f.snapshot.agents[0].archived);
    assert!(f.run(&o).await.refusal.is_none());
}

#[tokio::test]
async fn collect_refusals_leave_local_history_and_remote_checkout_untouched() {
    for case in [
        "busy",
        "feedback",
        "multiple",
        "dirty-local",
        "unowned-dirty",
        "parked",
    ] {
        let mut f = Fixture::new();
        match case {
            "busy" => f.owner(AgentStatus::Running),
            "feedback" => f.owner(AgentStatus::NeedsFeedback),
            "multiple" => {
                f.owner(AgentStatus::Finished);
                f.owner(AgentStatus::Finished);
            }
            "dirty-local" => std::fs::write(f.local.join("mine.txt"), "uncommitted").unwrap(),
            "unowned-dirty" => std::fs::write(f.remote.join("theirs.txt"), "uncommitted").unwrap(),
            "parked" => {
                git(
                    &f.local,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        "feature",
                        f._dir.path().join("parked").to_str().unwrap(),
                    ],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let before = git(&f.local, &["rev-parse", "HEAD"]).unwrap();
        let remote_before = git(&f.remote, &["status", "--porcelain"]).unwrap();
        let r = f.run(&f.options(Mode::Branch)).await;
        assert!(r.refusal.is_some(), "{case}: {r:?}");
        assert_eq!(
            git(&f.local, &["rev-parse", "HEAD"]).unwrap(),
            before,
            "{case}"
        );
        assert_eq!(
            git(&f.remote, &["status", "--porcelain"]).unwrap(),
            remote_before,
            "{case}"
        );
    }
}

struct CommittingOwner {
    fixture: Fixture,
}
impl Sessions for CommittingOwner {
    async fn snapshot(&mut self) -> Result<Snapshot> {
        self.fixture.snapshot().await
    }
    async fn inspect(&mut self, _: &Worktree) -> Result<()> {
        Ok(())
    }
    async fn handoff(&mut self, _: &Agent, _: &str, _: Duration) -> Result<()> {
        // An external agent completes its request by committing only its file.
        git(&self.fixture.remote, &["add", "result.txt"])?;
        git(&self.fixture.remote, &["commit", "-qm", "handoff"])?;
        Ok(())
    }
}

#[tokio::test]
async fn collect_commits_only_through_the_owner_and_refuses_any_remaining_dirty_files() {
    for unrelated in [false, true] {
        let mut f = Fixture::new();
        f.owner(AgentStatus::Finished);
        std::fs::write(f.remote.join("result.txt"), "completed handoff").unwrap();
        if unrelated {
            std::fs::write(f.remote.join("someone-else.txt"), "keep this").unwrap();
        }
        let o = f.options(Mode::Branch);
        let mut owner = CommittingOwner { fixture: f };
        let mut r = Report::default();
        let result = collect(&o, &mut owner, &LocalHost, &mut r).await;
        assert_eq!(result.is_ok(), !unrelated, "{result:?}");
        if unrelated {
            assert_eq!(
                std::fs::read_to_string(owner.fixture.remote.join("someone-else.txt")).unwrap(),
                "keep this"
            );
            assert_eq!(
                git(&owner.fixture.local, &["branch", "--show-current"]).unwrap(),
                "main"
            );
        } else {
            assert_eq!(
                std::fs::read_to_string(owner.fixture.local.join("result.txt")).unwrap(),
                "completed handoff"
            );
        }
    }
}
