//! The revision of the store the scheduler's in-process copy has applied, readable by any task.

use std::sync::atomic::{AtomicU64, Ordering};

pub trait Changes: Send + Sync {
    /// The newest revision applied; `None` while nothing is loaded.
    fn rev(&self) -> Option<u64>;
}

/// [`Changes`] fed by a script under virtual time.
#[derive(Default)]
pub struct ScriptedChanges(AtomicU64);

impl ScriptedChanges {
    pub fn set(&self, rev: u64) {
        self.0.fetch_max(rev.saturating_add(1), Ordering::SeqCst);
    }
}

impl Changes for ScriptedChanges {
    fn rev(&self) -> Option<u64> {
        self.0.load(Ordering::SeqCst).checked_sub(1)
    }
}
