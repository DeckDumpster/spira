//! The seam every store sits behind. It knows nothing of txn, history, TTLs or locks: it reads at
//! a snapshot revision, commits a batch atomically and durably if the head is still the one the
//! caller read, serves the watch log and compacts. The contract computes every revision it stores.

use crate::{Result, Rev};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub value: Vec<u8>,
    pub mod_rev: Rev,
    pub create_rev: Rev,
}

/// One key of a batch; its `sub` is its index in the batch, its `main` the committed head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    Put {
        key: String,
        value: Vec<u8>,
        create_rev: Rev,
        prev: Option<Rev>,
    },
    Delete {
        key: String,
        prev: Rev,
    },
}

impl Write {
    pub fn key(&self) -> &str {
        match self {
            Write::Put { key, .. } | Write::Delete { key, .. } => key,
        }
    }
}

/// A committed write as the watch log serves it; `value` is `None` for a delete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub key: String,
    pub rev: Rev,
    pub value: Option<Vec<u8>>,
    pub create_rev: Option<Rev>,
    pub prev: Option<Rev>,
}

pub trait Backend: Send + Sync {
    /// The last committed main revision, kept in its own meta entry: never derived from keys.
    fn head(&self) -> Result<u64>;
    /// The key as of `at`; `TooOld` below the compaction point, `FutureRev` above the head.
    fn get(&self, key: &str, at: u64) -> Result<Option<Version>>;
    /// Keys starting with `prefix`, strictly after `after`, in key order, at most `limit`.
    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>>;
    /// Applies `batch` as main `expect + 1`, or `Stale` if the head is no longer `expect`.
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64>;
    /// Whole txns with `main > from`, at most `max_mains` of them; `TooOld` if `from` is below
    /// the compaction point.
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>>;
    /// Drops the watch log and superseded versions at or below `rev`.
    fn compact(&self, rev: u64) -> Result<()>;
}
