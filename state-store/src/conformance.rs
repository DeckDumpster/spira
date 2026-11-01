//! The conformance suite: the contract's specification, run identically on every backend. A
//! `Rig` supplies a fresh backend and reopens the same one, which is how a restart is staged.
//! Every scenario runs on a `VirtualClock`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use runtime::ports::VirtualClock;
use serde_json::json;

use crate::backend::{Backend, LogEntry, Version, Write};
use crate::contract::{
    Compare, Config, Contract, Created, Deleted, Ports, Request, Selector, Txn, Watched,
};
use crate::gc::Controller;
use crate::limits::{MAX_REQUEST_BYTES, MAX_TXN_COMPARES, MAX_TXN_OPS};
use crate::{events_key, request_key, Error, Limit, Object, Result, Rev};

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
}

impl<B: Backend> Interpose<B> {
    pub fn new(inner: B) -> Self {
        Interpose {
            inner,
            after_range: Mutex::new(None),
            ambiguous_commit: AtomicBool::new(false),
        }
    }

    pub fn after_next_range(&self, f: impl FnOnce() + Send + 'static) {
        *self.after_range.lock().unwrap() = Some(Box::new(f));
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

pub fn watch_delivers_a_txns_events_together<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let from = c.head().unwrap();
    let got = std::thread::scope(|s| {
        let h = s.spawn(|| c.watch("g/", from, 2_500));
        while c.waiting_watchers() == 0 {
            std::thread::yield_now();
        }
        let t = Txn::new()
            .put("g/a", obj(json!(1)))
            .put("g/b", obj(json!(2)))
            .put("g/c", obj(json!(3)));
        let r = c.txn(&t, &req("1")).unwrap();
        while !h.is_finished() {
            clock.advance(100);
            c.wake_watchers();
            std::thread::yield_now();
        }
        (r.rev, h.join().unwrap())
    });
    let (main, w) = got;
    let Watched::Events { events, through } = w.unwrap() else {
        panic!("progress, not events")
    };
    assert_eq!(
        events
            .iter()
            .map(|e| (e.key.as_str(), e.rev))
            .collect::<Vec<_>>(),
        [
            ("g/a", Rev::new(main, 0)),
            ("g/b", Rev::new(main, 1)),
            ("g/c", Rev::new(main, 2))
        ]
    );
    assert_eq!(through, main);
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
            &Txn::new().put("beads/x", obj(json!(1)).with_ttl(60)),
            &req("other")
        ),
        Err(Error::TtlNotAllowed {
            key: "beads/x".into()
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
    watch_delivers_a_txns_events_together,
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
    request_bytes_at_the_limit_pass_and_one_over_is_refused,
    a_key_twice_in_one_txn_is_refused,
    caller_writes_to_the_contracts_own_prefixes_are_refused,
);
