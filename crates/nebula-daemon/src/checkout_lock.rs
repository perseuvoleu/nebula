//! Orders a physical change to one checkout (primary switch, removal)
//! against sessions spawning or moving into that same checkout. Each
//! checkout has its own lock, so a slow delete of one worktree never
//! blocks sessions in the others.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use nebula_core::WorktreeId;
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

#[derive(Default)]
pub struct CheckoutLocks {
    slots: Mutex<HashMap<WorktreeId, Slot>>,
}

#[derive(Clone, Default)]
struct Slot {
    lock: Arc<RwLock<()>>,
    op: Arc<Mutex<Option<String>>>,
}

/// Held while a checkout's files change; names the change for refusals.
pub struct Changing {
    _guard: OwnedRwLockWriteGuard<()>,
    op: Arc<Mutex<Option<String>>>,
}

impl Drop for Changing {
    fn drop(&mut self) {
        *self.op.lock().unwrap() = None;
    }
}

impl CheckoutLocks {
    fn slot(&self, id: &WorktreeId) -> Slot {
        self.slots
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_default()
            .clone()
    }

    /// Exclusive: waits for in-flight session actions in this checkout.
    pub async fn change(&self, id: &WorktreeId, op: String) -> Changing {
        let slot = self.slot(id);
        let guard = slot.lock.write_owned().await;
        *slot.op.lock().unwrap() = Some(op);
        Changing {
            _guard: guard,
            op: slot.op,
        }
    }

    /// Shared, waiting for a change in progress to finish.
    pub async fn session(&self, id: &WorktreeId) -> OwnedRwLockReadGuard<()> {
        self.slot(id).lock.read_owned().await
    }

    /// Shared, refusing while this checkout is changing; `action` ends the message.
    pub fn try_session(&self, id: &WorktreeId, action: &str) -> Result<OwnedRwLockReadGuard<()>> {
        let slot = self.slot(id);
        slot.lock.try_read_owned().map_err(|_| {
            let op = slot.op.lock().unwrap().clone();
            let op = op.as_deref().unwrap_or("a checkout change");
            anyhow::anyhow!("{op} is in progress; {action}")
        })
    }
}
