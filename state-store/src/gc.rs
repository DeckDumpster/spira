//! The GC controller: one `tick(now)` drains the markers of owners that went, and expires keys
//! whose TTL has run out. A finished key is removed by the write that finished it, so no pass
//! reads the key space.

use crate::backend::Backend;
use crate::contract::{Compaction, Contract, COMPACT_STEP, COMPACT_STEPS, PAGE};
use crate::{Error, Item, Result, ORPHANS_PREFIX};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tick {
    pub orphans: Vec<String>,
    pub expired: Vec<String>,
    /// Audit records cut for being older than [`EVENT_WINDOW_MS`].
    pub trimmed: usize,
    /// Steps that failed this pass; every other step still ran.
    pub failed: Vec<StepFailure>,
    /// What this pass's compaction did, when it took a step.
    pub compaction: Option<Compaction>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepFailure {
    pub step: &'static str,
    pub error: Error,
}

impl std::fmt::Display for StepFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.step, self.error)
    }
}

/// How long the audit history stays online.
pub const EVENT_WINDOW_MS: u64 = 14 * 24 * 60 * 60 * 1_000;
/// Revisions behind the head that compaction leaves, so a read as of a recent revision holds.
pub const COMPACT_KEEP: u64 = 10_000;
/// The most revisions a hot-key count looks back over: what compaction keeps plus one step.
pub const HOT_KEYS_MAX_REVS: u64 = COMPACT_KEEP + crate::contract::COMPACT_STEP;
/// The dependents of gone owners one pass looks at, however many markers name them.
pub const ORPHANS_PER_PASS: usize = 1_024;

pub struct Controller<'a, B: Backend> {
    contract: &'a Contract<B>,
}

fn orphan(i: &Item, alive: &dyn Fn(&str) -> Result<bool>) -> Result<bool> {
    let m = &i.value.metadata;
    if m.owner_references.is_empty() || m.deletion_timestamp.is_some() {
        return Ok(false);
    }
    for o in &m.owner_references {
        if alive(o)? {
            return Ok(false);
        }
    }
    Ok(true)
}

impl<'a, B: Backend> Controller<'a, B> {
    pub fn new(contract: &'a Contract<B>) -> Self {
        Controller { contract }
    }

    /// `now_ms` is on the contract's monotonic clock.
    pub fn tick(&self, now_ms: u64) -> Result<Tick> {
        let mut tick = self.pass(now_ms);
        if tick.failed.is_empty() {
            return Ok(tick);
        }
        Err(Error::Backend(tick.failed.remove(0).to_string()))
    }

    /// Collects the dependents of each owner a marker names, at most [`ORPHANS_PER_PASS`] in all.
    fn drain_markers(&self, collected: &mut Vec<String>) -> Result<()> {
        let c = self.contract;
        let mut budget = ORPHANS_PER_PASS;
        let head = c.head()?;
        for (key, marker) in c.backend().range(ORPHANS_PREFIX, None, PAGE, head)? {
            if budget == 0 {
                break;
            }
            let Some(owner) = key.strip_prefix(ORPHANS_PREFIX) else {
                continue;
            };
            if c.exists_at(owner, c.head()?)? {
                c.settle_marker(&key, marker.mod_rev, None)?;
                continue;
            }
            let after = crate::Object::decode(&marker.value)?
                .data
                .get("after")
                .and_then(|a| a.as_str().map(str::to_string));
            let want = budget;
            let page = c.dependents(owner, after.as_deref(), want)?;
            budget -= page.len();
            collected.extend(c.collect(&page, "gc.orphan", |i, head| {
                orphan(i, &|o| c.exists_at(o, head))
            })?);
            let resume = if page.len() < want { None } else { page.last().cloned() };
            c.settle_marker(&key, marker.mod_rev, resume.as_deref())?;
        }
        Ok(())
    }

    /// Runs every step; a failed step is recorded and the others go ahead.
    pub fn pass(&self, now_ms: u64) -> Tick {
        let c = self.contract;
        let mut tick = Tick::default();
        let fail = |tick: &mut Tick, step: &'static str, error: Error| {
            tick.failed.push(StepFailure { step, error })
        };

        let mut collected = Vec::new();
        let drained = self.drain_markers(&mut collected);
        tick.orphans = collected;
        if let Err(e) = drained {
            fail(&mut tick, "orphan", e);
        }

        let due = c.due(now_ms);
        match c.expire(&due, now_ms) {
            Ok(expired) => tick.expired = expired,
            Err(e) => fail(&mut tick, "expire", e),
        }
        match c.trim_events(EVENT_WINDOW_MS) {
            Ok(trimmed) => tick.trimmed = trimmed,
            Err(e) => fail(&mut tick, "trim", e),
        }
        match c.compact_behind(COMPACT_KEEP, COMPACT_STEP, COMPACT_STEPS) {
            Ok(done) => tick.compaction = Some(done).filter(|d| d.steps > 0),
            Err(e) => fail(&mut tick, "compact", e),
        }
        tick
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use runtime::ports::VirtualClock;
    use serde_json::json;

    use super::*;
    use crate::backend::{Version, Write};
    use crate::contract::{Config, Ports, Request, Txn};
    use crate::{events_key, Error, MemBackend, Object, ORPHANS_PREFIX};

    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;

    fn open<B: Backend>(b: B, clock: &VirtualClock) -> Contract<B> {
        Contract::open(
            b,
            Ports::virtual_clock(clock),
            Config {
                min_lock_ttl_ms: 1_000,
            },
        )
        .unwrap()
    }

    fn write<B: Backend>(c: &Contract<B>, key: &str, id: &str) {
        c.txn(
            &Txn::new().put(key, Object::new(json!(1))),
            &Request::new(id, "t", "test.write"),
        )
        .unwrap();
    }

    fn events(c: &Contract<impl Backend>) -> usize {
        let head = c.head().unwrap();
        c.backend()
            .range(crate::EVENTS_PREFIX, None, usize::MAX, head)
            .unwrap()
            .len()
    }

    #[test]
    fn audit_history_older_than_the_window_is_cut_in_bounded_passes_and_the_rest_stays() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(MemBackend::new(), &clock);
        for i in 0..2_500 {
            write(&c, &format!("rec/{i}"), &format!("old{i}"));
        }
        clock.advance(EVENT_WINDOW_MS + DAY_MS);
        for i in 0..5 {
            write(&c, &format!("new/{i}"), &format!("new{i}"));
        }
        assert_eq!(events(&c), 2_505);
        let gc = Controller::new(&c);
        let first = gc.tick(c.now_ms()).unwrap().trimmed;
        assert!(first > 0 && first < 2_500, "one pass is bounded: {first}");
        assert_eq!(events(&c), 2_505 - first);
        let mut passes = 1;
        while gc.tick(c.now_ms()).unwrap().trimmed > 0 {
            passes += 1;
        }
        assert!(passes >= 2, "{passes}");
        assert_eq!(events(&c), 5, "only the events inside the window remain");
    }

    #[test]
    fn nothing_is_cut_from_a_history_that_is_all_inside_the_window() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(MemBackend::new(), &clock);
        write(&c, "rec/a", "1");
        clock.advance(EVENT_WINDOW_MS - 1);
        assert_eq!(Controller::new(&c).tick(c.now_ms()).unwrap().trimmed, 0);
        assert_eq!(events(&c), 1);
    }

    #[test]
    fn unstamped_history_is_as_old_as_the_first_stamped_record() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let b = MemBackend::new();
        let legacy = Object::new(json!({"actor": "x", "subs": []})).encode();
        for n in 1..=3u64 {
            let w = Write::Put {
                key: events_key(n),
                value: legacy.clone(),
                create_rev: crate::Rev::new(n, 0),
                prev: None,
            };
            b.commit(n - 1, &[w]).unwrap();
        }
        let c = open(b, &clock);
        let gc = Controller::new(&c);
        assert_eq!(
            gc.tick(c.now_ms()).unwrap().trimmed,
            0,
            "no stamped record to date them by"
        );
        write(&c, "rec/a", "1");
        clock.advance(EVENT_WINDOW_MS + DAY_MS);
        assert_eq!(gc.tick(c.now_ms()).unwrap().trimmed, 4);
        assert_eq!(events(&c), 0);
    }

    #[test]
    fn superseded_versions_are_compacted_in_steps_behind_the_head() {
        let clock = VirtualClock::new(0);
        let c = open(MemBackend::new(), &clock);
        for i in 0..1_000 {
            write(&c, "rec/a", &format!("r{i}"));
        }
        c.compact_behind(100, 200, 2).unwrap();
        assert_eq!(c.backend().compacted().unwrap(), 400);
        c.compact_behind(100, 200, 10).unwrap();
        assert_eq!(c.backend().compacted().unwrap(), c.head().unwrap() - 100);
    }

    #[test]
    fn a_compaction_over_many_steps_completes_across_passes() {
        let clock = VirtualClock::new(0);
        let c = open(MemBackend::new(), &clock);
        let (keep, step) = (50, 20);
        for i in 0..keep + 10 * step {
            write(&c, "rec/a", &format!("r{i}"));
        }
        let target = c.head().unwrap() - keep;
        let mut passes = Vec::new();
        while c.backend().compacted().unwrap() < target {
            let done = c.compact_behind(keep, step, 3).unwrap();
            assert!(done.steps <= 3);
            assert_eq!(done.to, c.backend().compacted().unwrap());
            assert_eq!(done.target, target);
            assert!(!done.yielded);
            passes.push(done.steps);
            assert!(passes.len() <= 4, "no progress: {passes:?}");
        }
        assert_eq!(passes, [3, 3, 3, 1]);
        let idle = c.compact_behind(keep, step, 3).unwrap();
        assert_eq!((idle.steps, idle.from, idle.to), (0, target, target));
    }

    /// A backend whose `compact` and `commit` exclude each other, as the store's writer does,
    /// logging each call; its first `compact` runs `during` before it releases, and a commit
    /// holds the lock until `release` is set.
    struct Excluding {
        inner: MemBackend,
        lock: std::sync::Mutex<()>,
        log: std::sync::Mutex<Vec<&'static str>>,
        during: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
        release: std::sync::atomic::AtomicBool,
    }

    impl Backend for Excluding {
        fn head(&self) -> crate::Result<u64> {
            self.inner.head()
        }
        fn get(&self, key: &str, at: u64) -> crate::Result<Option<Version>> {
            self.inner.get(key, at)
        }
        fn range(
            &self,
            prefix: &str,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<(String, Version)>> {
            self.inner.range(prefix, after, limit, at)
        }
        fn commit(&self, expect: u64, batch: &[Write]) -> crate::Result<u64> {
            let _held = self.lock.lock().unwrap();
            self.log.lock().unwrap().push("commit");
            while !self.release.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            self.inner.commit(expect, batch)
        }
        fn log(&self, from: u64, max_mains: usize) -> crate::Result<Vec<crate::LogEntry>> {
            self.inner.log(from, max_mains)
        }
        fn compact(&self, rev: u64) -> crate::Result<()> {
            let _held = self.lock.lock().unwrap();
            self.log.lock().unwrap().push("compact");
            if let Some(during) = self.during.lock().unwrap().take() {
                during();
            }
            self.inner.compact(rev)
        }
        fn compacted(&self) -> crate::Result<u64> {
            self.inner.compacted()
        }
        fn gc_range(
            &self,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<(String, Version)>> {
            self.inner.gc_range(after, limit, at)
        }
        fn owned_by(
            &self,
            owner: &str,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<String>> {
            self.inner.owned_by(owner, after, limit, at)
        }
    }

    #[test]
    fn a_commit_waiting_between_steps_is_applied_before_the_next_step() {
        use std::sync::Arc;
        let clock = VirtualClock::new(0);
        let c = Arc::new(open(
            Excluding {
                inner: MemBackend::new(),
                lock: Default::default(),
                log: Default::default(),
                during: Default::default(),
                release: AtomicBool::new(true),
            },
            &clock,
        ));
        for i in 0..100 {
            write(&c, "rec/a", &format!("r{i}"));
        }
        c.backend().release.store(false, Ordering::SeqCst);
        c.backend().log.lock().unwrap().clear();
        let writer = Arc::new(std::sync::Mutex::new(None));
        let (c2, slot) = (Arc::clone(&c), Arc::clone(&writer));
        let during: Box<dyn FnOnce() + Send> = Box::new(move || {
            let c3 = Arc::clone(&c2);
            let handle = std::thread::spawn(move || write(&c3, "rec/b", "late"));
            while c2.writers() == 0 {
                std::thread::yield_now();
            }
            *slot.lock().unwrap() = Some(handle);
        });
        *c.backend().during.lock().unwrap() = Some(during);
        let first = c.compact_behind(10, 20, 5).unwrap();
        assert!(first.yielded);
        assert_eq!(first.steps, 1);
        c.backend().release.store(true, Ordering::SeqCst);
        writer.lock().unwrap().take().unwrap().join().unwrap();
        assert_eq!(*c.backend().log.lock().unwrap(), ["compact", "commit"]);
        let second = c.compact_behind(10, 20, 1).unwrap();
        assert_eq!(second.from, first.to);
        assert_eq!(second.steps, 1);
        assert!(!second.yielded);
        assert_eq!(*c.backend().log.lock().unwrap(), ["compact", "commit", "compact"]);
    }

    /// Refuses `range` over `events/` with a plain error once armed, and counts the reads that
    /// visit the key space: `gc_range`, `range_outside`, and a `range` of any prefix but
    /// `events/` or `orphans/`.
    struct Flaky {
        inner: MemBackend,
        refuse_events: AtomicBool,
        refuse_markers: AtomicBool,
        stale_owned_by: AtomicUsize,
        key_space_reads: AtomicUsize,
    }

    impl Flaky {
        fn new() -> Self {
            Flaky {
                inner: MemBackend::new(),
                refuse_events: Default::default(),
                refuse_markers: Default::default(),
                stale_owned_by: AtomicUsize::new(0),
                key_space_reads: AtomicUsize::new(0),
            }
        }
    }

    impl Backend for Flaky {
        fn head(&self) -> crate::Result<u64> {
            self.inner.head()
        }
        fn get(&self, key: &str, at: u64) -> crate::Result<Option<Version>> {
            self.inner.get(key, at)
        }
        fn range(
            &self,
            prefix: &str,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<(String, Version)>> {
            if prefix == crate::EVENTS_PREFIX && self.refuse_events.load(Ordering::SeqCst) {
                return Err(Error::Backend("refused".into()));
            }
            if prefix == ORPHANS_PREFIX && self.refuse_markers.load(Ordering::SeqCst) {
                return Err(Error::Backend("refused".into()));
            }
            if prefix != crate::EVENTS_PREFIX && prefix != ORPHANS_PREFIX {
                self.key_space_reads.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.range(prefix, after, limit, at)
        }
        fn owned_by(
            &self,
            owner: &str,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<String>> {
            let moved = self
                .stale_owned_by
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok();
            if moved {
                return Err(Error::Stale { expected: at, head: at + 1 });
            }
            self.inner.owned_by(owner, after, limit, at)
        }
        fn commit(&self, expect: u64, batch: &[Write]) -> crate::Result<u64> {
            self.inner.commit(expect, batch)
        }
        fn log(&self, from: u64, max_mains: usize) -> crate::Result<Vec<crate::LogEntry>> {
            self.inner.log(from, max_mains)
        }
        fn compact(&self, rev: u64) -> crate::Result<()> {
            self.inner.compact(rev)
        }
        fn compacted(&self) -> crate::Result<u64> {
            self.inner.compacted()
        }
        fn gc_range(
            &self,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<(String, Version)>> {
            self.key_space_reads.fetch_add(1, Ordering::SeqCst);
            self.inner.gc_range(after, limit, at)
        }
        fn range_outside(
            &self,
            skip: &[&str],
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<(String, Version)>> {
            self.key_space_reads.fetch_add(1, Ordering::SeqCst);
            self.inner.range_outside(skip, after, limit, at)
        }
    }

    #[test]
    fn a_pass_with_nothing_queued_reads_no_key_page() {
        let clock = VirtualClock::new(0);
        let c = open(Flaky::new(), &clock);
        write(&c, "owners/o", "o");
        for i in 0..2_000 {
            let t = Txn::new().put(
                &format!("rec/{i}"),
                Object::new(json!(1)).with_owners(&["owners/o"]),
            );
            c.txn(&t, &Request::new(&format!("r{i}"), "t", "test.write")).unwrap();
        }
        let before = c.backend().key_space_reads.load(Ordering::SeqCst);
        let pass = Controller::new(&c).pass(c.now_ms());
        assert!(pass.failed.is_empty() && pass.orphans.is_empty(), "{pass:?}");
        assert_eq!(c.backend().key_space_reads.load(Ordering::SeqCst), before);
    }

    #[test]
    fn a_head_that_moves_under_an_owner_read_replans_the_write_and_the_drain() {
        let clock = VirtualClock::new(0);
        let c = open(Flaky::new(), &clock);
        write(&c, "owners/o", "o");
        let t = Txn::new().put("rec/a", Object::new(json!(1)).with_owners(&["owners/o"]));
        c.txn(&t, &Request::new("a", "t", "test.write")).unwrap();
        c.backend().stale_owned_by.store(2, Ordering::SeqCst);
        c.delete("owners/o", None, &Request::new("d", "t", "test.delete")).unwrap();
        assert_eq!(c.backend().stale_owned_by.load(Ordering::SeqCst), 0, "the write replanned past both");
        c.backend().stale_owned_by.store(1, Ordering::SeqCst);
        let pass = Controller::new(&c).pass(c.now_ms());
        assert_eq!(c.backend().stale_owned_by.load(Ordering::SeqCst), 0, "the drain replanned past it");
        assert_eq!((pass.orphans, pass.failed), (vec!["rec/a".to_string()], vec![]));
    }

    #[test]
    fn a_failed_orphan_drain_is_one_failure_and_expiry_and_trim_still_run() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(Flaky::new(), &clock);
        write(&c, "rec/old", "old");
        clock.advance(EVENT_WINDOW_MS + DAY_MS);
        write(&c, "new/0", "new0");
        c.backend().refuse_markers.store(true, Ordering::SeqCst);
        let pass = Controller::new(&c).pass(c.now_ms());
        assert_eq!(pass.failed.len(), 1, "{pass:?}");
        assert_eq!(pass.failed[0].step, "orphan");
        assert_eq!(pass.trimmed, 1, "trim ran after the failed step");
    }

    #[test]
    fn a_failed_trim_still_compacts_and_names_its_step() {
        let clock = VirtualClock::new(0);
        let c = open(Flaky::new(), &clock);
        for i in 0..(COMPACT_KEEP + COMPACT_STEP + 10) {
            write(&c, "rec/a", &format!("r{i}"));
        }
        c.backend().refuse_events.store(true, Ordering::SeqCst);
        let err = Controller::new(&c).tick(c.now_ms()).unwrap_err();
        assert!(
            matches!(&err, Error::Backend(m) if m.starts_with("trim: ")),
            "{err:?}"
        );
        assert!(c.backend().compacted().unwrap() > 0);
    }
}
