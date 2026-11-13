//! The conformance suite: the contract's specification, run identically on every backend. A
//! `Rig` supplies a fresh backend and reopens the same one, which is how a restart is staged.
//! Every scenario runs on a `VirtualClock`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::ports::VirtualClock;
use serde_json::json;

use crate::backend::{Backend, LogEntry, Version, Write};
use crate::contract::{
    Compare, Config, Contract, Created, Deleted, Fence, Ports, Request, Selector, Txn, Watched,
};
use crate::gc::Controller;
use crate::limits::{self, MAX_REQUEST_BYTES, MAX_TXN_COMPARES, MAX_TXN_OPS};
use crate::{events_key, request_key, Error, Limit, Object, Result, Rev, HISTORY_PREFIX};

pub trait Rig {
    type B: Backend + 'static;
    /// An empty store.
    fn fresh(&self) -> Self::B;
    /// The same store, opened again.
    fn reopen(&self, b: &Self::B) -> Self::B;
}

pub const MIN_LOCK_TTL_MS: u64 = 1_000;
const T0: u64 = 1_000_000;
const HOUR_MS: u64 = 3_600_000;

pub fn open<B: Backend>(b: B, clock: &VirtualClock) -> Contract<B> {
    Contract::open(
        b,
        Ports::virtual_clock(clock),
        Config {
            min_lock_ttl_ms: MIN_LOCK_TTL_MS,
        },
    )
    .expect("open")
}

fn req(id: &str) -> Request {
    Request::new(id, "conformance", "test.write")
}

fn obj(v: serde_json::Value) -> Object {
    Object::new(v)
}

fn data<B: Backend>(c: &Contract<B>, key: &str) -> Option<serde_json::Value> {
    c.get(key).expect("get").map(|i| i.value.data)
}

fn put<B: Backend>(c: &Contract<B>, key: &str, v: Object, id: &str) -> u64 {
    let r = c.txn(&Txn::new().put(key, v), &req(id)).expect("txn");
    assert!(r.succeeded, "{key}: {r:?}");
    r.rev
}

/// Delegates to a backend, and can run a hook after a range read or lose a commit's reply.
pub struct Interpose<B: Backend> {
    inner: B,
    after_range: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    ambiguous_commit: AtomicBool,
    gc_rows: AtomicUsize,
}

impl<B: Backend> Interpose<B> {
    pub fn new(inner: B) -> Self {
        Interpose {
            inner,
            after_range: Mutex::new(None),
            ambiguous_commit: AtomicBool::new(false),
            gc_rows: AtomicUsize::new(0),
        }
    }

    pub fn after_next_range(&self, f: impl FnOnce() + Send + 'static) {
        *self.after_range.lock().unwrap() = Some(Box::new(f));
    }

    /// The rows `gc_range` has returned so far.
    pub fn gc_rows(&self) -> usize {
        self.gc_rows.load(Ordering::SeqCst)
    }

    /// The next commit is applied, and its caller is told it failed.
    pub fn lose_next_commit_reply(&self) {
        self.ambiguous_commit.store(true, Ordering::SeqCst);
    }
}

impl<B: Backend> Backend for Interpose<B> {
    fn head(&self) -> Result<u64> {
        self.inner.head()
    }
    fn get(&self, key: &str, at: u64) -> Result<Option<Version>> {
        self.inner.get(key, at)
    }
    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let out = self.inner.range(prefix, after, limit, at);
        let hook = self.after_range.lock().unwrap().take();
        if let Some(f) = hook {
            f();
        }
        out
    }
    fn range_in_state(
        &self,
        prefix: &str,
        state: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        self.inner.range_in_state(prefix, state, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[Write]) -> Result<u64> {
        let main = self.inner.commit(expect, batch)?;
        if self.ambiguous_commit.swap(false, Ordering::SeqCst) {
            return Err(Error::Backend(format!(
                "connection lost after commit of main {main}"
            )));
        }
        Ok(main)
    }
    fn log(&self, from: u64, max_mains: usize) -> Result<Vec<LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> Result<()> {
        self.inner.compact(rev)
    }
    fn gc_range(
        &self,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let rows = self.inner.gc_range(after, limit, at)?;
        self.gc_rows.fetch_add(rows.len(), Ordering::SeqCst);
        Ok(rows)
    }
}

/// Every key the txn at `main` wrote.
fn written_at<B: Backend>(c: &Contract<B>, main: u64) -> Vec<String> {
    let (items, _) = c.list(&[""], &Selector::Everything).expect("list");
    items
        .into_iter()
        .filter(|i| i.mod_rev.main == main)
        .map(|i| i.key)
        .collect()
}

fn sub_event_keys(ev: &serde_json::Value) -> Vec<String> {
    ev["subs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| s["key"].as_str().unwrap_or_default().to_string())
        .collect()
}

pub fn cas_conflict_on_a_stale_mod_rev<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    put(&c, "a", obj(json!(1)), "1");
    let stale = c.get("a").unwrap().unwrap().mod_rev;
    put(&c, "a", obj(json!(2)), "2");
    let r = c
        .txn(
            &Txn::new()
                .when(Compare::ModRev("a".into(), Some(stale)))
                .put("a", obj(json!(3))),
            &req("3"),
        )
        .unwrap();
    assert!(!r.succeeded);
    assert_eq!(data(&c, "a"), Some(json!(2)));
    let fresh = c.get("a").unwrap().unwrap().mod_rev;
    assert_eq!(
        c.delete("a", Some(stale), &req("4")),
        Err(Error::RevMismatch {
            key: "a".into(),
            expected: stale,
            actual: fresh
        })
    );
    assert_eq!(data(&c, "a"), Some(json!(2)));
}

pub fn a_refused_txn_writes_nothing_but_its_events_key_carrying_the_refusal<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    put(&c, "b", obj(json!(1)), "1");
    let before = c.head().unwrap();
    let t = Txn::new()
        .when(Compare::ModRev("b".into(), None))
        .put("x", obj(json!(1)))
        .put("y", obj(json!(1)));
    let r = c.txn(&t, &req("2")).unwrap();
    assert!(!r.succeeded);
    assert_eq!(r.rev, before + 1);
    assert_eq!(data(&c, "x"), None);
    assert_eq!(data(&c, "y"), None);
    assert_eq!(
        written_at(&c, r.rev),
        vec![events_key(r.rev), request_key("2")]
    );
    let ev = data(&c, &events_key(r.rev)).expect("events key");
    assert_eq!(ev["refused"], json!(true));
    assert_eq!(ev["actor"], json!("conformance"));
    assert_eq!(ev["kind"], json!("test.write"));
    assert!(sub_event_keys(&ev).is_empty());
}

pub fn one_main_per_txn_with_main_sub_per_key<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let r = c
        .txn(
            &Txn::new()
                .put("k/a", obj(json!(1)))
                .put("k/b", obj(json!(2)))
                .put("k/c", obj(json!(3))),
            &req("1"),
        )
        .unwrap();
    for (sub, key) in ["k/a", "k/b", "k/c"].iter().enumerate() {
        let i = c.get(key).unwrap().unwrap();
        assert_eq!(i.mod_rev, Rev::new(r.rev, sub as u32), "{key}");
        assert_eq!(i.create_rev, Rev::new(r.rev, sub as u32), "{key}");
    }
    let ev = data(&c, &events_key(r.rev)).unwrap();
    assert_eq!(sub_event_keys(&ev), ["k/a", "k/b", "k/c"]);
    assert_eq!(ev["subs"][2]["sub"], json!(2));
    let r2 = c
        .txn(&Txn::new().put("k/a", obj(json!(9))), &req("2"))
        .unwrap();
    assert_eq!(r2.rev, r.rev + 1);
    let a = c.get("k/a").unwrap().unwrap();
    assert_eq!(a.mod_rev, Rev::new(r2.rev, 0));
    assert_eq!(a.create_rev, Rev::new(r.rev, 0));
    assert_eq!(
        data(&c, &events_key(r2.rev)).unwrap()["subs"][0]["prev_mod_rev"],
        json!({"main": r.rev, "sub": 0})
    );
}

pub fn the_next_main_stays_greater_after_deleting_the_newest_keys_and_reopening<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let b = rig.fresh();
    let c = open(rig.reopen(&b), &clock);
    put(&c, "n/a", obj(json!(1)), "1");
    put(&c, "n/b", obj(json!(2)), "2");
    c.delete("n/b", None, &req("3")).unwrap();
    let Deleted::Removed { rev: last } = c.delete("n/a", None, &req("4")).unwrap() else {
        panic!("not removed")
    };
    assert!(last > 0);
    put(&c, "telemetry/n", obj(json!(1)).with_ttl(1), "5");
    clock.advance(1_000);
    Controller::new(&c).tick(c.now_ms()).unwrap();
    let last = c.head().unwrap();
    let (live, _) = c.list(&[""], &Selector::Everything).unwrap();
    assert!(
        live.iter().all(|i| i.mod_rev.main < last),
        "the newest txn left no key behind"
    );
    drop(c);
    let c = open(rig.reopen(&b), &clock);
    assert_eq!(c.head().unwrap(), last);
    let next = put(&c, "n/c", obj(json!(3)), "6");
    assert!(next > last, "{next} <= {last}");
}

pub fn create_with_an_equal_value_succeeds_and_a_different_value_conflicts<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let first = c
        .create("c/x", obj(json!({"s": "OPEN"})), &req("1"))
        .unwrap();
    assert!(!first.existed);
    let head = c.head().unwrap();
    assert_eq!(
        c.create("c/x", obj(json!({"s": "OPEN"})), &req("2"))
            .unwrap(),
        Created {
            mod_rev: first.mod_rev,
            existed: true
        }
    );
    assert_eq!(c.head().unwrap(), head, "an equal create writes nothing");
    match c.create("c/x", obj(json!({"s": "READY"})), &req("3")) {
        Err(Error::Conflict { key, existing }) => {
            assert_eq!(key, "c/x");
            assert_eq!(existing.value.data, json!({"s": "OPEN"}));
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

pub fn list_pages_and_list_rev_come_from_one_snapshot_while_a_commit_lands_between_pages<R: Rig>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    for (i, k) in ["p/a", "p/b", "p/c"].iter().enumerate() {
        put(&c, k, obj(json!(i)), &format!("seed{i}"));
    }
    let snap = c.head().unwrap();
    let p1 = c
        .list_page(&["p/"], &Selector::Everything, 2, None)
        .unwrap();
    assert_eq!(p1.list_rev, snap);
    assert_eq!(
        p1.items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
        ["p/a", "p/b"]
    );
    let next = p1.next.clone().expect("a second page");
    c.txn(
        &Txn::new()
            .put("p/c", obj(json!("new")))
            .put("p/d", obj(json!(4))),
        &req("between"),
    )
    .unwrap();
    let p2 = c
        .list_page(&["p/"], &Selector::Everything, 2, Some(&next))
        .unwrap();
    assert_eq!(p2.list_rev, snap);
    assert_eq!(
        p2.items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
        ["p/c"]
    );
    assert_eq!(p2.items[0].value.data, json!(2));
    assert!(p2.next.is_none());
    let (all, rev) = c.list(&["p/"], &Selector::Eq("".into(), json!(4))).unwrap();
    assert_eq!(rev, c.head().unwrap());
    assert_eq!(
        all.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
        ["p/d"]
    );
}

pub fn watch_resumes_from_a_revision<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let r1 = put(&c, "w/a", obj(json!(1)), "1");
    let r2 = put(&c, "w/b", obj(json!(2)), "2");
    let r3 = put(&c, "x/other", obj(json!(3)), "3");
    assert_eq!(
        c.watch("w/", r3 + 1, 0),
        Err(Error::FutureRev {
            rev: r3 + 1,
            head: r3
        })
    );
    let Watched::Events { events, through } = c.watch("w/", r1, 0).unwrap() else {
        panic!("no events")
    };
    assert_eq!(
        events.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        ["w/b"]
    );
    assert_eq!(events[0].rev, Rev::new(r2, 0));
    assert_eq!(events[0].value.as_ref().unwrap().data, json!(2));
    assert_eq!(through, r3, "a watch resumes past the txns it filtered out");
    let Watched::Events { events, .. } = c.watch("w/", 0, 0).unwrap() else {
        panic!("no events")
    };
    assert_eq!(events.len(), 2);
    let r4 = c.delete("w/a", None, &req("4")).unwrap();
    let Deleted::Removed { rev: r4 } = r4 else {
        panic!()
    };
    let Watched::Events { events, .. } = c.watch("w/", through, 0).unwrap() else {
        panic!("no events")
    };
    assert_eq!(events.len(), 1);
    assert_eq!(
        (
            events[0].key.as_str(),
            events[0].rev.main,
            events[0].value.is_none()
        ),
        ("w/a", r4, true)
    );
    assert_eq!(events[0].prev_mod_rev, Some(Rev::new(r1, 0)));
}

/// Runs `f` on another thread while advancing the virtual clock until it returns.
fn while_time_passes<B: Backend, T: Send>(
    c: &Contract<B>,
    clock: &VirtualClock,
    f: impl FnOnce() -> T + Send,
) -> T {
    std::thread::scope(|s| {
        let h = s.spawn(f);
        while !h.is_finished() {
            clock.advance(100);
            c.wake_watchers();
            std::thread::yield_now();
        }
        h.join().expect("watcher")
    })
}

pub fn a_watch_on_a_quiet_prefix_returns_progress_rev<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let r = put(&c, "q/a", obj(json!(1)), "1");
    put(&c, "elsewhere", obj(json!(1)), "2");
    let got = while_time_passes(&c, &clock, || c.watch("q/", r, 2_500));
    assert_eq!(
        got.unwrap(),
        Watched::Progress {
            progress_rev: c.head().unwrap()
        }
    );
    assert_eq!(
        c.watch("q/", r, 2_501),
        Err(Error::WaitTooLong {
            max_ms: 2_500,
            got_ms: 2_501
        })
    );
}

pub fn watch_returns_too_old_after_compaction<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let r1 = put(&c, "t/a", obj(json!(1)), "1");
    let r2 = put(&c, "t/b", obj(json!(2)), "2");
    put(&c, "t/c", obj(json!(3)), "3");
    c.compact(r2).unwrap();
    assert_eq!(c.watch("t/", r1, 0), Err(Error::TooOld { compacted: r2 }));
    assert_eq!(c.get_at("t/a", r1), Err(Error::TooOld { compacted: r2 }));
    let Watched::Events { events, .. } = c.watch("t/", r2, 0).unwrap() else {
        panic!("no events")
    };
    assert_eq!(
        events.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        ["t/c"]
    );
    assert_eq!(
        data(&c, "t/a"),
        Some(json!(1)),
        "compaction keeps the latest version"
    );
}

pub fn a_lock_expires_on_the_virtual_clock<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let a = c.lock("job", 5_000, &req("1")).unwrap();
    assert_eq!(
        c.lock("job", 5_000, &req("2")),
        Err(Error::Locked {
            resource: "job".into(),
            token: a.token
        })
    );
    clock.advance(4_999);
    assert!(matches!(
        c.lock("job", 5_000, &req("3")),
        Err(Error::Locked { .. })
    ));
    c.renew(&a, &req("4")).unwrap();
    clock.advance(4_999);
    assert!(
        matches!(c.lock("job", 5_000, &req("5")), Err(Error::Locked { .. })),
        "renew restarts the TTL"
    );
    clock.advance(1);
    let b = c.lock("job", 5_000, &req("6")).unwrap();
    assert!(b.token > a.token);
    assert_eq!(
        c.renew(&a, &req("7")),
        Err(Error::LockLost {
            resource: "job".into()
        })
    );
    assert_eq!(
        c.release(&a, &req("8")),
        Err(Error::LockLost {
            resource: "job".into()
        })
    );
    c.release(&b, &req("9")).unwrap();
    assert!(c.get("locks/job").unwrap().is_none());
}

pub fn a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let a = c.lock("round/1", 5_000, &req("1")).unwrap();
    assert!(
        c.txn(
            &Txn::new().fenced(&a).put("rounds/1", obj(json!("a"))),
            &req("2")
        )
        .unwrap()
        .succeeded
    );
    clock.advance(5_000);
    let b = c.lock("round/1", 5_000, &req("3")).unwrap();
    assert!(
        c.txn(
            &Txn::new().fenced(&b).put("rounds/1", obj(json!("b"))),
            &req("4")
        )
        .unwrap()
        .succeeded
    );
    let late = c
        .txn(
            &Txn::new().fenced(&a).put("rounds/1", obj(json!("a-late"))),
            &req("5"),
        )
        .unwrap();
    assert!(!late.succeeded);
    assert_eq!(data(&c, "rounds/1"), Some(json!("b")));
    assert!(data(&c, "locks/round/1").is_some());
}

pub fn reopen_extends_every_lock<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let b = rig.fresh();
    let c = open(rig.reopen(&b), &clock);
    c.lock("one", 5_000, &req("1")).unwrap();
    c.lock("two", 8_000, &req("2")).unwrap();
    clock.advance(4_000);
    drop(c);
    let c = open(rig.reopen(&b), &clock);
    clock.advance(4_999);
    assert!(matches!(
        c.lock("one", 5_000, &req("3")),
        Err(Error::Locked { .. })
    ));
    assert!(matches!(
        c.lock("two", 5_000, &req("4")),
        Err(Error::Locked { .. })
    ));
    clock.advance(1);
    c.lock("one", 5_000, &req("5")).unwrap();
    clock.advance(2_999);
    assert!(matches!(
        c.lock("two", 5_000, &req("6")),
        Err(Error::Locked { .. })
    ));
    clock.advance(1);
    c.lock("two", 5_000, &req("7")).unwrap();
}

pub fn a_lock_ttl_below_the_minimum_is_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    assert_eq!(
        c.lock("x", MIN_LOCK_TTL_MS - 1, &req("1")),
        Err(Error::LockTtlBelowMinimum {
            min_ms: MIN_LOCK_TTL_MS,
            got_ms: MIN_LOCK_TTL_MS - 1
        })
    );
    assert!(c.get("locks/x").unwrap().is_none());
    let head = c.head().unwrap();
    assert_eq!(
        c.txn(
            &Txn::new()
                .lock_acquire("x", MIN_LOCK_TTL_MS - 1)
                .put("jobs/x", obj(json!(1))),
            &req("txn")
        ),
        Err(Error::LockTtlBelowMinimum {
            min_ms: MIN_LOCK_TTL_MS,
            got_ms: MIN_LOCK_TTL_MS - 1
        })
    );
    assert_eq!(
        c.head().unwrap(),
        head,
        "a refused lock_acquire writes nothing"
    );
    c.lock("x", MIN_LOCK_TTL_MS, &req("2")).unwrap();
}

pub fn a_finalizer_blocks_delete_until_gc_removes_the_key_once_cleared<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    c.create(
        "rec/1",
        obj(json!({"s": "DONE"})).with_finalizers(&["planning-closed"]),
        &req("1"),
    )
    .unwrap();
    assert!(matches!(
        c.delete("rec/1", None, &req("2")).unwrap(),
        Deleted::Pending { .. }
    ));
    let held = c.get("rec/1").unwrap().expect("still present");
    assert_eq!(held.value.metadata.deletion_timestamp, Some(T0));
    assert!(gc.tick(c.now_ms()).unwrap().finalized.is_empty());
    assert!(c.get("rec/1").unwrap().is_some());
    put(&c, "rec/1", obj(json!({"s": "DONE"})), "3");
    assert_eq!(
        c.get("rec/1")
            .unwrap()
            .unwrap()
            .value
            .metadata
            .deletion_timestamp,
        Some(T0),
        "a write keeps the mark"
    );
    assert_eq!(gc.tick(c.now_ms()).unwrap().finalized, ["rec/1"]);
    assert!(c.get("rec/1").unwrap().is_none());
}

pub fn an_owner_reference_orphan_is_collected<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    put(&c, "batches/r/1", obj(json!(1)), "1");
    put(
        &c,
        "members/r/1/a",
        obj(json!(1)).with_owners(&["batches/r/1"]),
        "2",
    );
    put(
        &c,
        "members/r/9/a",
        obj(json!(1)).with_owners(&["batches/r/9"]),
        "3",
    );
    assert_eq!(gc.tick(c.now_ms()).unwrap().orphans, ["members/r/9/a"]);
    assert!(c.get("members/r/1/a").unwrap().is_some());
    c.delete("batches/r/1", None, &req("4")).unwrap();
    assert_eq!(gc.tick(c.now_ms()).unwrap().orphans, ["members/r/1/a"]);
    assert!(c.get("members/r/1/a").unwrap().is_none());
}

pub fn a_request_retry_after_an_ambiguous_commit_reports_already_applied<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(Interpose::new(rig.fresh()), &clock);
    c.backend().lose_next_commit_reply();
    let t = Txn::new().put("r/a", obj(json!(1)));
    assert!(c.txn(&t, &req("once")).is_err());
    let applied = c
        .get("r/a")
        .unwrap()
        .expect("the commit landed")
        .mod_rev
        .main;
    let retry = c.txn(&t, &req("once")).unwrap();
    assert_eq!(
        (retry.rev, retry.succeeded, retry.already_applied),
        (applied, true, true)
    );
    assert_eq!(c.head().unwrap(), applied, "the retry wrote nothing");
}

pub fn retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(Interpose::new(rig.fresh()), &clock);
    let lost = |f: &dyn Fn() -> bool| {
        c.backend().lose_next_commit_reply();
        assert!(f(), "the commit's reply was lost");
    };

    lost(&|| c.lock("job", 5_000, &req("a-lock")).is_err());
    let a = c.get("locks/job").unwrap().unwrap().create_rev;
    clock.advance(5_000);
    let b = c.lock("job", 5_000, &req("b-lock")).unwrap();
    let retried = c.lock("job", 5_000, &req("a-lock")).unwrap();
    assert_eq!(
        retried.token, a,
        "a retry returns its own token, never the new holder's"
    );
    assert_ne!(retried.token, b.token);
    let late = c.txn(
        &Txn::new().fenced(&retried).put("jobs/1", obj(json!("a"))),
        &req("a-write"),
    );
    assert!(!late.unwrap().succeeded);

    lost(&|| c.create("c/1", obj(json!(1)), &req("create")).is_err());
    let created = c.get("c/1").unwrap().unwrap().mod_rev;
    let again = c.create("c/1", obj(json!(1)), &req("create")).unwrap();
    assert_eq!(again.mod_rev, created);

    lost(&|| c.delete("c/1", None, &req("delete")).is_err());
    let Deleted::Removed { rev } = c.delete("c/1", None, &req("delete")).unwrap() else {
        panic!("not removed")
    };
    assert_eq!(c.head().unwrap(), rev, "the retry wrote nothing");

    let refused = Txn::new()
        .when(Compare::ModRev("c/1".into(), Some(Rev::new(1, 0))))
        .put("c/2", obj(json!(2)));
    lost(&|| c.txn(&refused, &req("refused")).is_err());
    let r = c.txn(&refused, &req("refused")).unwrap();
    assert_eq!((r.succeeded, r.already_applied), (false, true));
}

pub fn request_keys_expire_at_24h_of_virtual_time<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    put(&c, "a", obj(json!(1)), "day");
    clock.advance(24 * HOUR_MS - 1);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get(&request_key("day")).unwrap().is_some());
    clock.advance(1);
    assert_eq!(gc.tick(c.now_ms()).unwrap().expired, [request_key("day")]);
    assert!(c.get(&request_key("day")).unwrap().is_none());
}

pub fn a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing<R: Rig>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let m = put(
        &c,
        "telemetry/reconciler/pass",
        obj(json!(1)).with_ttl(86_400),
        "1",
    );
    assert_eq!(
        c.get("telemetry/reconciler/pass").unwrap().unwrap().mod_rev,
        Rev::new(m, 0)
    );
    assert_eq!(
        written_at(&c, m),
        ["requests/1", "telemetry/reconciler/pass"]
    );
    let head = c.head().unwrap();
    let t = Txn::new()
        .when(Compare::ModRev("telemetry/reconciler/pass".into(), None))
        .put("telemetry/reconciler/pass", obj(json!(2)).with_ttl(60));
    let r = c.txn(&t, &req("2")).unwrap();
    assert!(!r.succeeded);
    assert_eq!(
        c.head().unwrap(),
        head,
        "a refused telemetry txn stores nothing"
    );
    assert!(c.get(&request_key("2")).unwrap().is_none());
    let Watched::Events { events, .. } = c.watch("telemetry/", 0, 0).unwrap() else {
        panic!("telemetry is watched")
    };
    assert_eq!(events.len(), 1);
}

pub fn a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events<R: Rig>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let t = Txn::new()
        .put(
            "telemetry/sections/Round/a",
            obj(json!(1)).with_ttl(604_800),
        )
        .put("intents/Round/a/1", obj(json!(1)).with_ttl(172_800));
    let m = c.txn(&t, &req("1")).unwrap().rev;
    let ev = data(&c, &events_key(m)).expect("events key");
    assert_eq!(
        sub_event_keys(&ev),
        ["telemetry/sections/Round/a", "intents/Round/a/1"]
    );
    assert_eq!(written_at(&c, m).len(), 4);
}

pub fn a_put_under_intents_alone_writes_one_events_key<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let m = put(
        &c,
        "intents/Round/a/1",
        obj(json!(1)).with_ttl(172_800),
        "1",
    );
    assert_eq!(
        sub_event_keys(&data(&c, &events_key(m)).unwrap()),
        ["intents/Round/a/1"]
    );
    let (events, _) = c.list(&["events/"], &Selector::Everything).unwrap();
    assert_eq!(events.len(), 1);
}

pub fn a_put_under_telemetry_or_intents_without_ttl_secs_is_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let head = c.head().unwrap();
    for key in ["telemetry/sections/A/b", "intents/A/b/1"] {
        let want = Err(Error::TtlRequired { key: key.into() });
        assert_eq!(c.txn(&Txn::new().put(key, obj(json!(1))), &req(key)), want);
        assert_eq!(
            c.txn(&Txn::new().put(key, obj(json!(1)).with_ttl(0)), &req(key)),
            want
        );
        assert_eq!(
            c.create(key, obj(json!(1)), &req(key)).map(|_| ()),
            Err(Error::TtlRequired { key: key.into() })
        );
    }
    assert_eq!(
        c.txn(
            &Txn::new().put("lifecycle/x", obj(json!(1)).with_ttl(60)),
            &req("other")
        ),
        Err(Error::TtlNotAllowed {
            key: "lifecycle/x".into()
        })
    );
    assert_eq!(c.head().unwrap(), head);
}

pub fn ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    put(
        &c,
        "telemetry/sections/A/x",
        obj(json!(1)).with_ttl(3_600),
        "1",
    );
    put(&c, "intents/A/x/1", obj(json!(1)).with_ttl(48 * 3_600), "2");
    gc.tick(T0 + HOUR_MS - 1).unwrap();
    assert!(c.get("telemetry/sections/A/x").unwrap().is_some());
    assert_eq!(
        gc.tick(T0 + HOUR_MS).unwrap().expired,
        ["telemetry/sections/A/x"]
    );
    assert!(c.get("telemetry/sections/A/x").unwrap().is_none());
    gc.tick(T0 + 48 * HOUR_MS - 1).unwrap();
    assert!(c.get("intents/A/x/1").unwrap().is_some());
    assert!(gc
        .tick(T0 + 48 * HOUR_MS)
        .unwrap()
        .expired
        .contains(&"intents/A/x/1".to_string()));
    assert!(c.get("intents/A/x/1").unwrap().is_none());
}

pub fn a_rewrite_restarts_a_keys_ttl<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    put(&c, "telemetry/s", obj(json!(1)).with_ttl(3_600), "1");
    clock.advance(HOUR_MS / 2);
    put(&c, "telemetry/s", obj(json!(2)).with_ttl(3_600), "2");
    gc.tick(T0 + HOUR_MS).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_some());
    gc.tick(T0 + HOUR_MS / 2 + HOUR_MS - 1).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_some());
    gc.tick(T0 + HOUR_MS / 2 + HOUR_MS).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_none());
}

pub fn reopen_restarts_every_ttl<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let b = rig.fresh();
    let c = open(rig.reopen(&b), &clock);
    put(&c, "telemetry/s", obj(json!(1)).with_ttl(3_600), "1");
    put(&c, "intents/A/x/1", obj(json!(1)).with_ttl(3_600), "2");
    clock.advance(HOUR_MS / 2);
    drop(c);
    let c = open(rig.reopen(&b), &clock);
    let gc = Controller::new(&c);
    gc.tick(T0 + HOUR_MS).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_some());
    assert!(c.get("intents/A/x/1").unwrap().is_some());
    gc.tick(T0 + HOUR_MS / 2 + HOUR_MS).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_none());
    assert!(c.get("intents/A/x/1").unwrap().is_none());
}

pub fn a_gc_expiry_writes_no_events_key<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    let evented = put(&c, "intents/A/x/1", obj(json!(1)).with_ttl(60), "1");
    assert!(c.get(&events_key(evented)).unwrap().is_some());
    clock.advance(60_000);
    let before = c.head().unwrap();
    assert!(gc
        .tick(c.now_ms())
        .unwrap()
        .expired
        .contains(&"intents/A/x/1".to_string()));
    let after = c.head().unwrap();
    assert!(after > before);
    for m in before + 1..=after {
        assert!(
            c.get(&events_key(m)).unwrap().is_none(),
            "expiry at {m} was evented"
        );
    }
}

pub fn a_multi_prefix_list_reads_one_snapshot_while_a_commit_lands_between_prefixes<R: Rig>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let b = rig.fresh();
    let other = open(rig.reopen(&b), &clock);
    let c = open(Interpose::new(b), &clock);
    put(
        &c,
        "telemetry/sections/Round/a",
        obj(json!("s")).with_ttl(604_800),
        "1",
    );
    put(
        &c,
        "intents/Round/a/1",
        obj(json!("i")).with_ttl(172_800),
        "2",
    );
    put(
        &c,
        "telemetry/reconciler/pass",
        obj(json!("old")).with_ttl(86_400),
        "3",
    );
    let snap = c.head().unwrap();
    c.backend().after_next_range(move || {
        let t = Txn::new()
            .put(
                "telemetry/reconciler/pass",
                obj(json!("new")).with_ttl(86_400),
            )
            .put("intents/Round/a/2", obj(json!("i2")).with_ttl(172_800))
            .put(
                "telemetry/sections/Round/b",
                obj(json!("s2")).with_ttl(604_800),
            );
        assert!(other.txn(&t, &req("between")).unwrap().succeeded);
    });
    let (items, list_rev) = c
        .list(
            &[
                "telemetry/sections/",
                "intents/",
                "telemetry/reconciler/pass",
            ],
            &Selector::Everything,
        )
        .unwrap();
    assert_eq!(list_rev, snap);
    let mut got: Vec<(String, serde_json::Value)> =
        items.into_iter().map(|i| (i.key, i.value.data)).collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        [
            ("intents/Round/a/1".to_string(), json!("i")),
            ("telemetry/reconciler/pass".to_string(), json!("old")),
            ("telemetry/sections/Round/a".to_string(), json!("s")),
        ]
    );
    assert!(
        c.head().unwrap() > snap,
        "the commit landed during the list"
    );
}

fn puts(n: usize) -> Txn {
    (0..n).fold(Txn::new(), |t, i| t.put(&format!("l/{i}"), obj(json!(i))))
}

pub fn txn_ops_at_the_limit_pass_and_one_over_is_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let bookkeeping = 2;
    assert!(
        c.txn(&puts(MAX_TXN_OPS - bookkeeping), &req("at"))
            .unwrap()
            .succeeded
    );
    let head = c.head().unwrap();
    assert_eq!(
        c.txn(&puts(MAX_TXN_OPS - bookkeeping + 1), &req("over")),
        Err(Error::Limit(Limit::Ops {
            max: MAX_TXN_OPS,
            got: MAX_TXN_OPS + 1
        }))
    );
    assert_eq!(c.head().unwrap(), head);
    let telemetry = |n: usize| {
        (0..n).fold(Txn::new(), |t, i| {
            t.put(&format!("telemetry/l/{i}"), obj(json!(i)).with_ttl(60))
        })
    };
    assert!(
        c.txn(&telemetry(MAX_TXN_OPS - 1), &req("tel-at"))
            .unwrap()
            .succeeded
    );
    assert_eq!(
        c.txn(&telemetry(MAX_TXN_OPS), &req("tel-over")),
        Err(Error::Limit(Limit::Ops {
            max: MAX_TXN_OPS,
            got: MAX_TXN_OPS + 1
        }))
    );
    let mut else_over = Txn::new().when(Compare::ModRev("l/0".into(), None));
    else_over.else_ops = puts(MAX_TXN_OPS - bookkeeping + 1).ops;
    assert_eq!(
        c.txn(&else_over, &req("else-over")),
        Err(Error::Limit(Limit::Ops {
            max: MAX_TXN_OPS,
            got: MAX_TXN_OPS + 1
        }))
    );
}

pub fn txn_compares_at_the_limit_pass_and_one_over_is_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let cmp = |n: usize| {
        (0..n).fold(Txn::new().put("k", obj(json!(1))), |t, i| {
            t.when(Compare::ModRev(format!("absent/{i}"), None))
        })
    };
    assert!(c.txn(&cmp(MAX_TXN_COMPARES), &req("at")).unwrap().succeeded);
    assert_eq!(
        c.txn(&cmp(MAX_TXN_COMPARES + 1), &req("over")),
        Err(Error::Limit(Limit::Compares {
            max: MAX_TXN_COMPARES,
            got: MAX_TXN_COMPARES + 1
        }))
    );
}

/// A round's cut, shaped as `limits::cut_ops` counts it: each member's record, delivery and
/// gate written under compares of all three, with its history entry; the round, its id index and
/// its history entry, the round and index compared absent. The contract adds the events and
/// request keys.
fn cut(members: usize, round: &str) -> Txn {
    let batch = format!("batches/r/{round}");
    let index = format!("batch-ids/{round}");
    let ids: Vec<String> = (0..members).map(|i| format!("{round}-{i}")).collect();
    let t = Txn::new()
        .when(Compare::ModRev(batch.clone(), None))
        .when(Compare::ModRev(index.clone(), None))
        .put(&batch, obj(json!({ "members": ids })))
        .put(&index, obj(json!({ "repo": "r" })))
        .put(&format!("history/{round}/0"), obj(json!("Cut")));
    ids.iter().fold(t, |t, id| {
        t.when(Compare::ModRev(format!("lifecycle/{id}"), None))
            .when(Compare::ModRev(format!("deliveries/{id}"), None))
            .when(Compare::ModRev(format!("gates/{id}"), None))
            .put(&format!("lifecycle/{id}"), obj(json!("IN_DELIVERY")))
            .put(&format!("deliveries/{id}"), obj(json!({ "round": round })))
            .put(&format!("gates/{id}"), obj(json!({ "class": "finished" })))
            .put(&format!("history/{id}/0"), obj(json!("Enqueued")))
    })
}

pub fn a_cut_at_the_member_cap_passes_the_limits_and_one_more_is_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    assert!(limits::cut_fits(limits::MAX_CUT_MEMBERS, 0));
    assert!(!limits::cut_fits(limits::MAX_CUT_MEMBERS + 1, 0));
    let at = c.txn(&cut(limits::MAX_CUT_MEMBERS, "at"), &req("at")).unwrap();
    assert!(at.succeeded);
    let head = c.head().unwrap();
    assert!(matches!(
        c.txn(&cut(limits::MAX_CUT_MEMBERS + 1, "over"), &req("over")),
        Err(Error::Limit(Limit::Ops { .. }))
    ));
    assert_eq!(c.head().unwrap(), head);
}

pub fn request_bytes_at_the_limit_pass_and_one_over_is_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let sized = |pad: usize| Txn::new().put("big", obj(json!("a".repeat(pad))));
    let base = c.request_bytes(&sized(0), &req("at")).unwrap();
    let at = sized(MAX_REQUEST_BYTES - base);
    assert_eq!(c.request_bytes(&at, &req("at")).unwrap(), MAX_REQUEST_BYTES);
    assert!(c.txn(&at, &req("at")).unwrap().succeeded);
    let base = c.request_bytes(&sized(0), &req("ov")).unwrap();
    let over = sized(MAX_REQUEST_BYTES - base + 1);
    assert_eq!(
        c.txn(&over, &req("ov")),
        Err(Error::Limit(Limit::RequestBytes {
            max: MAX_REQUEST_BYTES,
            got: MAX_REQUEST_BYTES + 1
        }))
    );
}

pub fn a_key_twice_in_one_txn_is_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let t = Txn::new()
        .put("d/x", obj(json!(1)))
        .put("d/x", obj(json!(2)));
    assert_eq!(
        c.txn(&t, &req("1")),
        Err(Error::Limit(Limit::DuplicateKey { key: "d/x".into() }))
    );
    let t = Txn::new().put("d/x", obj(json!(1))).delete("d/x");
    assert_eq!(
        c.txn(&t, &req("2")),
        Err(Error::Limit(Limit::DuplicateKey { key: "d/x".into() }))
    );
    let once_per_branch = Txn::new()
        .when(Compare::ModRev("d/x".into(), None))
        .put("d/x", obj(json!(1)))
        .or_put("d/x", obj(json!(2)));
    assert!(c.txn(&once_per_branch, &req("3")).unwrap().succeeded);
    assert_eq!(data(&c, "d/x"), Some(json!(1)));
}

pub fn caller_writes_to_the_contracts_own_prefixes_are_refused<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    for key in ["events/00000000000000000001", "requests/x", "locks/x"] {
        assert_eq!(
            c.txn(&Txn::new().put(key, obj(json!(1))), &req("1")),
            Err(Error::ReservedKey { key: key.into() })
        );
    }
    let mut else_lock = Txn::new().when(Compare::ModRev("a".into(), None));
    else_lock.else_ops = Txn::new().lock_acquire("x", MIN_LOCK_TTL_MS).ops;
    assert_eq!(
        c.txn(&else_lock, &req("2")),
        Err(Error::ReservedKey {
            key: "locks/x".into()
        })
    );
}

/// A claim of `lifecycle/x` as one txn: the record unchanged since `at`, the lock taken, the record
/// written.
fn claim(at: Rev, v: &str) -> Txn {
    Txn::new()
        .when(Compare::ModRev("lifecycle/x".into(), Some(at)))
        .lock_acquire("lifecycle/x", 5_000)
        .put("lifecycle/x", obj(json!(v)))
}

fn mod_rev<B: Backend>(c: &Contract<B>, key: &str) -> Rev {
    c.get(key).unwrap().expect(key).mod_rev
}

pub fn a_txn_that_acquires_a_lock_and_writes_a_fenced_key_applies_whole_or_not_at_all<R: Rig>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let c = open(Interpose::new(rig.fresh()), &clock);
    put(&c, "lifecycle/x", obj(json!("open")), "seed1");
    let stale = mod_rev(&c, "lifecycle/x");
    put(&c, "lifecycle/x", obj(json!("ready")), "seed2");

    let refused = c.txn(&claim(stale, "a"), &req("stale")).unwrap();
    assert!(!refused.succeeded);
    assert!(refused.fences.is_empty());
    assert_eq!(data(&c, "lifecycle/x"), Some(json!("ready")));
    assert!(c.get("locks/lifecycle/x").unwrap().is_none());
    let b = c
        .lock("lifecycle/x", 5_000, &req("b-lock"))
        .expect("no TTL wait");
    c.release(&b, &req("b-release")).unwrap();

    let r = c
        .txn(&claim(mod_rev(&c, "lifecycle/x"), "a"), &req("claim"))
        .unwrap();
    assert!(r.succeeded);
    let lock = c.get("locks/lifecycle/x").unwrap().expect("lock taken");
    assert_eq!(mod_rev(&c, "lifecycle/x").main, r.rev);
    assert_eq!(lock.create_rev.main, r.rev);
    assert_eq!(
        r.fences,
        [Fence {
            resource: "lifecycle/x".into(),
            token: lock.create_rev
        }]
    );
    let checked = Txn::new()
        .lock_check(&r.fences[0])
        .put("jobs/x", obj(json!(1)));
    assert!(c.txn(&checked, &req("checked")).unwrap().succeeded);

    clock.advance(5_000);
    assert!(
        c.get("locks/lifecycle/x").unwrap().is_some(),
        "not yet collected"
    );
    let reclaim = Txn::new()
        .when(Compare::ModRev(
            "lifecycle/x".into(),
            Some(mod_rev(&c, "lifecycle/x")),
        ))
        .delete("absent/x")
        .put("lifecycle/x", obj(json!("b")))
        .lock_acquire("lifecycle/x", 5_000);
    let again = c.txn(&reclaim, &req("reclaim")).unwrap();
    assert!(again.succeeded, "an expired lock is taken in one call");
    let token = c.get("locks/lifecycle/x").unwrap().unwrap().create_rev;
    assert_eq!(
        (again.fences[0].token, token.sub),
        (token, 1),
        "the token is the lock's own sub, after a dropped delete"
    );
    assert!(again.fences[0].token > r.fences[0].token);
    assert_eq!(data(&c, "lifecycle/x"), Some(json!("b")));

    c.release(&again.fences[0], &req("release")).unwrap();
    let t = claim(mod_rev(&c, "lifecycle/x"), "c");
    c.backend().lose_next_commit_reply();
    assert!(c.txn(&t, &req("lost")).is_err());
    let token = c.get("locks/lifecycle/x").unwrap().unwrap().create_rev;
    let retry = c.txn(&t, &req("lost")).unwrap();
    assert_eq!(
        (retry.rev, retry.succeeded, retry.already_applied),
        (token.main, true, true)
    );
    assert_eq!(
        retry.fences,
        [Fence {
            resource: "lifecycle/x".into(),
            token
        }]
    );
}

pub fn a_txn_acquiring_a_lock_held_by_another_holder_fails_whole<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    put(&c, "lifecycle/x", obj(json!("open")), "seed");
    let b = c.lock("lifecycle/x", 5_000, &req("b")).unwrap();
    let t = Txn::new()
        .lock_acquire("lifecycle/x", 5_000)
        .put("lifecycle/x", obj(json!("a")))
        .or_put("markers/x", obj(json!("refused")));
    let r = c.txn(&t, &req("a")).unwrap();
    assert!(!r.succeeded);
    assert!(r.fences.is_empty());
    assert_eq!(data(&c, "lifecycle/x"), Some(json!("open")));
    assert_eq!(data(&c, "markers/x"), Some(json!("refused")));
    assert_eq!(c.get("locks/lifecycle/x").unwrap().unwrap().create_rev, b.token);
    assert_eq!(
        data(&c, &events_key(r.rev)).unwrap()["refused"],
        json!(true)
    );
}

pub fn lock_check_fails_the_txn_after_the_lock_expired_or_was_retaken<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let a = c.lock("job", 5_000, &req("a")).unwrap();
    put(&c, "jobs/1", obj(json!("start")), "seed");
    let guarded = |f: &Fence, v: &str| Txn::new().lock_check(f).put("jobs/1", obj(json!(v)));

    clock.advance(5_000);
    assert!(c.get("locks/job").unwrap().is_some(), "not yet collected");
    assert!(!c.txn(&guarded(&a, "expired"), &req("1")).unwrap().succeeded);
    assert_eq!(data(&c, "jobs/1"), Some(json!("start")));

    let b = c.lock("job", 5_000, &req("b")).unwrap();
    assert!(!c.txn(&guarded(&a, "retaken"), &req("2")).unwrap().succeeded);
    assert_eq!(data(&c, "jobs/1"), Some(json!("start")));
    assert!(c.txn(&guarded(&b, "b"), &req("3")).unwrap().succeeded);
}

pub fn get_and_list_return_a_locks_expiry_and_renew_moves_it_forward<R: Rig>(rig: &R) {
    const WALL: u64 = 1_700_000_000_000;
    let clock = VirtualClock::new(T0);
    let wall = VirtualClock::new(WALL);
    let c = Contract::open(
        rig.fresh(),
        Ports {
            mono: Arc::new(clock.clone()),
            wall: Arc::new(wall.clone()),
        },
        Config {
            min_lock_ttl_ms: MIN_LOCK_TTL_MS,
        },
    )
    .expect("open");
    let advance = |ms: u64| {
        clock.advance(ms);
        wall.advance(ms);
    };
    let expiry = |key: &str| {
        let got = c.get(key).unwrap().expect(key).lock_expires_at_ms;
        let prefix = &key[..=key.find('/').unwrap()];
        let (items, _) = c.list(&[prefix], &Selector::Everything).unwrap();
        let listed = items.iter().find(|i| i.key == key).expect(key);
        assert_eq!(listed.lock_expires_at_ms, got, "get and list agree");
        got
    };
    let a = c.lock("job", 5_000, &req("1")).unwrap();
    c.lock("other", 9_000, &req("2")).unwrap();
    assert_eq!(expiry("locks/job"), Some(WALL + 5_000), "on the wall clock");
    assert_eq!(expiry("locks/other"), Some(WALL + 9_000));
    advance(2_000);
    c.renew(&a, &req("3")).unwrap();
    assert_eq!(expiry("locks/job"), Some(WALL + 7_000));
    put(&c, "jobs/1", obj(json!(1)), "4");
    assert_eq!(expiry("jobs/1"), None);
    advance(5_000);
    assert_eq!(expiry("locks/job"), None, "an expired lock has no expiry");
}

pub fn a_renew_under_a_new_ttl_keeps_the_token_and_moves_the_expiry<R: Rig>(rig: &R) {
    const WALL: u64 = 1_700_000_000_000;
    let clock = VirtualClock::new(T0);
    let wall = VirtualClock::new(WALL);
    let c = Contract::open(
        rig.fresh(),
        Ports {
            mono: Arc::new(clock.clone()),
            wall: Arc::new(wall.clone()),
        },
        Config {
            min_lock_ttl_ms: MIN_LOCK_TTL_MS,
        },
    )
    .expect("open");
    let a = c.lock("job", 5_000, &req("1")).unwrap();
    clock.advance(1_000);
    wall.advance(1_000);
    c.renew_for(&a, 20_000, &req("2")).unwrap();
    let held = c.get("locks/job").unwrap().expect("held");
    assert_eq!(held.create_rev, a.token, "the token survives the renew");
    assert_eq!(held.lock_expires_at_ms, Some(WALL + 21_000));
    assert_eq!(
        c.renew_for(&a, MIN_LOCK_TTL_MS - 1, &req("3")),
        Err(Error::LockTtlBelowMinimum {
            min_ms: MIN_LOCK_TTL_MS,
            got_ms: MIN_LOCK_TTL_MS - 1
        })
    );
    clock.advance(19_999);
    assert!(matches!(
        c.lock("job", 5_000, &req("4")),
        Err(Error::Locked { .. })
    ));
    c.renew(&a, &req("5")).unwrap();
    clock.advance(19_999);
    assert!(
        matches!(c.lock("job", 5_000, &req("6")), Err(Error::Locked { .. })),
        "a plain renew keeps the new TTL"
    );
    clock.advance(1);
    let b = c.lock("job", 5_000, &req("7")).unwrap();
    assert_eq!(
        c.renew_for(&a, 20_000, &req("8")),
        Err(Error::LockLost {
            resource: "job".into()
        })
    );
    assert!(b.token > a.token);
}

/// A state-filtered range answers exactly the keys under the prefix whose `data.state` is that
/// string, in key order, paged by `after` and `limit`, at the head and at an older revision.
pub fn a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it<R: Rig>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    put(&c, "p/a", obj(json!({ "state": "A" })), "1");
    put(&c, "p/b", obj(json!({ "state": "B" })), "2");
    put(&c, "p/c", obj(json!({ "state": "A" })), "3");
    put(&c, "q/d", obj(json!({ "state": "A" })), "4");
    put(&c, "p/e", obj(json!({ "other": "A" })), "5");
    let then = put(&c, "p/f", obj(json!({ "state": { "A": 1 } })), "6");
    put(&c, "p/b", obj(json!({ "state": "A" })), "7");
    c.delete("p/a", None, &req("8")).expect("delete");
    let head = c.head().expect("head");
    let keys = |state: &str, after: Option<&str>, limit: usize, at: u64| -> Vec<String> {
        c.range_in_state("p/", state, after, limit, at)
            .expect("range_in_state")
            .into_iter()
            .map(|i| i.key)
            .collect()
    };
    assert_eq!(keys("A", None, 10, head), ["p/b", "p/c"]);
    assert_eq!(keys("A", Some("p/b"), 10, head), ["p/c"]);
    assert_eq!(keys("A", None, 1, head), ["p/b"]);
    assert!(keys("B", None, 10, head).is_empty(), "p/b left B");
    assert_eq!(keys("A", None, 10, then), ["p/a", "p/c"]);
    assert_eq!(keys("B", None, 10, then), ["p/b"]);
    let read = c.range_in_state("p/", "A", None, 10, head).expect("range_in_state");
    assert_eq!(read[0].value.data, json!({ "state": "A" }));
    assert_eq!(read[0].mod_rev.main, head - 1, "p/b as rewritten");
}

/// `gc_range` and `Contract::open` never visit `history/`, however many keys it holds: open
/// reads exactly the rows it reads from a store without them.
pub fn gc_range_and_open_skip_the_history_prefix<R: Rig>(rig: &R) {
    const HISTORY_KEYS: usize = 10_000;
    let clock = VirtualClock::new(T0);
    let open_rows = |history: usize| -> usize {
        let b = rig.fresh();
        if history > 0 {
            let value = obj(json!({})).encode();
            let batch: Vec<Write> = (0..history)
                .map(|i| Write::Put {
                    key: format!("{HISTORY_PREFIX}sp-x/{i:020}/r"),
                    value: value.clone(),
                    create_rev: Rev::new(1, 0),
                    prev: None,
                })
                .collect();
            b.commit(0, &batch).expect("history commit");
        }
        let c = open(b, &clock);
        for (i, k) in ["a", "g", "z"].into_iter().enumerate() {
            put(&c, k, obj(json!(i)), &format!("p{i}"));
        }
        let b = rig.reopen(c.backend());
        drop(c);
        let counted = Interpose::new(b);
        let c = open(counted, &clock);
        let rows = c.backend().gc_rows();
        let mut after: Option<String> = None;
        loop {
            let head = c.head().expect("head");
            let page = c.backend().gc_range(after.as_deref(), 500, head).expect("gc_range");
            assert!(
                page.iter().all(|(k, _)| !k.starts_with(HISTORY_PREFIX)),
                "gc_range visited a history key"
            );
            if page.len() < 500 {
                break;
            }
            after = page.last().map(|(k, _)| k.clone());
        }
        rows
    };
    let without = open_rows(0);
    assert!(without >= 3, "open scanned the plain keys: {without}");
    assert_eq!(open_rows(HISTORY_KEYS), without);
}

/// Every scenario, in order; a backend's own test file names each one as its own test.
pub fn run_all<R: Rig>(rig: &R) {
    for s in scenarios::<R>() {
        s(rig);
    }
}

type Scenario<R> = fn(&R);

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        /// The names every backend's test file must cover, one test each.
        pub const SCENARIOS: &[&str] = &[$(stringify!($name)),*];
        fn scenarios<R: Rig>() -> Vec<Scenario<R>> {
            vec![$($name::<R> as Scenario<R>),*]
        }
    };
}

scenarios!(
    cas_conflict_on_a_stale_mod_rev,
    a_refused_txn_writes_nothing_but_its_events_key_carrying_the_refusal,
    one_main_per_txn_with_main_sub_per_key,
    the_next_main_stays_greater_after_deleting_the_newest_keys_and_reopening,
    create_with_an_equal_value_succeeds_and_a_different_value_conflicts,
    list_pages_and_list_rev_come_from_one_snapshot_while_a_commit_lands_between_pages,
    watch_resumes_from_a_revision,
    a_watch_on_a_quiet_prefix_returns_progress_rev,
    watch_returns_too_old_after_compaction,
    a_lock_expires_on_the_virtual_clock,
    a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition,
    reopen_extends_every_lock,
    a_lock_ttl_below_the_minimum_is_refused,
    a_finalizer_blocks_delete_until_gc_removes_the_key_once_cleared,
    an_owner_reference_orphan_is_collected,
    a_request_retry_after_an_ambiguous_commit_reports_already_applied,
    retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome,
    request_keys_expire_at_24h_of_virtual_time,
    a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing,
    a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events,
    a_put_under_intents_alone_writes_one_events_key,
    a_put_under_telemetry_or_intents_without_ttl_secs_is_refused,
    ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before,
    a_rewrite_restarts_a_keys_ttl,
    reopen_restarts_every_ttl,
    a_gc_expiry_writes_no_events_key,
    a_multi_prefix_list_reads_one_snapshot_while_a_commit_lands_between_prefixes,
    txn_ops_at_the_limit_pass_and_one_over_is_refused,
    txn_compares_at_the_limit_pass_and_one_over_is_refused,
    a_cut_at_the_member_cap_passes_the_limits_and_one_more_is_refused,
    request_bytes_at_the_limit_pass_and_one_over_is_refused,
    a_key_twice_in_one_txn_is_refused,
    caller_writes_to_the_contracts_own_prefixes_are_refused,
    a_txn_that_acquires_a_lock_and_writes_a_fenced_key_applies_whole_or_not_at_all,
    a_txn_acquiring_a_lock_held_by_another_holder_fails_whole,
    lock_check_fails_the_txn_after_the_lock_expired_or_was_retaken,
    get_and_list_return_a_locks_expiry_and_renew_moves_it_forward,
    a_renew_under_a_new_ttl_keeps_the_token_and_moves_the_expiry,
    a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it,
    gc_range_and_open_skip_the_history_prefix,
);
