//! A backend that records each read it serves, by kind, prefix and limit.

use state_store::{Backend, MemBackend};

/// Each read a backend served, by kind, prefix and limit.
#[derive(Default)]
pub(crate) struct Counting<B = MemBackend> {
    pub inner: B,
    pub reads: std::sync::Mutex<Vec<(&'static str, String, usize)>>,
}

impl<B> Counting<B> {
    pub fn new(inner: B) -> Self {
        Counting { inner, reads: Default::default() }
    }
    pub fn note(&self, kind: &'static str, key: &str, limit: usize) {
        self.reads.lock().unwrap().push((kind, key.to_string(), limit));
    }
    pub fn take(&self) -> Vec<(&'static str, String, usize)> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

impl<B: Backend> Backend for Counting<B> {
    fn head(&self) -> state_store::Result<u64> {
        self.inner.head()
    }
    fn due(&self, now: u64, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
        self.inner.due(now, limit, at)
    }

    fn owned_by(&self, owner: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
        self.inner.owned_by(owner, after, limit, at)
    }
    fn get(&self, key: &str, at: u64) -> state_store::Result<Option<state_store::Version>> {
        self.note("get", key, 1);
        self.inner.get(key, at)
    }
    fn get_many(&self, keys: &[&str], at: u64) -> state_store::Result<Vec<Option<state_store::Version>>> {
        self.note("get_many", keys.first().copied().unwrap_or(""), keys.len());
        self.inner.get_many(keys, at)
    }
    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.note("range", prefix, limit);
        self.inner.range(prefix, after, limit, at)
    }
    fn range_in_state(&self, prefix: &str, state: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.note("range_in_state", &format!("{prefix}{state}"), limit);
        self.inner.range_in_state(prefix, state, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[state_store::Write]) -> state_store::Result<u64> {
        self.inner.commit(expect, batch)
    }
    fn log(&self, from: u64, max_mains: usize) -> state_store::Result<Vec<state_store::LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> state_store::Result<()> {
        self.inner.compact(rev)
    }
    fn compacted(&self) -> state_store::Result<u64> {
        self.inner.compacted()
    }
}

