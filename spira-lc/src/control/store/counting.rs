//! A backend that records each read it serves, for tests that hold a query to its bound.

use state_store::{Backend, MemBackend};

/// Each read a backend served, by kind, prefix and limit: what a list's reads are held to.
#[derive(Default)]
pub(crate) struct Counting {
    pub(crate) inner: MemBackend,
    pub(crate) reads: std::sync::Mutex<Vec<(&'static str, String, usize)>>,
    pub(crate) many_calls: std::sync::Mutex<Vec<String>>,
    pub(crate) widths: std::sync::Mutex<Vec<(&'static str, usize)>>,
}

impl Counting {
    pub(crate) fn note(&self, kind: &'static str, key: &str, limit: usize) {
        self.reads.lock().unwrap().push((kind, key.to_string(), limit));
    }
    fn width(&self, kind: &'static str, keys: usize) {
        self.widths.lock().unwrap().push((kind, keys));
    }
    /// The keys or rows each read call asked for, by kind, since the last call: a range's `limit`,
    /// not the rows it returned.
    pub(crate) fn take_widths(&self) -> Vec<(&'static str, usize)> {
        std::mem::take(&mut *self.widths.lock().unwrap())
    }
    pub(crate) fn take_many_calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.many_calls.lock().unwrap())
    }
    pub(crate) fn take(&self) -> Vec<(&'static str, String, usize)> {
        std::mem::take(&mut *self.reads.lock().unwrap())
    }
}

impl Backend for Counting {
    fn head(&self) -> state_store::Result<u64> {
        self.inner.head()
    }
    fn due(&self, now: u64, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
        self.inner.due(now, limit, at)
    }

    fn owned_by(&self, owner: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
        self.inner.owned_by(owner, after, limit, at)
    }
    fn range_newest_opened(&self, prefix: &str, since: i64, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.note("range_newest_opened", prefix, limit);
        self.inner.range_newest_opened(prefix, since, limit, at)
    }
    fn get(&self, key: &str, at: u64) -> state_store::Result<Option<state_store::Version>> {
        self.note("get", key, 1);
        self.width("get", 1);
        self.inner.get(key, at)
    }
    fn get_many(&self, keys: &[&str], at: u64) -> state_store::Result<Vec<Option<state_store::Version>>> {
        if let Some(first) = keys.first() {
            self.many_calls.lock().unwrap().push(first.to_string());
        }
        self.width("get_many", keys.len());
        for k in keys {
            self.note("get_many", k, 1);
        }
        self.inner.get_many(keys, at)
    }
    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.note("range", prefix, limit);
        self.width("range", limit);
        self.inner.range(prefix, after, limit, at)
    }
    fn range_in_state(&self, prefix: &str, state: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
        self.note("range_in_state", &format!("{prefix}{state}"), limit);
        self.width("range_in_state", limit);
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
