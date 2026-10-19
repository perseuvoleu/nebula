use super::{Daemon, Retired, SessionEnd};
use crate::dispatcher::LeaseId;
use crate::heavy::{Admission, Lease, SessionNews};
use crate::pty::PtySession;
use crate::status::HookEvent;
use anyhow::{anyhow, Context};
use nebula_core::{Agent, AgentId, AgentKind, AgentStatus, Entity, ServerEvent, SessionRef};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{mpsc, MutexGuard, RwLockReadGuard};

const BUSY_RETRY: Duration = Duration::from_millis(250);

/// Every agent start holds `lock`, so a row's process is spawned at most
/// once; `queued` holds the launch each queued row waits as, and a grant
/// for any other launch is stale.
#[derive(Default)]
pub struct Starts {
    lock: tokio::sync::Mutex<()>,
    queued: Mutex<HashMap<AgentId, LeaseId>>,
}

impl Starts {
    pub(super) async fn hold(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().await
    }
}

pub enum Start {
    Admit,
    /// `Admit` for a process that replaces one just stopped: the old turn is
    /// reset before the new process can report its first hook.
    Respawn,
    Force,
    Granted {
        launch: LeaseId,
        charge: Lease,
    },
    /// Admitted for a caller that left before it heard; never queued, so it
    /// starts only a row that nothing else owns.
    Admitted {
        charge: Lease,
    },
}

pub enum Launch {
    Started(Arc<PtySession>),
    Queued,
}

pub enum StartError {
    Busy(Start),
    Stale,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for StartError {
    fn from(e: anyhow::Error) -> Self {
        StartError::Failed(e)
    }
}

impl StartError {
    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            StartError::Busy(_) => anyhow!("checkout operation in progress; retry resume"),
            StartError::Stale => anyhow!("agent is no longer queued for this start"),
            StartError::Failed(e) => e,
        }
    }
}

pub(super) fn queued_message(agent: &Agent) -> String {
    format!(
        "\"{}\" is queued for memory and starts by itself when room frees; `nebula agent start {} --force` starts it now",
        agent.name,
        nebula_core::remote::ssh_quote(&agent.name)
    )
}

pub(super) fn too_large_message(kind: AgentKind, reserved: u64, budget: u64) -> String {
    format!(
        "a {} session reserves {} but the whole memory budget is {}; raise heavy.budget_gib or lower its heavy.session_reservation_gib",
        kind.as_str(),
        gib(reserved),
        gib(budget)
    )
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

impl Daemon {
    pub async fn start_agent(
        self: &Arc<Self>,
        id: &AgentId,
        start: Start,
        cols: u16,
        rows: u16,
    ) -> Result<Launch, StartError> {
        if matches!(start, Start::Admit) {
            if let Some(session) = self
                .session(&SessionRef::Agent(id.clone()))
                .filter(|s| !s.was_killed() && !s.has_exited())
            {
                return Ok(Launch::Started(session));
            }
        }
        let Ok(execution) = self.checkout_execution.try_read() else {
            return Err(StartError::Busy(start));
        };
        self.start_held(id, start, cols, rows, &execution).await
    }

    /// Start a queued row now, bypassing memory admission; the row as started.
    pub async fn force_start_agent(self: &Arc<Self>, id: &AgentId) -> anyhow::Result<Agent> {
        self.start_agent(id, Start::Force, 80, 24)
            .await
            .map_err(StartError::into_anyhow)?;
        self.agent_entity(id)
    }

    pub(super) async fn start_held(
        self: &Arc<Self>,
        id: &AgentId,
        start: Start,
        cols: u16,
        rows: u16,
        execution: &RwLockReadGuard<'_, ()>,
    ) -> Result<Launch, StartError> {
        let owner = self.starts.hold().await;
        self.start_owned(id, start, cols, rows, execution, &owner)
            .await
    }

    /// `start_held` for a caller that already holds `Starts.lock`.
    pub(super) async fn start_owned(
        self: &Arc<Self>,
        id: &AgentId,
        start: Start,
        cols: u16,
        rows: u16,
        _execution: &RwLockReadGuard<'_, ()>,
        owner: &MutexGuard<'_, ()>,
    ) -> Result<Launch, StartError> {
        let sref = SessionRef::Agent(id.clone());
        if let Some(old) = self
            .session(&sref)
            .filter(|s| s.was_killed() || s.has_exited())
        {
            let exit_code = self.outlive(&old).await?;
            if !matches!(self.retire(&old, exit_code, owner), Retired::Gone) {
                self.publish_session_end(&sref, SessionEnd::Exited(exit_code));
            }
        }
        let agent = self.store.get_agent(id)?.context("agent not found")?;
        if let Start::Granted { launch, .. } = &start {
            if self.starts.queued.lock().unwrap().get(id) != Some(launch) {
                return Err(StartError::Stale);
            }
        }
        if let Some(session) = self.session(&sref) {
            return match start {
                Start::Admit | Start::Respawn => Ok(Launch::Started(session)),
                Start::Force => Err(anyhow!(not_queued(&agent)).into()),
                Start::Granted { .. } | Start::Admitted { .. } => Err(StartError::Stale),
            };
        }
        let takes_queued = matches!(start, Start::Force | Start::Granted { .. })
            && agent.status == AgentStatus::Queued;
        let started = self.admit_and_spawn(&agent, start, cols, rows).await;
        if takes_queued
            && matches!(started, Err(StartError::Failed(_)))
            && self.store.get_agent(id)?.is_some()
        {
            self.starts.queued.lock().unwrap().remove(id);
            self.override_hook_status(id, AgentStatus::Terminated);
        }
        let launch = started?;
        if let Launch::Started(_) = launch {
            self.broadcast(ServerEvent::EntityUpserted {
                entity: Entity::Agent(self.agent_entity(id)?),
            });
        }
        Ok(launch)
    }

    async fn admit_and_spawn(
        self: &Arc<Self>,
        agent: &Agent,
        start: Start,
        cols: u16,
        rows: u16,
    ) -> Result<Launch, StartError> {
        if agent.archived {
            return Err(anyhow!("agent is archived — unarchive it first").into());
        }
        let worktree = self
            .store
            .get_worktree(&agent.worktree_id)?
            .context("worktree not found")?;
        self.validate_execution_branch(&worktree, agent.branch.as_deref())?;
        let reserved = crate::config::Config::load()
            .heavy
            .session_reservation_bytes(agent.kind);
        let respawn = matches!(start, Start::Respawn);
        let charge = match start {
            Start::Admit | Start::Respawn => {
                if agent.status == AgentStatus::Queued {
                    return Err(anyhow!(queued_message(agent)).into());
                }
                let pending = self.heavy.request_session(agent.id.clone(), reserved)?;
                self.unstarted_creates.lock().unwrap().remove(&agent.id);
                match pending.answer().await? {
                    Admission::Admitted(charge) => charge,
                    Admission::Queued(launch) => {
                        self.starts
                            .queued
                            .lock()
                            .unwrap()
                            .insert(agent.id.clone(), launch);
                        self.override_hook_status(&agent.id, AgentStatus::Queued);
                        tracing::info!(agent = %agent.id, launch = launch.0, "agent queued for memory");
                        return Ok(Launch::Queued);
                    }
                    Admission::TooLarge {
                        reserved_bytes,
                        budget_bytes,
                    } => {
                        self.override_hook_status(&agent.id, AgentStatus::Terminated);
                        return Err(anyhow!(too_large_message(
                            agent.kind,
                            reserved_bytes,
                            budget_bytes
                        ))
                        .into());
                    }
                }
            }
            Start::Force => {
                if agent.status != AgentStatus::Queued {
                    return Err(anyhow!(not_queued(agent)).into());
                }
                self.starts.queued.lock().unwrap().remove(&agent.id);
                self.heavy.force_session(agent.id.clone(), reserved)
            }
            Start::Granted { charge, .. } => {
                self.starts.queued.lock().unwrap().remove(&agent.id);
                charge
            }
            Start::Admitted { charge } => {
                if agent.status == AgentStatus::Queued {
                    return Err(StartError::Stale);
                }
                charge
            }
        };
        let current = self
            .store
            .get_agent(&agent.id)?
            .filter(|a| !a.archived && a.worktree_id == agent.worktree_id);
        let Some(current) = current else {
            return Err(anyhow!(
                "the agent was removed or moved while it was starting; nothing was started"
            )
            .into());
        };
        if self.store.get_project(&worktree.project_id)?.is_none() {
            return Err(anyhow!(
                "the agent's project was removed while it was starting; nothing was started"
            )
            .into());
        }
        if respawn {
            self.apply_hook_event(&agent.id, HookEvent::Restarted, None);
        }
        let session = match self.spawn_agent_session(agent, &worktree, cols, rows, charge) {
            Ok(session) => session,
            Err(e) => {
                if respawn {
                    self.override_hook_status(&agent.id, current.status);
                }
                return Err(e.into());
            }
        };
        Ok(Launch::Started(session))
    }

    /// Own a queued launch whose caller stopped waiting: its row reads and
    /// broadcasts `Queued` and starts by itself when granted. A row that is
    /// gone, archived, running or queued as another launch gives it back.
    async fn adopt_queued(&self, agent: &AgentId, launch: LeaseId) {
        let _one = self.starts.hold().await;
        let row = self.store.get_agent(agent).ok().flatten();
        let running = self.session(&SessionRef::Agent(agent.clone())).is_some();
        let mut queued = self.starts.queued.lock().unwrap();
        if row.is_none_or(|a| a.archived) || running || queued.contains_key(agent) {
            drop(queued);
            self.heavy.give_back(launch);
            return;
        }
        queued.insert(agent.clone(), launch);
        drop(queued);
        tracing::info!(%agent, launch = launch.0, "queued agent kept after its caller left");
        self.override_hook_status(agent, AgentStatus::Queued);
        if let Ok(row) = self.agent_entity(agent) {
            self.broadcast(ServerEvent::EntityUpserted {
                entity: Entity::Agent(row),
            });
        }
    }

    /// A start refused as larger than the whole budget after its caller left
    /// reads `Terminated`, as a heard refusal leaves it, unless another start
    /// owns the row by now.
    async fn refuse_unheard(&self, agent: &AgentId, reserved: u64, budget: u64) {
        let _one = self.starts.hold().await;
        let owned = self.session(&SessionRef::Agent(agent.clone())).is_some()
            || self.starts.queued.lock().unwrap().contains_key(agent);
        if owned || !matches!(self.store.get_agent(agent), Ok(Some(_))) {
            return;
        }
        tracing::warn!(%agent, reserved, budget, "agent can never fit the whole budget; terminated");
        self.override_hook_status(agent, AgentStatus::Terminated);
    }

    pub(super) fn cancel_queued_start(&self, id: &AgentId) -> bool {
        self.heavy.cancel_session(id.clone());
        self.starts.queued.lock().unwrap().remove(id).is_some()
    }

    async fn refuse_queued(&self, agent: &AgentId, launch: LeaseId, reserved: u64, budget: u64) {
        let _one = self.starts.hold().await;
        let mut queued = self.starts.queued.lock().unwrap();
        if queued.get(agent) != Some(&launch) {
            return;
        }
        queued.remove(agent);
        drop(queued);
        tracing::warn!(%agent, reserved, budget, "queued agent can never fit the whole budget; terminated");
        self.override_hook_status(agent, AgentStatus::Terminated);
    }
}

fn not_queued(agent: &Agent) -> String {
    format!(
        "\"{}\" is not queued (status: {}); only a queued agent can be force-started",
        agent.name,
        agent.status.as_str()
    )
}

/// Start the ledger and the session starter, with the rows an earlier daemon
/// left queued back in the queue, oldest first, before any client connects.
pub async fn start(daemon: &Arc<Daemon>) {
    let (news_tx, news_rx) = mpsc::unbounded_channel();
    daemon.heavy.spawn(daemon.shutdown.clone(), news_tx.clone());
    {
        let _one = daemon.starts.hold().await;
        let heavy = crate::config::Config::load().heavy;
        match daemon.store.load_tree() {
            Ok((_, _, mut agents, _)) => {
                agents.retain(|a| a.status == AgentStatus::Queued && !a.archived);
                agents.sort_by(|a, b| {
                    (a.status_changed_at, a.id.as_str()).cmp(&(b.status_changed_at, b.id.as_str()))
                });
                for agent in agents {
                    let launch = daemon.heavy.queue_session(
                        agent.id.clone(),
                        heavy.session_reservation_bytes(agent.kind),
                    );
                    tracing::info!(agent = %agent.id, launch = launch.0, "queued agent restored");
                    daemon
                        .starts
                        .queued
                        .lock()
                        .unwrap()
                        .insert(agent.id, launch);
                }
            }
            Err(e) => tracing::warn!(error = %e, "reading queued agents failed"),
        }
    }
    tokio::spawn(run_starter(Arc::downgrade(daemon), news_rx, news_tx));
}

async fn run_starter(
    daemon: Weak<Daemon>,
    mut news: mpsc::UnboundedReceiver<SessionNews>,
    retry: mpsc::UnboundedSender<SessionNews>,
) {
    while let Some(item) = news.recv().await {
        let Some(daemon) = daemon.upgrade() else {
            break;
        };
        if daemon.shutdown.is_cancelled() {
            break;
        }
        act_on(&daemon, item, &retry).await;
    }
}

async fn act_on(
    daemon: &Arc<Daemon>,
    item: SessionNews,
    retry: &mpsc::UnboundedSender<SessionNews>,
) {
    match item {
        SessionNews::Granted {
            agent,
            launch,
            charge,
        } => {
            start_from_news(
                daemon,
                agent,
                launch,
                Start::Granted { launch, charge },
                retry,
            )
            .await
        }
        SessionNews::Unheard {
            agent,
            launch,
            admission,
        } => match admission {
            Admission::Queued(_) => daemon.adopt_queued(&agent, launch).await,
            Admission::Admitted(charge) => {
                start_from_news(daemon, agent, launch, Start::Admitted { charge }, retry).await
            }
            Admission::TooLarge {
                reserved_bytes,
                budget_bytes,
            } => {
                daemon
                    .refuse_unheard(&agent, reserved_bytes, budget_bytes)
                    .await
            }
        },
        SessionNews::Refused {
            agent,
            launch,
            reserved_bytes,
            budget_bytes,
        } => {
            daemon
                .refuse_queued(&agent, launch, reserved_bytes, budget_bytes)
                .await
        }
    }
}

async fn start_from_news(
    daemon: &Arc<Daemon>,
    agent: AgentId,
    launch: LeaseId,
    start: Start,
    retry: &mpsc::UnboundedSender<SessionNews>,
) {
    match daemon.start_agent(&agent, start, 80, 24).await {
        Ok(_) => tracing::info!(%agent, launch = launch.0, "agent started by the session starter"),
        Err(StartError::Busy(start)) => {
            let news = match start {
                Start::Granted { launch, charge } => SessionNews::Granted {
                    agent,
                    launch,
                    charge,
                },
                Start::Admitted { charge } => SessionNews::Unheard {
                    agent,
                    launch,
                    admission: Admission::Admitted(charge),
                },
                Start::Admit | Start::Respawn | Start::Force => return,
            };
            let retry = retry.clone();
            tokio::spawn(async move {
                tokio::time::sleep(BUSY_RETRY).await;
                let _ = retry.send(news);
            });
        }
        Err(StartError::Stale) => {
            tracing::debug!(%agent, launch = launch.0, "stale session grant dropped")
        }
        Err(StartError::Failed(e)) => {
            tracing::warn!(%agent, launch = launch.0, error = %e, "agent failed to start for the session starter")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::Consumer;
    use crate::hooks::HookEnv;
    use crate::registry::DaemonOverrides;
    use crate::store::Store;
    use nebula_core::{Project, ProjectId, Worktree, WorktreeId};
    use std::path::Path;

    struct Fixture {
        _tmp: tempfile::TempDir,
        daemon: Arc<Daemon>,
        started: std::path::PathBuf,
        agent: AgentId,
    }

    fn queued_row(agent_cmd: Option<&str>) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        let started = tmp.path().join("started");
        let worker = tmp.path().join("worker.sh");
        std::fs::write(
            &worker,
            format!("echo \"$NEBULA_AGENT_ID $*\" >> {}\n", started.display()),
        )
        .unwrap();
        let cmd = agent_cmd.map_or_else(|| format!("/bin/sh {}", worker.display()), str::to_string);
        let daemon = Daemon::with_overrides(
            Arc::new(Store::open_in_memory().unwrap()),
            HookEnv {
                port: 0,
                token: String::new(),
            },
            DaemonOverrides {
                agent_cmd: Some(cmd),
                ..DaemonOverrides::default()
            },
        );
        daemon
            .store
            .insert_project(&Project {
                workspace_id: Default::default(),
                id: ProjectId("p".into()),
                name: "p".into(),
                repo_path: repo.clone(),
                sort_order: 0,
                divider_after: false,
                divider_label: None,
                divider_before: false,
                divider_before_label: None,
                host: None,
            })
            .unwrap();
        daemon
            .store
            .insert_worktree(&Worktree {
                id: WorktreeId("w".into()),
                project_id: ProjectId("p".into()),
                path: repo,
                branch: "main".into(),
                is_main: true,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 0,
            })
            .unwrap();
        let agent = AgentId("a".into());
        daemon
            .store
            .insert_agent_with_prompt(
                &Agent {
                    id: agent.clone(),
                    worktree_id: WorktreeId("w".into()),
                    name: "worker".into(),
                    status: AgentStatus::Queued,
                    archived: false,
                    archived_at: 0,
                    pinned: false,
                    kind: AgentKind::Claude,
                    model: None,
                    effort: None,
                    session_id: None,
                    sort_order: 0,
                    status_changed_at: 0,
                    alive: false,
                    branch: Some("main".into()),
                },
                false,
                Some("the task"),
            )
            .unwrap();
        Fixture {
            _tmp: tmp,
            daemon,
            started,
            agent,
        }
    }

    fn git(repo: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn started(f: &Fixture) -> Vec<String> {
        std::fs::read_to_string(&f.started)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    async fn until_gone(f: &Fixture) {
        let sref = SessionRef::Agent(f.agent.clone());
        for _ in 0..200 {
            if f.daemon.session(&sref).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the forced session never exited");
    }

    #[tokio::test]
    async fn a_grant_still_in_flight_when_a_force_starts_the_row_spawns_nothing_and_frees_its_charge_once(
    ) {
        let f = queued_row(None);
        let granted = f.daemon.heavy.queue_session(f.agent.clone(), 1);
        f.daemon
            .starts
            .queued
            .lock()
            .unwrap()
            .insert(f.agent.clone(), granted);
        let stale = f.daemon.heavy.charge_for(granted);

        let forced = f.daemon.force_start_agent(&f.agent).await.unwrap();
        assert_eq!(forced.status, AgentStatus::Fresh);
        until_gone(&f).await;

        let late = f
            .daemon
            .start_agent(
                &f.agent,
                Start::Granted {
                    launch: granted,
                    charge: stale,
                },
                80,
                24,
            )
            .await;
        assert!(
            matches!(late, Err(StartError::Stale)),
            "the stale grant was authorized"
        );
        assert_eq!(
            started(&f),
            ["a the task"],
            "the row started more than once"
        );
        let mut released = f.daemon.heavy.released();
        for _ in 0..200 {
            if released.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            released.extend(f.daemon.heavy.released());
        }
        assert_eq!(
            released
                .iter()
                .filter(|c| **c == Consumer::SessionLaunch(granted))
                .count(),
            1,
            "{released:?}"
        );
        assert_eq!(
            released.len(),
            2,
            "the stale charge and the forced process's own: {released:?}"
        );
    }

    #[test]
    fn the_suggested_force_command_quotes_the_name_for_the_shell() {
        let f = queued_row(None);
        let mut agent = f.daemon.store.get_agent(&f.agent).unwrap().unwrap();
        agent.name = "Mobile ab's".into();
        assert!(
            queued_message(&agent).contains(r"`nebula agent start 'Mobile ab'\''s' --force`"),
            "{}",
            queued_message(&agent)
        );
    }

    #[tokio::test]
    async fn a_spawn_that_fails_keeps_the_prompt_and_a_later_spawn_hands_it_out_once() {
        let f = queued_row(Some("/nonexistent/agent-cli"));
        let failed = f.daemon.force_start_agent(&f.agent).await;
        assert!(failed.is_err(), "{failed:?}");
        assert_eq!(
            f.daemon
                .store
                .take_agent_pending_prompt(&f.agent)
                .unwrap()
                .as_deref(),
            Some("the task"),
            "the task was lost with the failed spawn"
        );
        assert_eq!(
            f.daemon.store.take_agent_pending_prompt(&f.agent).unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_spawned_prompt_is_gone_before_the_next_fresh_start() {
        let f = queued_row(None);
        f.daemon.force_start_agent(&f.agent).await.unwrap();
        until_gone(&f).await;
        let agent = f.daemon.store.get_agent(&f.agent).unwrap().unwrap();
        let worktree = f
            .daemon
            .store
            .get_worktree(&agent.worktree_id)
            .unwrap()
            .unwrap();
        let charge = f
            .daemon
            .heavy
            .charge_for(f.daemon.heavy.queue_session(f.agent.clone(), 1));
        f.daemon
            .spawn_agent_session(&agent, &worktree, 80, 24, charge)
            .unwrap();
        until_gone(&f).await;
        assert_eq!(started(&f), ["a the task", "a "], "the task was sent twice");
    }

    fn stay() -> (tempfile::TempDir, String, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("cwds");
        let script = dir.path().join("stay.sh");
        std::fs::write(
            &script,
            format!("pwd -P >> {}\nexec sleep 30\n", log.display()),
        )
        .unwrap();
        (dir, format!("/bin/sh {}", script.display()), log)
    }

    fn cwds(log: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(std::path::PathBuf::from)
            .collect()
    }

    fn live_cwd(f: &Fixture) -> Option<std::path::PathBuf> {
        let pid = f
            .daemon
            .session(&SessionRef::Agent(f.agent.clone()))?
            .child_pid?;
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    fn finished(f: &Fixture) {
        f.daemon
            .store
            .set_agent_status(&f.agent, AgentStatus::Finished)
            .unwrap();
    }

    fn add_feat(f: &Fixture) -> (WorktreeId, std::path::PathBuf) {
        let repo = f
            .daemon
            .store
            .get_worktree(&WorktreeId("w".into()))
            .unwrap()
            .unwrap()
            .path;
        git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
        );
        let feat = repo.with_file_name("feat");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                feat.to_str().unwrap(),
            ],
        );
        let id = WorktreeId("w2".into());
        f.daemon
            .store
            .insert_worktree(&Worktree {
                id: id.clone(),
                project_id: ProjectId("p".into()),
                path: feat.clone(),
                branch: "feat".into(),
                is_main: false,
                created_from: None,
                pinned: false,
                for_branch: false,
                sort_order: 1,
            })
            .unwrap();
        (id, std::fs::canonicalize(feat).unwrap())
    }

    async fn admit(f: &Fixture, inbox: &mut crate::heavy::Inbox) -> LeaseId {
        let (launch, reply) = tokio::time::timeout(Duration::from_secs(10), inbox.next_admission())
            .await
            .expect("no start asked for memory");
        assert!(reply
            .send(Admission::Admitted(f.daemon.heavy.charge_for(launch)))
            .is_ok());
        launch
    }

    fn attach_in_background(f: &Fixture) -> tokio::task::JoinHandle<anyhow::Result<()>> {
        let (daemon, agent) = (f.daemon.clone(), f.agent.clone());
        tokio::spawn(async move {
            daemon
                .start_agent(&agent, Start::Admit, 80, 24)
                .await
                .map(|_| ())
                .map_err(StartError::into_anyhow)
        })
    }

    #[tokio::test]
    async fn a_project_removed_while_a_start_owns_its_row_waits_and_then_stops_that_start() {
        let (_dir, cmd, _log) = stay();
        let f = queued_row(Some(&cmd));
        finished(&f);
        let mut inbox = f.daemon.heavy.take_inbox();
        let start = attach_in_background(&f);
        let (launch, reply) = inbox.next_admission().await;
        let removal = {
            let daemon = f.daemon.clone();
            tokio::spawn(async move { daemon.remove_project(&ProjectId("p".into())).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !removal.is_finished(),
            "the project was removed while a start owned its row"
        );
        assert!(reply
            .send(Admission::Admitted(f.daemon.heavy.charge_for(launch)))
            .is_ok());
        start.await.unwrap().unwrap();
        removal.await.unwrap().unwrap();
        assert!(f.daemon.store.get_agent(&f.agent).unwrap().is_none());
        assert!(
            f.daemon
                .session(&SessionRef::Agent(f.agent.clone()))
                .is_none(),
            "a process kept running for a row that is gone"
        );
    }

    #[tokio::test]
    async fn a_row_removed_while_its_start_waited_for_memory_starts_nothing() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        finished(&f);
        let mut inbox = f.daemon.heavy.take_inbox();
        let start = attach_in_background(&f);
        let (launch, reply) = inbox.next_admission().await;
        f.daemon.store.delete_agent(&f.agent).unwrap();
        assert!(reply
            .send(Admission::Admitted(f.daemon.heavy.charge_for(launch)))
            .is_ok());
        let refused = start.await.unwrap().unwrap_err().to_string();
        assert!(refused.contains("removed"), "{refused}");
        assert!(
            cwds(&log).is_empty(),
            "spawned from a row that was already gone"
        );
        assert_eq!(
            inbox
                .released()
                .iter()
                .filter(|c| **c == Consumer::SessionLaunch(launch))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn a_move_waits_for_a_start_in_flight_and_its_process_ends_in_the_new_checkout() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        finished(&f);
        let (feat_id, feat) = add_feat(&f);
        let mut inbox = f.daemon.heavy.take_inbox();
        let start = attach_in_background(&f);
        let (launch, reply) = inbox.next_admission().await;
        let mover = {
            let (daemon, agent, feat_id) = (f.daemon.clone(), f.agent.clone(), feat_id.clone());
            tokio::spawn(async move { daemon.move_agent(&agent, &feat_id).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !mover.is_finished(),
            "the move re-homed the row while a start from the old row was in flight"
        );
        assert!(reply
            .send(Admission::Admitted(f.daemon.heavy.charge_for(launch)))
            .is_ok());
        start.await.unwrap().unwrap();
        admit(&f, &mut inbox).await;
        mover.await.unwrap().unwrap();
        assert_eq!(live_cwd(&f), Some(feat.clone()), "starts: {:?}", cwds(&log));
        for _ in 0..250 {
            if cwds(&log).len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            cwds(&log).last(),
            Some(&feat),
            "the last start ran elsewhere: {:?}",
            cwds(&log)
        );
        assert_eq!(
            f.daemon
                .store
                .get_agent(&f.agent)
                .unwrap()
                .unwrap()
                .worktree_id,
            feat_id
        );
    }

    #[tokio::test]
    async fn a_queued_row_moved_while_its_grant_waits_runs_in_the_new_checkout() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let (feat_id, feat) = add_feat(&f);
        let mut inbox = f.daemon.heavy.take_inbox();
        let launch = f.daemon.heavy.queue_session(f.agent.clone(), 1);
        f.daemon
            .starts
            .queued
            .lock()
            .unwrap()
            .insert(f.agent.clone(), launch);
        let owner = f.daemon.starts.hold().await;
        let grant = {
            let (daemon, agent) = (f.daemon.clone(), f.agent.clone());
            let charge = f.daemon.heavy.charge_for(launch);
            tokio::spawn(async move {
                daemon
                    .start_agent(&agent, Start::Granted { launch, charge }, 80, 24)
                    .await
                    .map(|_| ())
                    .map_err(StartError::into_anyhow)
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mover = {
            let (daemon, agent, feat_id) = (f.daemon.clone(), f.agent.clone(), feat_id.clone());
            tokio::spawn(async move { daemon.move_agent(&agent, &feat_id).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(owner);
        grant.await.unwrap().unwrap();
        admit(&f, &mut inbox).await;
        mover.await.unwrap().unwrap();
        assert_eq!(live_cwd(&f), Some(feat.clone()), "starts: {:?}", cwds(&log));
        assert_eq!(
            f.daemon
                .store
                .get_agent(&f.agent)
                .unwrap()
                .unwrap()
                .worktree_id,
            feat_id
        );
    }

    fn reset_before_exposure(
        events: &mut tokio::sync::broadcast::Receiver<ServerEvent>,
        agent: &AgentId,
    ) {
        let mut seen = Vec::new();
        while let Ok(event) = events.try_recv() {
            seen.push(event);
        }
        let reset = seen.iter().position(|e| {
            matches!(e, ServerEvent::StatusChanged { agent: a, status: AgentStatus::Fresh, .. } if a == agent)
        });
        let exposed = seen.iter().position(|e| {
            matches!(e, ServerEvent::EntityUpserted { entity: Entity::Agent(a) } if &a.id == agent && a.alive)
        });
        assert!(
            matches!((reset, exposed), (Some(r), Some(x)) if r < x),
            "the old turn was reset after the new process was exposed: reset {reset:?}, exposed {exposed:?}"
        );
    }

    async fn abandoned_create(
        f: &Fixture,
        inbox: &mut crate::heavy::Inbox,
    ) -> (AgentId, LeaseId, crate::heavy::Reply) {
        let creator = creating(f);
        let (launch, reply) = tokio::time::timeout(Duration::from_secs(10), inbox.next_admission())
            .await
            .expect("the create never asked for memory");
        creator.abort();
        assert!(creator.await.unwrap_err().is_cancelled());
        let late = row_named(f, "late").expect("the row was persisted").id;
        (late, launch, reply)
    }

    fn handed_over(inbox: &mut crate::heavy::Inbox) -> Admission {
        let SessionNews::Unheard { admission, .. } = inbox.unheard() else {
            panic!("the answer was handed over as something else");
        };
        admission
    }

    async fn hear(f: &Fixture, agent: &AgentId, launch: LeaseId, admission: Admission) {
        let (retry, _) = mpsc::unbounded_channel();
        act_on(
            &f.daemon,
            SessionNews::Unheard {
                agent: agent.clone(),
                launch,
                admission,
            },
            &retry,
        )
        .await;
    }

    async fn starts_logged(log: &Path, expected: usize) -> usize {
        for _ in 0..100 {
            if cwds(log).len() >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        cwds(log).len()
    }

    fn creating(f: &Fixture) -> tokio::task::JoinHandle<anyhow::Result<Agent>> {
        let daemon = f.daemon.clone();
        tokio::spawn(async move {
            daemon
                .create_agent(
                    &WorktreeId("w".into()),
                    None,
                    "late",
                    AgentKind::Claude,
                    None,
                    None,
                    false,
                    Some("hello".into()),
                )
                .await
        })
    }

    fn row_named(f: &Fixture, name: &str) -> Option<Agent> {
        let (_, _, agents, _) = f.daemon.store.load_tree().unwrap();
        agents.into_iter().find(|a| a.name == name)
    }

    /// The ledger's answer reaches the creator's receiver, and the creator is
    /// cancelled before it runs again to read it.
    async fn answered_then_cancelled(
        f: &Fixture,
        inbox: &mut crate::heavy::Inbox,
        answer: impl FnOnce(LeaseId) -> Admission,
    ) -> (AgentId, LeaseId, Admission) {
        let creator = creating(f);
        let (launch, reply) = tokio::time::timeout(Duration::from_secs(10), inbox.next_admission())
            .await
            .expect("the create never asked for memory");
        assert!(
            reply.send(answer(launch)).is_ok(),
            "the creator was not waiting"
        );
        creator.abort();
        assert!(creator.await.unwrap_err().is_cancelled());
        let late = row_named(f, "late")
            .expect("a row its start owns was rolled back")
            .id;
        let unheard = handed_over(inbox);
        assert!(
            inbox.nothing_more_unheard(),
            "one answer was handed over twice"
        );
        (late, launch, unheard)
    }

    #[tokio::test]
    async fn a_queued_answer_the_cancelled_creator_never_read_still_queues_and_starts_the_row() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let (late, launch, unheard) =
            answered_then_cancelled(&f, &mut inbox, Admission::Queued).await;
        hear(&f, &late, launch, unheard).await;
        assert_eq!(
            f.daemon.store.get_agent(&late).unwrap().unwrap().status,
            AgentStatus::Queued
        );
        assert_eq!(
            f.daemon.starts.queued.lock().unwrap().get(&late),
            Some(&launch)
        );
        assert!(inbox.released().is_empty(), "the queued launch was dropped");

        let charge = f.daemon.heavy.charge_for(launch);
        let (retry, _) = mpsc::unbounded_channel();
        act_on(
            &f.daemon,
            SessionNews::Granted {
                agent: late.clone(),
                launch,
                charge,
            },
            &retry,
        )
        .await;
        assert_eq!(starts_logged(&log, 1).await, 1);
        f.daemon.kill_all();
    }

    #[tokio::test]
    async fn an_admission_the_cancelled_creator_never_read_still_starts_the_row_once() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let charge_for = |launch| Admission::Admitted(f.daemon.heavy.charge_for(launch));
        let (late, launch, unheard) = answered_then_cancelled(&f, &mut inbox, charge_for).await;
        hear(&f, &late, launch, unheard).await;
        assert_eq!(starts_logged(&log, 1).await, 1);
        assert!(f.daemon.is_alive(&SessionRef::Agent(late)));
        assert!(
            inbox.released().is_empty(),
            "the charge of the running process was given back"
        );
        f.daemon.kill_all();
    }

    #[tokio::test]
    async fn a_refusal_the_cancelled_creator_never_read_still_terminates_the_row() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let too_large = |_| Admission::TooLarge {
            reserved_bytes: 4 << 30,
            budget_bytes: 1 << 30,
        };
        let (late, launch, unheard) = answered_then_cancelled(&f, &mut inbox, too_large).await;
        hear(&f, &late, launch, unheard).await;
        assert_eq!(
            f.daemon.store.get_agent(&late).unwrap().unwrap().status,
            AgentStatus::Terminated
        );
        assert_eq!(starts_logged(&log, 1).await, 0);
    }

    #[tokio::test]
    async fn a_create_cancelled_before_its_start_asked_for_memory_leaves_no_row() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let mut events = f.daemon.events.subscribe();
        let owner = f.daemon.starts.hold().await;
        let creator = creating(&f);
        for _ in 0..500 {
            if row_named(&f, "late").is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let late = row_named(&f, "late")
            .expect("the create never persisted its row")
            .id;
        creator.abort();
        assert!(creator.await.unwrap_err().is_cancelled());
        drop(owner);

        assert!(
            row_named(&f, "late").is_none(),
            "the cancelled create left its row"
        );
        let removed = std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
            matches!(event, ServerEvent::EntityRemoved { id: nebula_core::EntityId::Agent(id) } if id == late)
        });
        assert!(removed, "clients still show the removed row");
        assert!(
            inbox.asked_for_nothing(),
            "the cancelled create still asked for memory"
        );
        assert_eq!(starts_logged(&log, 1).await, 0);
    }

    #[tokio::test]
    async fn an_unheard_admission_waits_out_a_checkout_and_then_starts_the_row_once() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        f.daemon.override_hook_status(&f.agent, AgentStatus::Fresh);
        let launch = LeaseId(61);
        let (retry, mut retried) = mpsc::unbounded_channel();
        let checkout = f.daemon.checkout_execution.write().await;
        act_on(
            &f.daemon,
            SessionNews::Unheard {
                agent: f.agent.clone(),
                launch,
                admission: Admission::Admitted(f.daemon.heavy.charge_for(launch)),
            },
            &retry,
        )
        .await;
        let again = tokio::time::timeout(Duration::from_secs(2), retried.recv())
            .await
            .expect("the busy start was never retried")
            .unwrap();
        assert!(
            matches!(&again, SessionNews::Unheard { launch: l, admission: Admission::Admitted(_), .. } if *l == launch),
            "the retry lost the charge"
        );
        assert!(
            inbox.released().is_empty(),
            "the charge was given back while busy"
        );
        assert_eq!(
            starts_logged(&log, 1).await,
            0,
            "started during the checkout"
        );
        drop(checkout);

        act_on(&f.daemon, again, &retry).await;
        assert_eq!(starts_logged(&log, 1).await, 1);
        assert!(inbox.released().is_empty());
        f.daemon.kill_all();
    }

    #[tokio::test]
    async fn a_creator_that_left_while_admitted_leaves_a_row_that_starts_by_itself_once() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let (late, launch, reply) = abandoned_create(&f, &mut inbox).await;
        assert!(reply
            .send(Admission::Admitted(f.daemon.heavy.charge_for(launch)))
            .is_err());
        let unheard = handed_over(&mut inbox);

        hear(&f, &late, launch, unheard).await;
        assert_eq!(starts_logged(&log, 1).await, 1);
        let duplicate = LeaseId(launch.0 + 100);
        let again = Admission::Admitted(f.daemon.heavy.charge_for(duplicate));
        hear(&f, &late, duplicate, again).await;

        assert_eq!(starts_logged(&log, 2).await, 1, "the row started twice");
        assert!(f.daemon.is_alive(&SessionRef::Agent(late.clone())));
        assert!(
            !f.daemon.starts.queued.lock().unwrap().contains_key(&late),
            "a queued token was left for a row that runs"
        );
        assert_eq!(
            inbox.released(),
            vec![Consumer::SessionLaunch(duplicate)],
            "the duplicate charge was kept, or the running one given back"
        );
        f.daemon.kill_all();
    }

    #[tokio::test]
    async fn an_unheard_admission_for_a_row_nothing_may_start_gives_its_charge_back() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let gone = Admission::Admitted(f.daemon.heavy.charge_for(LeaseId(51)));
        hear(&f, &AgentId("gone".into()), LeaseId(51), gone).await;
        let queued = Admission::Admitted(f.daemon.heavy.charge_for(LeaseId(52)));
        hear(&f, &f.agent, LeaseId(52), queued).await;

        assert_eq!(
            inbox.released(),
            vec![
                Consumer::SessionLaunch(LeaseId(51)),
                Consumer::SessionLaunch(LeaseId(52))
            ]
        );
        assert_eq!(starts_logged(&log, 1).await, 0);
        assert_eq!(
            f.daemon.store.get_agent(&f.agent).unwrap().unwrap().status,
            AgentStatus::Queued,
            "the queued row lost its place to a charge meant for another start"
        );
    }

    #[tokio::test]
    async fn a_too_large_answer_leaves_the_row_terminated_whether_or_not_its_caller_heard() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let too_large = || Admission::TooLarge {
            reserved_bytes: 4 << 30,
            budget_bytes: 1 << 30,
        };
        let (late, launch, reply) = abandoned_create(&f, &mut inbox).await;
        assert!(reply.send(too_large()).is_err());
        let unheard = handed_over(&mut inbox);
        hear(&f, &late, launch, unheard).await;
        let status = |id: &AgentId| f.daemon.store.get_agent(id).unwrap().unwrap().status;
        assert_eq!(status(&late), AgentStatus::Terminated, "unheard");

        f.daemon.override_hook_status(&late, AgentStatus::Fresh);
        let attach = {
            let (daemon, late) = (f.daemon.clone(), late.clone());
            tokio::spawn(async move {
                daemon
                    .start_agent(&late, Start::Admit, 80, 24)
                    .await
                    .is_err()
            })
        };
        let (_, reply) = tokio::time::timeout(Duration::from_secs(10), inbox.next_admission())
            .await
            .unwrap();
        assert!(reply.send(too_large()).is_ok());
        assert!(attach.await.unwrap(), "a refused start succeeded");
        assert_eq!(status(&late), AgentStatus::Terminated, "heard");
        assert_eq!(starts_logged(&log, 1).await, 0);
    }

    #[tokio::test]
    async fn a_creator_that_left_while_queued_leaves_a_row_that_starts_by_itself_once() {
        let (_dir, cmd, log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let mut events = f.daemon.events.subscribe();
        let (late, launch, reply) = abandoned_create(&f, &mut inbox).await;
        assert!(reply.send(Admission::Queued(launch)).is_err());
        let unheard = handed_over(&mut inbox);

        hear(&f, &late, launch, unheard).await;
        let status = |f: &Fixture| f.daemon.store.get_agent(&late).unwrap().unwrap().status;
        assert_eq!(status(&f), AgentStatus::Queued);
        let mut shown = false;
        while let Ok(event) = events.try_recv() {
            shown |= matches!(event, ServerEvent::EntityUpserted { entity: Entity::Agent(a) } if a.id == late && a.status == AgentStatus::Queued);
        }
        assert!(shown, "clients never saw the queued row");
        let charge = f.daemon.heavy.charge_for(launch);
        let started = f
            .daemon
            .start_agent(&late, Start::Granted { launch, charge }, 80, 24)
            .await;
        assert!(matches!(started, Ok(Launch::Started(_))));
        for _ in 0..100 {
            if !cwds(&log).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(cwds(&log).len(), 1);
        assert_ne!(status(&f), AgentStatus::Queued);
        f.daemon.kill_all();
    }

    #[tokio::test]
    async fn a_created_row_reaches_clients_before_its_process_can_report_a_status() {
        let (_dir, cmd, _log) = stay();
        let f = queued_row(Some(&cmd));
        let mut inbox = f.daemon.heavy.take_inbox();
        let mut events = f.daemon.events.subscribe();
        let creator = {
            let daemon = f.daemon.clone();
            tokio::spawn(async move {
                daemon
                    .create_agent(
                        &WorktreeId("w".into()),
                        None,
                        "seen",
                        AgentKind::Claude,
                        None,
                        None,
                        false,
                        None,
                    )
                    .await
            })
        };
        let (launch, reply) = tokio::time::timeout(Duration::from_secs(10), inbox.next_admission())
            .await
            .expect("the create never asked for memory");

        let shown = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
            ServerEvent::EntityUpserted {
                entity: Entity::Agent(a),
            } if a.name == "seen" => Some(a.status),
            _ => None,
        });
        assert_eq!(
            shown,
            Some(AgentStatus::Fresh),
            "the row was unknown to clients while its process could start"
        );
        assert!(reply
            .send(Admission::Admitted(f.daemon.heavy.charge_for(launch)))
            .is_ok());
        creator.await.unwrap().unwrap();
        f.daemon.kill_all();
    }

    #[tokio::test]
    async fn a_launch_handed_over_for_a_row_already_gone_is_given_back() {
        let f = queued_row(None);
        let mut inbox = f.daemon.heavy.take_inbox();
        f.daemon
            .adopt_queued(&AgentId("gone".into()), LeaseId(41))
            .await;
        assert_eq!(inbox.released(), vec![Consumer::SessionLaunch(LeaseId(41))]);
    }

    async fn live_finished_row() -> (tempfile::TempDir, Fixture, crate::heavy::Inbox) {
        let (dir, cmd, _log) = stay();
        let f = queued_row(Some(&cmd));
        finished(&f);
        let mut inbox = f.daemon.heavy.take_inbox();
        let start = attach_in_background(&f);
        admit(&f, &mut inbox).await;
        start.await.unwrap().unwrap();
        f.daemon
            .override_hook_status(&f.agent, AgentStatus::Finished);
        (dir, f, inbox)
    }

    #[tokio::test]
    async fn a_restart_resets_the_old_turn_before_its_new_process_is_exposed() {
        let (_dir, f, mut inbox) = live_finished_row().await;
        let mut events = f.daemon.events.subscribe();
        let restart = {
            let (daemon, agent) = (f.daemon.clone(), f.agent.clone());
            tokio::spawn(async move { daemon.restart_agent(&agent).await })
        };
        admit(&f, &mut inbox).await;
        restart.await.unwrap().unwrap();
        reset_before_exposure(&mut events, &f.agent);
    }

    #[tokio::test]
    async fn a_move_resets_the_old_turn_before_its_new_process_is_exposed() {
        let (_dir, f, mut inbox) = live_finished_row().await;
        let (feat_id, _) = add_feat(&f);
        let mut events = f.daemon.events.subscribe();
        let mover = {
            let (daemon, agent) = (f.daemon.clone(), f.agent.clone());
            tokio::spawn(async move { daemon.move_agent(&agent, &feat_id).await })
        };
        admit(&f, &mut inbox).await;
        mover.await.unwrap().unwrap();
        reset_before_exposure(&mut events, &f.agent);
    }
}
