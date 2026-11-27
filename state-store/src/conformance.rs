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
use crate::gc::{Controller, ORPHANS_PER_PASS};
use crate::limits::{self, MAX_REQUEST_BYTES, MAX_TXN_COMPARES, MAX_TXN_OPS};
use crate::{events_key, request_key, Error, Limit, Object, Result, Rev, HISTORY_PREFIX, ORPHANS_PREFIX};

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
    returned: Mutex<Vec<String>>,
}

impl<B: Backend> Interpose<B> {
    pub fn new(inner: B) -> Self {
        Interpose {
            inner,
            after_range: Mutex::new(None),
            ambiguous_commit: AtomicBool::new(false),
            gc_rows: AtomicUsize::new(0),
            returned: Mutex::new(Vec::new()),
        }
    }

    /// Every key `range` and `range_outside` have returned so far.
    pub fn returned_keys(&self) -> Vec<String> {
        self.returned.lock().unwrap().clone()
    }

    fn record(&self, rows: &[(String, Version)]) {
        self.returned.lock().unwrap().extend(rows.iter().map(|(k, _)| k.clone()));
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
    fn get_many(&self, keys: &[&str], at: u64) -> Result<Vec<Option<Version>>> {
        self.inner.get_many(keys, at)
    }
    fn range(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let out = self.inner.range(prefix, after, limit, at);
        if let Ok(rows) = &out {
            self.record(rows);
        }
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
    fn compacted(&self) -> Result<u64> {
        self.inner.compacted()
    }
    fn gc_range(
        &self,
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let rows = self.inner.gc_range(after, limit, at)?;
        self.gc_rows.fetch_add(rows.len(), Ordering::SeqCst);
        self.record(&rows);
        Ok(rows)
    }
    fn range_outside(
        &self,
        skip: &[&str],
        after: Option<&str>,
        limit: usize,
        at: u64,
    ) -> Result<Vec<(String, Version)>> {
        let rows = self.inner.range_outside(skip, after, limit, at)?;
        self.record(&rows);
        Ok(rows)
    }
    fn range_newest_opened(&self, prefix: &str, since: i64, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        self.inner.range_newest_opened(prefix, since, limit, at)
    }
    fn range_by_seq(&self, prefix: &str, after_seq: u64, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        self.inner.range_by_seq(prefix, after_seq, limit, at)
    }
    fn range_kind_since(&self, prefix: &str, kind: &str, since: i64, after: Option<(i64, &str)>, limit: usize, at: u64) -> Result<Vec<(String, Version)>> {
        self.inner.range_kind_since(prefix, kind, since, after, limit, at)
    }
    fn due(&self, now: u64, limit: usize, at: u64) -> Result<Vec<String>> {
        self.inner.due(now, limit, at)
    }

    fn owned_by(&self, owner: &str, after: Option<&str>, limit: usize, at: u64) -> Result<Vec<String>> {
        self.inner.owned_by(owner, after, limit, at)
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
    let got = while_time_passes(&c, &clock, || c.watch("q/", r, 30_000));
    assert_eq!(
        got.unwrap(),
        Watched::Progress {
            progress_rev: c.head().unwrap()
        }
    );
    assert_eq!(
        c.watch("q/", r, 30_001),
        Err(Error::WaitTooLong {
            max_ms: 30_000,
            got_ms: 30_001
        })
    );
}

/// A watch on a thread of its own, answered within a real second. The virtual clock never
/// moves, so only the close can end a wait.
fn answered_at_once<B: Backend + 'static>(c: &Arc<Contract<B>>, from: u64) -> Result<Watched> {
    let (tx, rx) = std::sync::mpsc::channel();
    let c = c.clone();
    std::thread::spawn(move || {
        let _ = tx.send(c.watch_any(&["lifecycle/"], from, 30_000));
    });
    rx.recv_timeout(std::time::Duration::from_secs(1))
        .expect("the watch is still held a second after the close")
}

pub fn closing_the_watches_answers_a_held_watch_and_every_later_one_with_progress<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = Arc::new(open(rig.fresh(), &clock));
    let head = put(&c, "lifecycle/a", obj(json!(1)), "1");
    let (tx, rx) = std::sync::mpsc::channel();
    let held = c.clone();
    std::thread::spawn(move || {
        let _ = tx.send(held.watch_any(&["lifecycle/"], head, 30_000));
    });
    c.close_watches();
    let got = rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("the held watch is still held a second after the close");
    assert_eq!(got, Ok(Watched::Progress { progress_rev: head }));
    assert_eq!(
        answered_at_once(&c, head),
        Ok(Watched::Progress { progress_rev: head })
    );
}

pub fn closing_the_watches_still_delivers_events_already_committed<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = Arc::new(open(rig.fresh(), &clock));
    let before = c.head().unwrap();
    put(&c, "lifecycle/a", obj(json!(1)), "1");
    c.close_watches();
    let got = answered_at_once(&c, before);
    let Ok(Watched::Events { events, .. }) = got else {
        panic!("the commit is lost: {got:?}")
    };
    assert_eq!(
        events.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
        ["lifecycle/a"]
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

/// A value overwritten or deleted inside the history ring reads as it was at a revision between
/// the writes, and a key created after that revision does not appear.
pub fn a_value_overwritten_or_deleted_inside_the_ring_reads_as_of_a_revision_between_the_writes<
    R: Rig,
>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let r1 = c
        .txn(
            &Txn::new().put("h/a", obj(json!(1))).put("h/b", obj(json!("b"))),
            &req("1"),
        )
        .unwrap()
        .rev;
    put(&c, "h/a", obj(json!(2)), "2");
    let r3 = c
        .txn(&Txn::new().delete("h/b").put("h/c", obj(json!(3))), &req("3"))
        .unwrap()
        .rev;
    let at = |key: &str, rev: u64| c.get_at(key, rev).unwrap().map(|i| i.value.data);
    assert_eq!(at("h/a", r1), Some(json!(1)));
    assert_eq!(at("h/b", r1), Some(json!("b")));
    assert_eq!(at("h/c", r1), None);
    let keys = |rev: u64| -> Vec<(String, serde_json::Value)> {
        c.range_at("h/", None, 100, rev)
            .unwrap()
            .into_iter()
            .map(|i| (i.key, i.value.data))
            .collect()
    };
    assert_eq!(keys(r1), [("h/a".to_string(), json!(1)), ("h/b".to_string(), json!("b"))]);
    assert_eq!(keys(r3), [("h/a".to_string(), json!(2)), ("h/c".to_string(), json!(3))]);
    let page = c.range_at("h/", Some("h/a"), 1, r1).unwrap();
    assert_eq!(page.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(), ["h/b"]);
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

pub fn a_finalizer_blocks_delete_until_the_write_clearing_it_removes_the_key<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    c.create(
        "rec/1",
        obj(json!({"s": "DONE"})).with_finalizers(&["planning-closed", "other"]),
        &req("1"),
    )
    .unwrap();
    assert!(matches!(
        c.delete("rec/1", None, &req("2")).unwrap(),
        Deleted::Pending { .. }
    ));
    let held = c.get("rec/1").unwrap().expect("still present");
    assert_eq!(held.value.metadata.deletion_timestamp, Some(T0));
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("rec/1").unwrap().is_some());
    put(
        &c,
        "rec/1",
        obj(json!({"s": "DONE"})).with_finalizers(&["other"]),
        "3",
    );
    assert_eq!(
        c.get("rec/1")
            .unwrap()
            .unwrap()
            .value
            .metadata
            .deletion_timestamp,
        Some(T0),
        "a write keeping a finalizer keeps the mark"
    );
    let rev = put(&c, "rec/1", obj(json!({"s": "DONE"})), "4");
    assert!(c.get("rec/1").unwrap().is_none(), "no tick was needed");
    let event = data(&c, &events_key(rev)).expect("the clearing commit's record");
    let ops: Vec<(String, String)> = event["subs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["key"].as_str().unwrap().to_string(), s["op"].as_str().unwrap().to_string()))
        .collect();
    assert!(ops.contains(&("rec/1".to_string(), "delete".to_string())), "{ops:?}");
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

pub fn an_owner_deleted_with_more_dependents_than_a_pass_is_drained_over_passes<R: Rig>(rig: &R) {
    const DEPENDENTS: usize = 2_500;
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    put(&c, "owners/o", obj(json!(1)), "o");
    for (n, chunk) in (0..DEPENDENTS).collect::<Vec<_>>().chunks(100).enumerate() {
        let mut t = Txn::new();
        for i in chunk {
            t = t.put(&format!("deps/{i:05}"), obj(json!(1)).with_owners(&["owners/o"]));
        }
        assert!(c.txn(&t, &req(&format!("d{n}"))).unwrap().succeeded);
    }
    c.delete("owners/o", None, &req("del")).unwrap();
    let mut passes = Vec::new();
    for _ in 0..3 {
        passes.push(gc.tick(c.now_ms()).unwrap().orphans.len());
    }
    assert_eq!(passes, [ORPHANS_PER_PASS, ORPHANS_PER_PASS, DEPENDENTS - 2 * ORPHANS_PER_PASS]);
    let head = c.head().unwrap();
    assert!(c.backend().range("deps/", None, 1, head).unwrap().is_empty());
    assert!(c.backend().range(ORPHANS_PREFIX, None, 1, head).unwrap().is_empty());
}

pub fn a_put_naming_an_owner_the_batch_writes_or_one_that_lives_queues_no_marker<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let t = Txn::new()
        .put("owners/o", obj(json!(1)))
        .put("deps/a", obj(json!(1)).with_owners(&["owners/o"]));
    assert!(c.txn(&t, &req("1")).unwrap().succeeded);
    put(&c, "deps/b", obj(json!(1)).with_owners(&["owners/o", "owners/gone"]), "2");
    let head = c.head().unwrap();
    assert!(c.backend().range(ORPHANS_PREFIX, None, 1, head).unwrap().is_empty());
    put(&c, "deps/c", obj(json!(1)).with_owners(&["owners/gone"]), "3");
    let head = c.head().unwrap();
    let markers = c.backend().range(ORPHANS_PREFIX, None, 10, head).unwrap();
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].0, format!("{ORPHANS_PREFIX}owners/gone"));
}

pub fn an_owner_recreated_before_collection_keeps_its_dependents<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    put(&c, "owners/o", obj(json!(1)), "1");
    put(&c, "deps/a", obj(json!(1)).with_owners(&["owners/o"]), "2");
    c.delete("owners/o", None, &req("3")).unwrap();
    put(&c, "owners/o", obj(json!(2)), "4");
    assert!(gc.tick(c.now_ms()).unwrap().orphans.is_empty());
    assert!(c.get("deps/a").unwrap().is_some());
    let head = c.head().unwrap();
    assert!(c.backend().range(ORPHANS_PREFIX, None, 1, head).unwrap().is_empty());
    c.delete("owners/o", None, &req("5")).unwrap();
    assert_eq!(gc.tick(c.now_ms()).unwrap().orphans, ["deps/a"]);
}

pub fn owned_by_names_a_keys_dependents_until_they_drop_the_reference<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    put(&c, "owners/o", obj(json!(1)), "1");
    put(&c, "deps/a", obj(json!(1)).with_owners(&["owners/o"]), "2");
    put(&c, "deps/b", obj(json!(1)).with_owners(&["owners/o", "owners/p"]), "3");
    put(&c, "deps/c", obj(json!(1)).with_owners(&["owners/p"]), "4");
    let b = c.backend();
    let head = c.head().unwrap();
    let owned = |owner: &str, after: Option<&str>, limit: usize| {
        b.owned_by(owner, after, limit, c.head().unwrap()).unwrap()
    };
    assert_eq!(owned("owners/o", None, 10), ["deps/a", "deps/b"]);
    assert_eq!(owned("owners/o", None, 1), ["deps/a"]);
    assert_eq!(owned("owners/o", Some("deps/a"), 1), ["deps/b"]);
    assert!(owned("owners/o", Some("deps/b"), 1).is_empty());
    assert_eq!(owned("owners/p", None, 10), ["deps/b", "deps/c"]);
    put(&c, "deps/a", obj(json!(2)), "5");
    assert_eq!(owned("owners/o", None, 10), ["deps/b"]);
    c.delete("deps/b", None, &req("6")).unwrap();
    assert!(owned("owners/o", None, 10).is_empty());
    assert_eq!(owned("owners/p", None, 10), ["deps/c"]);
    assert!(matches!(
        b.owned_by("owners/o", None, 1, head),
        Err(Error::Stale { expected, .. }) if expected == head
    ));
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

pub fn the_gc_never_expires_a_receipt<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    let t = Txn::new().put("a", obj(json!(1)));
    let first = c.txn(&t, &req("day")).unwrap();
    clock.advance(25 * HOUR_MS);
    assert!(gc.tick(c.now_ms()).unwrap().expired.is_empty());
    assert!(c.get(&request_key("day")).unwrap().is_some());
    let retry = c.txn(&t, &req("day")).unwrap();
    assert_eq!((retry.rev, retry.already_applied), (first.rev, true));
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
    clock.advance(HOUR_MS - 1);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("telemetry/sections/A/x").unwrap().is_some());
    clock.advance(1);
    assert_eq!(
        gc.tick(c.now_ms()).unwrap().expired,
        ["telemetry/sections/A/x"]
    );
    assert!(c.get("telemetry/sections/A/x").unwrap().is_none());
    clock.advance(47 * HOUR_MS - 1);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("intents/A/x/1").unwrap().is_some());
    clock.advance(1);
    assert!(gc
        .tick(c.now_ms())
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
    clock.advance(HOUR_MS / 2);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_some());
    clock.advance(HOUR_MS / 2 - 1);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_some());
    clock.advance(1);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("telemetry/s").unwrap().is_none());
}

pub fn a_reopen_renews_no_lifetime<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let b = rig.fresh();
    let c = open(rig.reopen(&b), &clock);
    put(&c, "telemetry/s", obj(json!(1)).with_ttl(3_600), "1");
    put(&c, "intents/A/x/1", obj(json!(1)).with_ttl(3_600), "2");
    clock.advance(HOUR_MS / 2);
    drop(c);
    let c = open(rig.reopen(&b), &clock);
    clock.advance(HOUR_MS / 2);
    let expired = Controller::new(&c).tick(c.now_ms()).unwrap().expired;
    assert_eq!(expired.len(), 2, "{expired:?}");
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

pub fn a_retained_prefix_put_may_carry_a_ttl_and_expires_only_with_one<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let gc = Controller::new(&c);
    put(&c, "runtime/effect/t/pending", obj(json!(1)), "1");
    put(&c, "runtime/effect/t/closed", obj(json!(1)).with_ttl(3_600), "2");
    put(&c, "runtime/resolved/r1", obj(json!({})).with_ttl(3_600), "3");
    clock.advance(HOUR_MS - 1);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("runtime/effect/t/closed").unwrap().is_some());
    clock.advance(1);
    let before = c.head().unwrap();
    let mut expired = gc.tick(c.now_ms()).unwrap().expired;
    expired.sort();
    assert_eq!(expired, ["runtime/effect/t/closed", "runtime/resolved/r1"]);
    for m in before + 1..=c.head().unwrap() {
        assert!(c.get(&events_key(m)).unwrap().is_none(), "expiry at {m} was evented");
    }
    clock.advance(1_000 * HOUR_MS);
    gc.tick(c.now_ms()).unwrap();
    assert!(c.get("runtime/effect/t/pending").unwrap().is_some());
    assert_eq!(
        c.txn(&Txn::new().put("runtime/state/t/k", obj(json!(1)).with_ttl(60)), &req("s")),
        Err(Error::TtlNotAllowed { key: "runtime/state/t/k".into() })
    );
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

pub fn a_lock_renew_op_keeps_the_token_restarts_the_ttl_and_runs_only_while_held<R: Rig>(rig: &R) {
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
    let a = c.lock("job", 5_000, &req("1")).unwrap();
    advance(1_000);
    let t = |v: &str| {
        Txn::new()
            .lock_renew(&a, 20_000)
            .put("jobs/1", obj(json!(v)))
            .or_put("jobs/1", obj(json!("else")))
    };
    assert!(c.txn(&t("renewed"), &req("2")).unwrap().succeeded);
    let held = c.get("locks/job").unwrap().expect("held");
    assert_eq!(held.create_rev, a.token, "the token survives");
    assert_eq!(held.lock_expires_at_ms, Some(WALL + 21_000));
    assert_eq!(data(&c, "jobs/1"), Some(json!("renewed")));

    advance(20_000);
    assert!(!c.txn(&t("lapsed"), &req("3")).unwrap().succeeded);
    assert_eq!(data(&c, "jobs/1"), Some(json!("else")));

    let b = c.lock("job", 5_000, &req("4")).unwrap();
    let before = c.get("locks/job").unwrap().unwrap().lock_expires_at_ms;
    assert!(!c.txn(&t("retaken"), &req("5")).unwrap().succeeded);
    assert_eq!(c.get("locks/job").unwrap().unwrap().create_rev, b.token);
    assert_eq!(
        c.get("locks/job").unwrap().unwrap().lock_expires_at_ms,
        before
    );
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

/// A kind-and-time range answers exactly the keys under the prefix of that `data.kind` with an
/// integer `data.at` above `since`, ascending by `(at, key)`, paged by `after` and `limit`, at
/// the head and at an older revision, as the scan default does. Keys live under `facts/`, the
/// only prefix a backend need index.
pub fn a_kind_since_range_reads_the_keys_of_that_kind_after_that_time_at_the_head_and_below_it<
    R: Rig,
>(
    rig: &R,
) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let fact = |kind: &str, at: serde_json::Value| obj(json!({ "kind": kind, "at": at }));
    put(&c, "facts/a", fact("k", json!(30)), "1");
    put(&c, "facts/b", fact("k", json!(10)), "2");
    put(&c, "facts/c", fact("k", json!(30)), "3");
    put(&c, "facts/d", fact("other", json!(20)), "4");
    put(&c, "facts/e", fact("k", json!("20")), "5");
    put(&c, "other/x", fact("k", json!(40)), "6");
    let then = put(&c, "facts/f", fact("k", json!(25)), "7");
    put(&c, "facts/b", fact("k", json!(50)), "8");
    c.delete("facts/a", None, &req("9")).expect("delete");
    let head = c.head().expect("head");
    let keys = |since: i64, after: Option<(i64, &str)>, limit: usize, at: u64| -> Vec<String> {
        c.range_kind_since("facts/", "k", since, after, limit, at)
            .expect("range_kind_since")
            .into_iter()
            .map(|i| i.key)
            .collect()
    };
    assert_eq!(keys(i64::MIN, None, 10, head), ["facts/f", "facts/c", "facts/b"]);
    assert_eq!(keys(25, None, 10, head), ["facts/c", "facts/b"]);
    assert_eq!(keys(i64::MIN, Some((25, "facts/f")), 10, head), ["facts/c", "facts/b"]);
    assert_eq!(keys(i64::MIN, None, 1, head), ["facts/f"]);
    assert!(keys(i64::MAX, None, 10, head).is_empty());
    assert_eq!(keys(i64::MIN, None, 10, then), ["facts/b", "facts/f", "facts/a", "facts/c"]);
    assert_eq!(keys(10, Some((30, "facts/a")), 10, then), ["facts/c"]);
    for (since, after, limit, at) in [(i64::MIN, None, 2, head), (20, Some((25, "facts/f")), 5, then), (i64::MIN, None, 3, then)] {
        let by_scan = crate::backend::range_kind_since_by_scan(c.backend(), "facts/", "k", since, after, limit, at)
            .expect("scan");
        let read = c.range_kind_since("facts/", "k", since, after, limit, at).expect("range_kind_since");
        assert_eq!(read.iter().map(|i| i.key.clone()).collect::<Vec<_>>(), by_scan.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>());
    }
}

/// A seq range answers exactly the keys under the prefix with an integer `data.seq` above
/// `after_seq`, ascending by `(seq, key)`, at the head and at an older revision, as the scan
/// default does.
pub fn a_seq_range_reads_the_keys_above_a_seq_in_seq_order_at_the_head_and_below_it<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let seq = |s: serde_json::Value| obj(json!({ "seq": s }));
    put(&c, "history/a", seq(json!(30)), "1");
    put(&c, "history/b", seq(json!(10)), "2");
    put(&c, "history/c", seq(json!(30)), "3");
    put(&c, "history/d", seq(json!("20")), "4");
    put(&c, "history/e", obj(json!({})), "5");
    put(&c, "other/x", seq(json!(5)), "6");
    let then = put(&c, "history/f", seq(json!(25)), "7");
    put(&c, "history/b", seq(json!(50)), "8");
    c.delete("history/a", None, &req("9")).expect("delete");
    let head = c.head().expect("head");
    let keys = |after: u64, limit: usize, at: u64| -> Vec<String> {
        c.range_by_seq("history/", after, limit, at).expect("range_by_seq").into_iter().map(|i| i.key).collect()
    };
    assert_eq!(keys(0, 10, head), ["history/f", "history/c", "history/b"]);
    assert_eq!(keys(25, 10, head), ["history/c", "history/b"]);
    assert_eq!(keys(0, 1, head), ["history/f"]);
    assert!(keys(50, 10, head).is_empty());
    assert_eq!(keys(0, 10, then), ["history/b", "history/f", "history/a", "history/c"]);
    for (after, limit, at) in [(0, 2, head), (10, 5, then), (0, 3, then)] {
        let by_scan = crate::backend::range_by_seq_by_scan(c.backend(), "history/", after, limit, at).expect("scan");
        let read = c.range_by_seq("history/", after, limit, at).expect("range_by_seq");
        assert_eq!(read.iter().map(|i| i.key.clone()).collect::<Vec<_>>(), by_scan.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>());
    }
}

/// `gc_range` and `Contract::open` never visit `history/`, however many keys it holds: open
/// reads exactly the rows it reads from a store without them.
pub fn gc_range_skips_the_history_prefix<R: Rig>(rig: &R) {
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
        c.backend().gc_rows()
    };
    let without = open_rows(0);
    assert!(without >= 3, "gc_range read the plain keys: {without}");
    assert_eq!(open_rows(HISTORY_KEYS), without);
}

/// Every scenario, in order; a backend's own test file names each one as its own test.
pub fn run_all<R: Rig>(rig: &R) {
    for s in scenarios::<R>() {
        s(rig);
    }
}

type Scenario<R> = fn(&R);

pub fn committed_at_is_the_wall_time_of_the_first_audited_commit_at_or_after_a_main<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    let r1 = put(&c, "c/a", obj(json!(1)), "1");
    clock.advance(5_000);
    let r2 = put(&c, "c/b", obj(json!(2)), "2");
    assert_eq!(c.committed_at(r1).unwrap(), Some(T0));
    assert_eq!(c.committed_at(r2).unwrap(), Some(T0 + 5_000));
    assert_eq!(c.committed_at(r2 + 1).unwrap(), None, "no record above the last");
    clock.advance(1_000);
    let tele = put(&c, "telemetry/c", obj(json!(3)).with_ttl(60), "3");
    clock.advance(1_000);
    put(&c, "c/d", obj(json!(4)), "4");
    assert_eq!(
        c.committed_at(tele).unwrap(),
        Some(T0 + 7_000),
        "a telemetry-only commit is answered by the next audited one"
    );
}

pub fn get_many_answers_what_get_answers_per_key_at_the_head_and_below_it<R: Rig>(rig: &R) {
    let clock = VirtualClock::new(T0);
    let c = open(rig.fresh(), &clock);
    put(&c, "kept", obj(json!(1)), "1");
    put(&c, "over", obj(json!(1)), "2");
    put(&c, "gone", obj(json!(1)), "3");
    let before = c.head().unwrap();
    put(&c, "over", obj(json!(2)), "4");
    c.delete("gone", None, &req("5")).unwrap();
    put(&c, "late", obj(json!(1)), "6");
    let keys = ["gone", "kept", "absent", "over", "late", "kept"];
    for at in [c.head().unwrap(), before] {
        let many = c.backend().get_many(&keys, at).unwrap();
        let each: Vec<_> = keys.iter().map(|k| c.backend().get(k, at).unwrap()).collect();
        assert_eq!(many, each, "backend at {at}");
        let items = c.get_many_at(&keys, at).unwrap();
        let each: Vec<_> = keys.iter().map(|k| c.get_at(k, at).unwrap()).collect();
        assert_eq!(items, each, "contract at {at}");
    }
    let head = c.backend().get_many(&keys, c.head().unwrap()).unwrap();
    assert!(head[0].is_none() && head[2].is_none() && head[1].is_some() && head[4].is_some());
    assert!(c.backend().get_many(&[], before).unwrap().is_empty());
}

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
    closing_the_watches_answers_a_held_watch_and_every_later_one_with_progress,
    closing_the_watches_still_delivers_events_already_committed,
    watch_returns_too_old_after_compaction,
    a_value_overwritten_or_deleted_inside_the_ring_reads_as_of_a_revision_between_the_writes,
    get_many_answers_what_get_answers_per_key_at_the_head_and_below_it,
    a_lock_expires_on_the_virtual_clock,
    a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition,
    reopen_extends_every_lock,
    a_lock_ttl_below_the_minimum_is_refused,
    a_finalizer_blocks_delete_until_the_write_clearing_it_removes_the_key,
    an_owner_reference_orphan_is_collected,
    an_owner_deleted_with_more_dependents_than_a_pass_is_drained_over_passes,
    a_put_naming_an_owner_the_batch_writes_or_one_that_lives_queues_no_marker,
    an_owner_recreated_before_collection_keeps_its_dependents,
    owned_by_names_a_keys_dependents_until_they_drop_the_reference,
    a_request_retry_after_an_ambiguous_commit_reports_already_applied,
    retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome,
    the_gc_never_expires_a_receipt,
    a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing,
    a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events,
    a_put_under_intents_alone_writes_one_events_key,
    a_put_under_telemetry_or_intents_without_ttl_secs_is_refused,
    ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before,
    a_rewrite_restarts_a_keys_ttl,
    a_reopen_renews_no_lifetime,
    a_gc_expiry_writes_no_events_key,
    a_retained_prefix_put_may_carry_a_ttl_and_expires_only_with_one,
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
    a_lock_renew_op_keeps_the_token_restarts_the_ttl_and_runs_only_while_held,
    a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it,
    a_kind_since_range_reads_the_keys_of_that_kind_after_that_time_at_the_head_and_below_it,
    a_seq_range_reads_the_keys_above_a_seq_in_seq_order_at_the_head_and_below_it,
    gc_range_skips_the_history_prefix,
    committed_at_is_the_wall_time_of_the_first_audited_commit_at_or_after_a_main,
);
