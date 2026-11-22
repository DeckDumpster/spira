//! The revision of the store the scheduler's in-process copy has applied, readable by any task.

use std::sync::atomic::{AtomicU64, Ordering};

pub trait Changes: Send + Sync {
    /// The newest revision applied; `None` while nothing is loaded.
    fn rev(&self) -> Option<u64>;
}

/// A revision that only moves forward and reads `None` until first set.
#[derive(Default)]
pub struct AppliedRev(AtomicU64);

/// [`Changes`] fed by a script under virtual time.
pub type ScriptedChanges = AppliedRev;

impl AppliedRev {
    pub fn set(&self, rev: u64) {
        self.0.fetch_max(rev.saturating_add(1), Ordering::SeqCst);
    }
}

impl Changes for AppliedRev {
    fn rev(&self) -> Option<u64> {
        self.0.load(Ordering::SeqCst).checked_sub(1)
    }
}
