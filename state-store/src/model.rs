//! `Modeled<B>`: a backend that serves reads from an in-memory copy of the store, kept by its own
//! commits. Every batch that commits through it is applied to the copy before `commit` returns,
//! so a read that starts after a write returned sees that write: the copy is never polled and
//! never lags its own writes. A read is served from memory only at a revision the copy holds
//! whole — `floor <= at <= rev` — and goes to the inner backend otherwise, so a read answered
//! here is the answer the inner backend would have given. Compare-and-set stays the inner
//! backend's: `commit` is its `commit`, guarded by its own head.
//!
//! The copy holds every key outside the prefixes it is told to skip (the audit history and the
//! other keyspaces that grow without bound), with the versions of the last `window` revisions,
//! so a reader paging one snapshot while commits land keeps reading from memory.
//!
//! [`track`] stamps a request with the revision its reads and writes reflect, and
//! [`Modeled::wait_for`] lets a caller that needs its own write wait for the copy to reach it.

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::ops::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::backend::{state_of, Backend, LogEntry, Version, Write};
use crate::{Error, Result, Rev};

/// One key's version as of a revision; `None` is a delete.
struct Slot {
    version: Version,
    /// The value's `data.state`, read once when the version is applied.
    state: Option<String>,
}

type Versions = Vec<(u64, Option<Slot>)>;

struct Model {
    /// Every commit at or below `rev` is applied.
    rev: u64,
    /// Every key's version at `floor` and every later one is held: reads in `floor..=rev`.
    floor: u64,
    keys: BTreeMap<String, Versions>,
    /// Deleted keys, by the revision of their delete, to forget once the floor passes it.
    deleted: VecDeque<(u64, String)>,
    /// Bytes of the values held, for the operator's log line.
    bytes: usize,
}

/// What a request's reads and writes reflected (see [`track`]).
#[derive(Clone, Copy, Default)]
struct Seen {
    on: bool,
    /// The lowest revision any read was made at.
    read: Option<u64>,
    /// The highest revision any commit made.
    wrote: Option<u64>,
    /// Every read goes to the inner backend.
    through: bool,
}

thread_local! {
    static SEEN: Cell<Seen> = const { Cell::new(Seen { on: false, read: None, wrote: None, through: false }) };
}

fn seen(f: impl FnOnce(&mut Seen)) {
    SEEN.with(|s| {
        let mut v = s.get();
        if v.on {
            f(&mut v);
            s.set(v);
        }
    });
}

fn through() -> bool {
    SEEN.with(|s| s.get().through)
}

/// Starts recording, on this thread, the revisions the request's reads and writes are made at;
/// `read_through` sends every read of the request to the inner backend. [`stamp`] ends it.
pub fn track(read_through: bool) {
    SEEN.with(|s| s.set(Seen { on: true, read: None, wrote: None, through: read_through }));
}

/// The revision the request [`track`] began reflects, and stops recording: its highest commit,
/// or, above that, the lowest revision it read at, so every part of the answer is at least that
/// fresh and a write it made is at or below it. `None` when it touched no store.
pub fn stamp() -> Option<u64> {
    SEEN.with(|s| {
        let v = s.replace(Seen::default());
        match (v.read, v.wrote) {
            (None, None) => None,
            (r, w) => Some(r.unwrap_or(0).max(w.unwrap_or(0))),
        }
    })
}

/// Reads per page while the copy is loaded.
const LOAD_PAGE: usize = 1_000;
/// Watch-log mains per page while the copy catches up with commits it did not make.
const CATCH_UP_MAINS: usize = 256;

pub struct Modeled<B: Backend> {
    inner: B,
    skip: Vec<String>,
    window: u64,
    model: RwLock<Model>,
    /// Held across a commit and its apply, so commits reach the copy in revision order.
    order: Mutex<()>,
    /// The copy's revision, for [`Modeled::wait_for`].
    published: Mutex<u64>,
    moved: Condvar,
    from_memory: AtomicU64,
    from_inner: AtomicU64,
}

fn visible(versions: &Versions, at: u64) -> Option<&Slot> {
    versions
        .iter()
        .rev()
        .find(|(m, _)| *m <= at)
        .and_then(|(_, s)| s.as_ref())
}

fn slot(value: &[u8], mod_rev: Rev, create_rev: Rev) -> Slot {
    Slot {
        state: state_of(value),
        version: Version {
            value: value.to_vec(),
            mod_rev,
            create_rev,
        },
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Model {
    fn holds(&self, at: u64) -> bool {
        self.floor <= at && at <= self.rev
    }

    /// `slot` as `key`'s version at `main`, dropping every version the floor no longer reads.
    fn put(&mut self, key: &str, main: u64, slot: Option<Slot>) {
        let floor = self.floor;
        let versions = self.keys.entry(key.to_string()).or_default();
        self.bytes += slot.as_ref().map_or(0, |s| s.version.value.len());
        if slot.is_none() {
            self.deleted.push_back((main, key.to_string()));
        }
        versions.push((main, slot));
        let keep_from = versions.iter().rposition(|(m, _)| *m <= floor).unwrap_or(0);
        for (_, s) in versions.drain(..keep_from) {
            self.bytes -= s.map_or(0, |s| s.version.value.len());
        }
    }

    /// Raises the floor to `floor`, forgetting the deleted keys no read can see any more.
    fn raise_floor(&mut self, floor: u64) {
        self.floor = self.floor.max(floor);
        while self.deleted.front().is_some_and(|(m, _)| *m <= self.floor) {
            let Some((_, key)) = self.deleted.pop_front() else { break };
            let gone = self.keys.get(&key).is_some_and(|v| {
                v.iter().rposition(|(m, _)| *m <= self.floor).is_some_and(|i| i == v.len() - 1 && v[i].1.is_none())
            });
            if gone {
                if let Some(v) = self.keys.remove(&key) {
                    self.bytes -= v.into_iter().map(|(_, s)| s.map_or(0, |s| s.version.value.len())).sum::<usize>();
                }
            }
        }
    }
}

impl<B: Backend> Modeled<B> {
    /// `inner` with a copy of everything outside `skip` at its head, holding `window` revisions.
    pub fn open(inner: B, skip: &[&str], window: u64) -> Result<Self> {
        let m = Modeled {
            inner,
            skip: skip.iter().map(|s| s.to_string()).collect(),
            window,
            model: RwLock::new(Model { rev: 0, floor: 0, keys: BTreeMap::new(), deleted: VecDeque::new(), bytes: 0 }),
            order: Mutex::new(()),
            published: Mutex::new(0),
            moved: Condvar::new(),
            from_memory: AtomicU64::new(0),
            from_inner: AtomicU64::new(0),
        };
        let _order = lock(&m.order);
        m.reload()?;
        drop(_order);
        Ok(m)
    }

    pub fn inner(&self) -> &B {
        &self.inner
    }

    /// The revision every commit at or below which the copy holds.
    pub fn revision(&self) -> u64 {
        self.read_model().rev
    }

    /// The keys and value bytes the copy holds.
    pub fn size(&self) -> (usize, usize) {
        let m = self.read_model();
        (m.keys.len(), m.bytes)
    }

    /// Reads answered from memory, and reads sent to the inner backend, since the copy opened.
    pub fn reads(&self) -> (u64, u64) {
        (self.from_memory.load(Ordering::Relaxed), self.from_inner.load(Ordering::Relaxed))
    }

    /// Waits up to `timeout` for the copy to reach `rev`; whether it did.
    pub fn wait_for(&self, rev: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut at = lock(&self.published);
        while *at < rev {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            at = self.moved.wait_timeout(at, left).unwrap_or_else(|p| p.into_inner()).0;
        }
        true
    }

    fn read_model(&self) -> std::sync::RwLockReadGuard<'_, Model> {
        self.model.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write_model(&self) -> std::sync::RwLockWriteGuard<'_, Model> {
        self.model.write().unwrap_or_else(|p| p.into_inner())
    }

    fn skipped(&self, key: &str) -> bool {
        self.skip.iter().any(|s| key.starts_with(s.as_str()))
    }

    /// Whether a range over `prefix` may reach a skipped key.
    fn overlaps_skipped(&self, prefix: &str) -> bool {
        self.skip.iter().any(|s| s.starts_with(prefix) || prefix.starts_with(s.as_str()))
    }

    fn publish(&self, rev: u64) {
        *lock(&self.published) = rev;
        self.moved.notify_all();
    }

    /// The whole copy again, at the inner backend's head. Called under `order`.
    fn reload(&self) -> Result<()> {
        let head = self.inner.head()?;
        let mut keys = BTreeMap::new();
        let mut bytes = 0;
        let mut after: Option<String> = None;
        loop {
            let page = self.inner.gc_range(after.as_deref(), LOAD_PAGE, head)?;
            let full = page.len() == LOAD_PAGE;
            after = page.last().map(|(k, _)| k.clone());
            for (k, v) in page {
                if self.skipped(&k) {
                    continue;
                }
                bytes += v.value.len();
                let s = Slot { state: state_of(&v.value), version: v };
                keys.insert(k, vec![(s.version.mod_rev.main, Some(s))]);
            }
            if !full {
                break;
            }
        }
        *self.write_model() = Model { rev: head, floor: head, keys, deleted: VecDeque::new(), bytes };
        self.publish(head);
        Ok(())
    }

    /// Applies the commits the inner backend holds and the copy does not, from its watch log;
    /// a log compacted past the copy reloads it. Called under `order`.
    fn catch_up(&self) -> Result<()> {
        loop {
            let (rev, head) = (self.revision(), self.inner.head()?);
            if rev >= head {
                return Ok(());
            }
            let log = match self.inner.log(rev, CATCH_UP_MAINS) {
                Err(Error::TooOld { .. }) => return self.reload(),
                r => r?,
            };
            let Some(through) = log.last().map(|e| e.rev.main) else {
                // Revisions the store skipped (a jump): nothing was written in them.
                self.advance(head, |_| {});
                return Ok(());
            };
            self.advance(through, |m| {
                for e in log.into_iter().filter(|e| !self.skipped(&e.key)) {
                    let LogEntry { key, rev, value, create_rev, .. } = e;
                    let s = value.map(|v| slot(&v, rev, create_rev.unwrap_or(rev)));
                    m.put(&key, rev.main, s);
                }
            });
        }
    }

    /// Moves the copy to `rev` with `apply`'s writes, then wakes every waiter.
    fn advance(&self, rev: u64, apply: impl FnOnce(&mut Model)) {
        {
            let mut m = self.write_model();
            m.raise_floor(rev.saturating_sub(self.window));
            apply(&mut m);
            m.rev = m.rev.max(rev);
        }
        self.publish(rev);
    }

    fn served(&self, at: u64) {
        self.from_memory.fetch_add(1, Ordering::Relaxed);
        seen(|s| s.read = Some(s.read.map_or(at, |r| r.min(at))));
    }

    fn passed(&self, at: u64) {
        self.from_inner.fetch_add(1, Ordering::Relaxed);
        seen(|s| s.read = Some(s.read.map_or(at, |r| r.min(at))));
    }

    /// Up to `limit` visible keys under `prefix` after `after` at `at`, with `state` when given.
    fn scan(m: &Model, prefix: &str, state: Option<&str>, after: Option<&str>, limit: usize, at: u64) -> Vec<(String, Version)> {
        let start = match after {
            Some(a) if a >= prefix => Bound::Excluded(a.to_string()),
            _ => Bound::Included(prefix.to_string()),
        };
        m.keys
            .range((start, Bound::Unbounded))
            .take_while(|(k, _)| k.starts_with(prefix))
            .filter_map(|(k, v)| visible(v, at).map(|s| (k, s)))
            .filter(|(_, s)| state.is_none_or(|st| s.state.as_deref() == Some(st)))
            .take(limit)
            .map(|(k, s)| (k.clone(), s.version.clone()))
            .collect()
    }

    fn ranged(
        &self,
        prefix: &str,
        state: Option<&str>,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Option<Vec<(String, Version)>> {
        if through() || self.overlaps_skipped(prefix) {
            return None;
        }
        let m = self.read_model();
        if !m.holds(at) {
            return None;
        }
        let out = Self::scan(&m, prefix, state, after, limit, at);
        drop(m);
        self.served(at);
        Some(out)
    }
}

impl<B: Backend> Backend for Modeled<B> {
    fn head(&self) -> Result<u64> {
        self.inner.head()
    }

    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        if !through() && !self.skipped(key) {
            let m = self.read_model();
            if m.holds(at) {
                let v = m.keys.get(key).and_then(|v| visible(v, at)).map(|s| s.version.clone());
                drop(m);
                self.served(at);
                return Ok(v);
            }
        }
        self.passed(at);
        self.inner.get(key, at)
    }

    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        if let Some(out) = self.ranged(prefix, None, after, limit, at) {
            return Ok(out);
        }
        self.passed(at);
        self.inner.range(prefix, after, limit, at)
    }

    fn range_in_state(
        &self,
        prefix: &str,
        state: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        if let Some(out) = self.ranged(prefix, Some(state), after, limit, at) {
            return Ok(out);
        }
        self.passed(at);
        self.inner.range_in_state(prefix, state, after, limit, at)
    }

    /// The inner backend's commit, applied to the copy before it returns. A commit whose
    /// outcome the inner backend could not report is read back from its log.
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        let _order = lock(&self.order);
        if self.revision() != self.inner.head()? {
            let _ = self.catch_up();
        }
        let res = self.inner.commit(expect, batch);
        match &res {
            Ok(rev) if self.revision() == expect => {
                let rev = *rev;
                self.advance(rev, |m| {
                    for (i, w) in batch.iter().enumerate().filter(|(_, w)| !self.skipped(w.key())) {
                        let mod_rev = Rev::new(rev, i as u32);
                        let s = match w {
                            Write::Put { value, create_rev, .. } => Some(slot(value, mod_rev, *create_rev)),
                            Write::Delete { .. } => None,
                        };
                        m.put(w.key(), rev, s);
                    }
                });
                seen(|s| s.wrote = Some(s.wrote.map_or(rev, |w| w.max(rev))));
            }
            Ok(rev) => {
                let rev = *rev;
                let _ = self.catch_up();
                seen(|s| s.wrote = Some(s.wrote.map_or(rev, |w| w.max(rev))));
            }
            Err(Error::Stale { .. }) => {}
            Err(_) => {
                let _ = self.catch_up();
            }
        }
        res
    }

    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        self.inner.log(from, max_mains)
    }

    /// The inner backend's compaction; the copy stops answering below it.
    fn compact(&self, rev: u64) -> Result<()> {
        let _order = lock(&self.order);
        self.inner.compact(rev)?;
        self.write_model().raise_floor(rev);
        Ok(())
    }

    fn gc_range(&self, after: Option<&str>, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        self.passed(at);
        self.inner.gc_range(after, limit, at)
    }
}

#[cfg(test)]
mod tests;
