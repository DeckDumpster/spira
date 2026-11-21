//! The GC controller: one `tick(now)` drains the markers of owners that went, and expires keys
//! whose TTL has run out. A finished key is removed by the write that finished it, so no pass
//! reads the key space. Locks and receipts expire on the monotonic clock; a key with a stored
//! lifetime expires when its `expiresAt` passes on the wall clock.

use crate::backend::Backend;
use crate::contract::{Contract, EXPIRE_PER_PASS, PAGE};
use crate::{Error, Item, Result, ORPHANS_PREFIX};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tick {
    pub orphans: Vec<String>,
    pub expired: Vec<String>,
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

    /// Deletes at most [`EXPIRE_PER_PASS`] keys whose stored expiry is at or before wall now. A
    /// head that moves under the index read is read again.
    fn expire_stored(&self) -> Result<Vec<String>> {
        let c = self.contract;
        let now = c.wall_ms();
        let mut tries = 0;
        let due = loop {
            let head = c.head()?;
            match c.backend().due(now, EXPIRE_PER_PASS, head) {
                Err(Error::Stale { .. }) if tries < 8 => tries += 1,
                other => break other?,
            }
        };
        c.expire_stored(&due, now)
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
        match self.expire_stored() {
            Ok(mut expired) => tick.expired.append(&mut expired),
            Err(e) => fail(&mut tick, "due", e),
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
    use crate::{Error, MemBackend, Object, ORPHANS_PREFIX};

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

    #[test_id(tgrn0sd)]
    #[test]
    fn a_gc_pass_leaves_every_audit_event_in_place_however_old() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(MemBackend::new(), &clock);
        for i in 0..5 {
            write(&c, &format!("rec/{i}"), &format!("old{i}"));
        }
        clock.advance(15 * DAY_MS);
        write(&c, "rec/new", "new");
        let gc = Controller::new(&c);
        gc.tick(c.now_ms()).unwrap();
        assert_eq!(events(&c), 6);
    }

    /// Refuses `range` over `events/` with a plain error once armed, and counts the reads that
    /// visit the key space: `gc_range`, `range_outside`, and a `range` of any prefix but
    /// `events/` or `orphans/`.
    struct Flaky {
        inner: MemBackend,
        refuse_markers: AtomicBool,
        stale_owned_by: AtomicUsize,
        key_space_reads: AtomicUsize,
        whole_space_calls: AtomicUsize,
        range_prefixes: std::sync::Mutex<Vec<String>>,
        refuse_due: AtomicBool,
    }

    impl Flaky {
        fn new() -> Self {
            Flaky {
                inner: MemBackend::new(),
                refuse_markers: Default::default(),
                stale_owned_by: AtomicUsize::new(0),
                key_space_reads: AtomicUsize::new(0),
                whole_space_calls: AtomicUsize::new(0),
                range_prefixes: Default::default(),
                refuse_due: Default::default(),
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
            if prefix == ORPHANS_PREFIX && self.refuse_markers.load(Ordering::SeqCst) {
                return Err(Error::Backend("refused".into()));
            }
            self.range_prefixes.lock().unwrap().push(prefix.to_string());
            if prefix != crate::EVENTS_PREFIX && prefix != ORPHANS_PREFIX {
                self.key_space_reads.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.range(prefix, after, limit, at)
        }
        fn due(&self, now: u64, limit: usize, at: u64) -> crate::Result<Vec<String>> {
            if self.refuse_due.load(Ordering::SeqCst) {
                return Err(Error::Backend("refused".into()));
            }
            self.inner.due(now, limit, at)
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
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
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
            self.whole_space_calls.fetch_add(1, Ordering::SeqCst);
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
            self.whole_space_calls.fetch_add(1, Ordering::SeqCst);
            self.inner.range_outside(skip, after, limit, at)
        }
    }

    #[test_id(tcpz9dp)]
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

    #[test_id(trzr19a)]
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

    #[test_id(tw6xnhy)]
    #[test]
    fn a_failed_orphan_drain_is_one_failure_and_expiry_still_runs() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(Flaky::new(), &clock);
        write_ttl(&c, "telemetry/a", 1);
        clock.advance(2_000);
        c.backend().refuse_markers.store(true, Ordering::SeqCst);
        let pass = Controller::new(&c).pass(c.now_ms());
        assert_eq!(pass.failed.len(), 1, "{pass:?}");
        assert_eq!(pass.failed[0].step, "orphan");
        assert_eq!(pass.expired, ["telemetry/a"], "expiry ran after the failed step");
    }

    fn write_ttl(c: &Contract<impl Backend>, key: &str, ttl_secs: u64) {
        let t = Txn::new().put(key, Object::new(json!(1)).with_ttl(ttl_secs));
        c.txn(&t, &Request::new(&format!("w{key}"), "t", "test.write")).unwrap();
    }

    #[test_id(tg759tt)]
    #[test]
    fn one_pass_expires_at_most_the_per_pass_cap_of_due_keys() {
        let clock = VirtualClock::new(0);
        let c = open(MemBackend::new(), &clock);
        for i in 0..3_000 {
            write_ttl(&c, &format!("telemetry/{i}"), 1);
        }
        clock.advance(2_000);
        let gc = Controller::new(&c);
        let passes: Vec<usize> = (0..4).map(|_| gc.tick(c.now_ms()).unwrap().expired.len()).collect();
        assert_eq!(passes, [1_024, 1_024, 952, 0]);
    }

    #[test_id(tg9t8if)]
    #[test]
    fn open_reads_only_the_locks_and_the_receipts() {
        let clock = VirtualClock::new(0);
        let b = Flaky::new();
        let c = open(b, &clock);
        for i in 0..2_000 {
            write_ttl(&c, &format!("telemetry/{i}"), 3_600);
        }
        for i in 0..10 {
            c.lock(&format!("l{i}"), 60_000, &Request::new(&format!("l{i}"), "t", "test.lock")).unwrap();
        }
        let b = c.backend().inner.clone();
        drop(c);
        let counted = Flaky { inner: b, ..Flaky::new() };
        let c = open(counted, &clock);
        let seen = c.backend().range_prefixes.lock().unwrap().clone();
        assert_eq!(c.backend().whole_space_calls.load(Ordering::SeqCst), 0);
        assert!(!seen.is_empty() && seen.iter().all(|p| p == "locks/" || p == "requests/"), "{seen:?}");
    }

    #[test_id(trcmuah)]
    #[test]
    fn a_failed_due_read_is_one_failure_and_the_other_steps_still_run() {
        let clock = VirtualClock::new(10 * DAY_MS);
        let c = open(Flaky::new(), &clock);
        write(&c, "rec/old", "old");
        c.backend().refuse_due.store(true, Ordering::SeqCst);
        let pass = Controller::new(&c).pass(c.now_ms());
        assert_eq!(pass.failed.len(), 1, "{pass:?}");
        assert_eq!(pass.failed[0].step, "due");
    }

    #[test_id(tapo9bk)]
    #[test]
    fn a_key_rewritten_after_it_was_listed_due_is_not_expired() {
        let clock = VirtualClock::new(0);
        let c = open(MemBackend::new(), &clock);
        write_ttl(&c, "telemetry/a", 1);
        clock.advance(2_000);
        let listed = c.backend().due(c.wall_ms(), 10, c.head().unwrap()).unwrap();
        assert_eq!(listed, ["telemetry/a"]);
        let t = Txn::new().put("telemetry/a", Object::new(json!(2)).with_ttl(3_600));
        c.txn(&t, &Request::new("again", "t", "test.write")).unwrap();
        assert!(c.expire_stored(&listed, c.wall_ms()).unwrap().is_empty());
        assert!(c.get("telemetry/a").unwrap().is_some());
    }

    #[test_id(tv9s7i3)]
    #[test]
    fn a_key_marked_for_deletion_carries_no_expiry() {
        let clock = VirtualClock::new(0);
        let c = open(MemBackend::new(), &clock);
        let held = Object::new(json!(1)).with_ttl(1).with_finalizers(&["f"]);
        c.txn(&Txn::new().put("telemetry/a", held), &Request::new("w", "t", "test.write")).unwrap();
        c.delete("telemetry/a", None, &Request::new("d", "t", "test.delete")).unwrap();
        clock.advance(2_000);
        let head = c.head().unwrap();
        assert!(c.backend().due(c.wall_ms(), 10, head).unwrap().is_empty());
        assert!(Controller::new(&c).tick(c.now_ms()).unwrap().expired.is_empty());
    }

}
