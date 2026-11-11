//! The GC controller: one `tick(now)` removes finalized keys, collects owner-reference orphans
//! and expires keys whose TTL has run out.

use crate::backend::Backend;
use crate::contract::{Contract, COMPACT_STEP, COMPACT_STEPS};
use std::cell::Cell;

use crate::{Error, Item, Result};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tick {
    pub finalized: Vec<String>,
    pub orphans: Vec<String>,
    pub expired: Vec<String>,
    /// Audit records cut for being older than [`EVENT_WINDOW_MS`].
    pub trimmed: usize,
    /// Steps that failed this pass; every other step still ran.
    pub failed: Vec<StepFailure>,
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

pub struct Controller<'a, B: Backend> {
    contract: &'a Contract<B>,
    scan_every_ms: u64,
    last_scan: Cell<Option<u64>>,
}

fn finalized(i: &Item) -> bool {
    i.value.metadata.deletion_timestamp.is_some() && i.value.metadata.finalizers.is_empty()
}

impl<'a, B: Backend> Controller<'a, B> {
    /// Scans on every pass.
    pub fn new(contract: &'a Contract<B>) -> Self {
        Self::with_scan_every(contract, 0)
    }

    /// The whole-key scan behind finalize and orphan collection runs at most once per `ms`.
    pub fn with_scan_every(contract: &'a Contract<B>, ms: u64) -> Self {
        Controller {
            contract,
            scan_every_ms: ms,
            last_scan: Cell::new(None),
        }
    }

    /// `now_ms` is on the contract's monotonic clock.
    pub fn tick(&self, now_ms: u64) -> Result<Tick> {
        let mut tick = self.pass(now_ms);
        if tick.failed.is_empty() {
            return Ok(tick);
        }
        Err(Error::Backend(tick.failed.remove(0).to_string()))
    }

    /// Runs every step; a failed step is recorded and the others go ahead.
    pub fn pass(&self, now_ms: u64) -> Tick {
        let c = self.contract;
        let mut tick = Tick::default();
        let fail = |tick: &mut Tick, step: &'static str, error: Error| {
            tick.failed.push(StepFailure { step, error })
        };

        let scan_due = self
            .last_scan
            .get()
            .is_none_or(|last| now_ms.saturating_sub(last) >= self.scan_every_ms);
        if scan_due {
            self.last_scan.set(Some(now_ms));
            match c.gc_scan() {
                Ok(scan) => {
                    let ready: Vec<String> = scan.finalized.iter().map(|i| i.key.clone()).collect();
                    match c.collect(&ready, "gc.finalize", |i, _| Ok(finalized(i))) {
                        Ok(done) => tick.finalized = done,
                        Err(e) => fail(&mut tick, "finalize", e),
                    }
                    let orphan = |i: &Item, alive: &dyn Fn(&str) -> Result<bool>| -> Result<bool> {
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
                    };
                    let found: Result<Vec<String>> = (|| {
                        let mut candidates = Vec::new();
                        for i in &scan.owned {
                            if orphan(i, &|o| {
                                Ok(scan.keys.contains(o) && !tick.finalized.iter().any(|f| f == o))
                            })? {
                                candidates.push(i.key.clone());
                            }
                        }
                        c.collect(&candidates, "gc.orphan", |i, head| {
                            orphan(i, &|o| c.exists_at(o, head))
                        })
                    })();
                    match found {
                        Ok(orphans) => tick.orphans = orphans,
                        Err(e) => fail(&mut tick, "orphan", e),
                    }
                }
                Err(e) => fail(&mut tick, "scan", e),
            }
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
        if let Err(e) = c.compact_behind(COMPACT_KEEP, COMPACT_STEP, COMPACT_STEPS) {
            fail(&mut tick, "compact", e);
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
    use crate::{events_key, Error, MemBackend, Object};

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

    /// Ends any `gc_range` read of more than `limit` rows, as the store ends a read held past
    /// `max_read_ms`, and counts the ends.
    struct Limited {
        inner: MemBackend,
        limit: usize,
        ended: AtomicUsize,
        widest: AtomicUsize,
        armed: std::sync::atomic::AtomicBool,
    }

    impl Backend for Limited {
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
            self.inner.commit(expect, batch)
        }
        fn log(&self, from: u64, max_mains: usize) -> crate::Result<Vec<crate::LogEntry>> {
            self.inner.log(from, max_mains)
        }
        fn compact(&self, rev: u64) -> crate::Result<()> {
            self.inner.compact(rev)
        }
        fn gc_range(
            &self,
            after: Option<&str>,
            limit: usize,
            at: u64,
        ) -> crate::Result<Vec<(String, Version)>> {
            if self.armed.load(Ordering::SeqCst) && limit > self.limit {
                self.ended.fetch_add(1, Ordering::SeqCst);
                return Err(Error::Backend(format!("{} (1000 ms)", crate::READ_ENDED)));
            }
            if self.armed.load(Ordering::SeqCst) {
                self.widest.fetch_max(limit, Ordering::SeqCst);
            }
            self.inner.gc_range(after, limit, at)
        }
    }

    #[test]
    fn a_pass_over_a_store_whose_page_outlives_the_read_limit_narrows_its_pages_until_each_fits() {
        let clock = VirtualClock::new(0);
        let b = Limited {
            inner: MemBackend::new(),
            limit: 100,
            ended: AtomicUsize::new(0),
            widest: AtomicUsize::new(0),
            armed: Default::default(),
        };
        let c = open(b, &clock);
        for i in 0..1_200 {
            write(&c, &format!("rec/{i}"), &format!("r{i}"));
        }
        c.backend().armed.store(true, Ordering::SeqCst);
        Controller::new(&c)
            .tick(c.now_ms())
            .expect("the pass finishes");
        let b = c.backend();
        assert!(
            b.ended.load(Ordering::SeqCst) > 0,
            "the limit was never reached, so nothing was shown"
        );
        assert!(b.widest.load(Ordering::SeqCst) <= 100);
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

    /// Refuses `gc_range`, or `range` over `events/`, with a plain error once armed, and counts
    /// `gc_range` calls.
    struct Flaky {
        inner: MemBackend,
        refuse_scan: AtomicBool,
        refuse_events: AtomicBool,
        scans: AtomicUsize,
    }

    impl Flaky {
        fn new() -> Self {
            Flaky {
                inner: MemBackend::new(),
                refuse_scan: Default::default(),
                refuse_events: Default::default(),
                scans: AtomicUsize::new(0),
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
            self.inner.range(prefix, after, limit, at)
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
            self.scans.fetch_add(1, Ordering::SeqCst);
            if self.refuse_scan.load(Ordering::SeqCst) {
                return Err(Error::Backend("refused".into()));
            }
            self.inner.gc_range(after, limit, at)
        }
    }

    #[test]
    fn a_failed_scan_still_trims_the_history_and_names_its_step() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(Flaky::new(), &clock);
        for i in 0..2_500 {
            write(&c, &format!("rec/{i}"), &format!("old{i}"));
        }
        clock.advance(EVENT_WINDOW_MS + DAY_MS);
        for i in 0..5 {
            write(&c, &format!("new/{i}"), &format!("new{i}"));
        }
        c.backend().refuse_scan.store(true, Ordering::SeqCst);
        let err = Controller::new(&c).tick(c.now_ms()).unwrap_err();
        assert!(err.to_string().contains("scan: "), "{err}");
        assert!(
            matches!(&err, Error::Backend(m) if m.starts_with("scan: ")),
            "{err:?}"
        );
        assert!(events(&c) < 2_505);
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

    #[test]
    fn a_scan_runs_once_per_cadence_while_trim_runs_every_pass() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(Flaky::new(), &clock);
        for i in 0..2_500 {
            write(&c, &format!("rec/{i}"), &format!("old{i}"));
        }
        clock.advance(EVENT_WINDOW_MS + DAY_MS);
        write(&c, "new/0", "new0");
        let gc = Controller::with_scan_every(&c, 300_000);
        let now = c.now_ms();
        let first = gc.pass(now);
        let scans = c.backend().scans.load(Ordering::SeqCst);
        assert!(scans > 0 && first.failed.is_empty());
        let second = gc.pass(now);
        assert_eq!(
            c.backend().scans.load(Ordering::SeqCst),
            scans,
            "no scan inside the cadence"
        );
        assert!(second.trimmed > 0);
        gc.pass(now + 300_000);
        assert!(
            c.backend().scans.load(Ordering::SeqCst) > scans,
            "the cadence elapsed"
        );
    }

    #[test]
    fn a_failed_scan_is_one_failure_and_is_retried_at_the_cadence_not_every_pass() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(Flaky::new(), &clock);
        write(&c, "rec/a", "a");
        c.backend().refuse_scan.store(true, Ordering::SeqCst);
        let gc = Controller::with_scan_every(&c, 300_000);
        let now = c.now_ms();
        let first = gc.pass(now);
        assert_eq!(first.failed.len(), 1);
        assert_eq!(first.failed[0].step, "scan");
        let scans = c.backend().scans.load(Ordering::SeqCst);
        gc.pass(now);
        assert_eq!(
            c.backend().scans.load(Ordering::SeqCst),
            scans,
            "a failed scan waits out the cadence"
        );
        gc.pass(now + 300_000);
        assert!(c.backend().scans.load(Ordering::SeqCst) > scans);
    }
}
