//! Source-side guarded handoff, correlated by the RPC's request id.
use crate::registry::Daemon;
use anyhow::{bail, Context, Result};
use nebula_core::{AgentId, AgentStatus, WorktreeId};
use std::time::Duration;

/// Internal ordered source events; never reconstructed from a relay cache.
#[derive(Clone)]
pub(crate) enum CollectEvent {
    Prompt(AgentId),
    Status(AgentId, AgentStatus),
}

// Drop releases a reservation on success, failure, timeout, or cancellation.
struct Reservation<'a>(&'a Daemon, AgentId);
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.0.collect_owners.lock().unwrap().remove(&self.1);
    }
}

impl Daemon {
    /// Submit only to an unchanged finished owner and acknowledge only after
    /// an accepted new UserPromptSubmit hook followed by a completed turn.
    /// All status decisions are made on the source daemon, not relay caches.
    pub async fn collect_handoff(
        &self,
        id: &AgentId,
        worktree: &WorktreeId,
        branch: &str,
        revision: i64,
        prompt: &str,
        timeout: Duration,
    ) -> Result<()> {
        if !self.collect_owners.lock().unwrap().insert(id.clone()) {
            bail!("another collect request owns this agent; wait for it to finish");
        }
        let _reservation = Reservation(self, id.clone());
        tokio::time::timeout(timeout, async {
            self.collect_input(id, worktree, branch, revision, format!("\x1b[200~{prompt}\x1b[201~").as_bytes())?;
            tokio::time::sleep(Duration::from_millis(150)).await;
            // Subscribe at the source immediately before submission. Old
            // finished statuses, even still in a relay, cannot satisfy this.
            let mut events = self.collect_events.subscribe();
            self.collect_input(id, worktree, branch, revision, b"\r")
                .context("source changed after paste; inspect any unsent input before retrying")?;
            let mut submitted = false;
            loop {
                match events.recv().await.context("lost source events; handoff completion is uncertain")? {
                    CollectEvent::Prompt(agent) if agent == *id => submitted = true,
                    CollectEvent::Status(agent, AgentStatus::Finished) if agent == *id && submitted => return Ok(()),
                    CollectEvent::Status(agent, AgentStatus::NeedsFeedback | AgentStatus::Disconnected | AgentStatus::Terminated) if agent == *id =>
                        bail!("owner needs feedback or disconnected; inspect its session before retrying"),
                    _ => {},
                }
            }
        }).await.context("handoff timed out without a new prompt submission and completion; inspect the owner before retrying")?
    }
}
