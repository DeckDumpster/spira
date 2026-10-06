use super::*;
use landing_pass::gateq::Where;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

struct Tip(Option<String>);
impl Branches for Tip {
    fn tip(&self, _: &str, _: &str) -> Option<String> {
        self.0.clone()
    }
}

struct Tick(AtomicU64);
impl Clock for Tick {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

struct Scripted(RefCell<Vec<(i32, String)>>);
impl Gate for Scripted {
    fn gate(&self, _: &str, _: &str, _: &str, _: &str) -> (i32, String) {
        self.0.borrow_mut().remove(0)
    }
}

fn tmp(tag: &str) -> testkit::TempDir {
    testkit::TempDir::new(&format!("gate-worker-test-{tag}"))
}

fn drain_one(rc: i32, out: &str) -> GateRun {
    let d = tmp("one");
    let q = GateQueue::new(&d);
    q.enqueue(&Job::new("r", "spira/sp-a", "sp-a", "t1", false)).unwrap();
    let g = Scripted(RefCell::new(vec![(rc, out.to_string())]));
    let w = Worker { queue: &q, gate: &g, branches: &Tip(Some("t1".into())), clock: &Tick(AtomicU64::new(0)), lock_wait: 5400, log: &|_| {}, slot: 0 };
    assert_eq!(w.drain(), 1);
    q.take_done("r", "spira/sp-a", "t1").unwrap().run
}

#[test]
fn the_gates_own_statuses_keep_their_meaning() {
    assert_eq!(drain_one(0, "gate: VERDICT=PASS").outcome, GateOutcome::Pass);
    assert_eq!(drain_one(1, "gate: VERDICT=FAIL reason=suite suite=test-x.sh").outcome, GateOutcome::Fail);
    assert_eq!(drain_one(75, "gate: VERDICT=NO_VERDICT reason=lock").outcome, GateOutcome::NoVerdict);
    let b = drain_one(76, "gate: VERDICT=BASE_FAIL reason=base-red suite=test-y.sh");
    assert_eq!((b.outcome, b.suite.as_str()), (GateOutcome::BaseFail, "test-y.sh"));
}

#[test]
fn a_gate_that_never_ran_is_never_the_branchs_fault() {
    for (rc, why) in [(-1, "gate-did-not-start"), (127, "gate-not-found"), (126, "gate-not-found"), (137, "gate-killed"), (143, "gate-killed")] {
        let r = drain_one(rc, "");
        assert!(!r.outcome.blames_branch(), "status {rc} must not blame the branch");
        assert_eq!((r.outcome, r.reason.as_deref()), (GateOutcome::NoVerdict, Some(why)));
    }
}

#[test]
fn a_dropped_branch_or_a_moved_tip_files_no_verdict() {
    for tip in [None, Some("elsewhere".to_string())] {
        let d = tmp("gone");
        let q = GateQueue::new(&d);
        q.enqueue(&Job::new("r", "spira/sp-a", "sp-a", "t1", false)).unwrap();
        let g = Scripted(RefCell::new(vec![]));
        let w = Worker { queue: &q, gate: &g, branches: &Tip(tip), clock: &Tick(AtomicU64::new(0)), lock_wait: 1, log: &|_| {}, slot: 0 };
        assert_eq!(w.drain(), 0);
        assert_eq!(q.find("r", "spira/sp-a", "t1"), None);
    }
}

#[test]
fn a_dead_workers_claim_is_gated_by_the_next() {
    let d = tmp("recover");
    let q = GateQueue::new(&d);
    q.enqueue(&Job::new("r", "spira/sp-a", "sp-a", "t1", false)).unwrap();
    q.claim(0).unwrap();
    let g = Scripted(RefCell::new(vec![(0, String::new())]));
    let w = Worker { queue: &q, gate: &g, branches: &Tip(Some("t1".into())), clock: &Tick(AtomicU64::new(0)), lock_wait: 1, log: &|_| {}, slot: 0 };
    assert_eq!(w.drain(), 1);
    assert_eq!(q.find("r", "spira/sp-a", "t1"), Some(Where::Done));
}

#[test]
fn lock_wait_is_two_trials_whatever_is_asked() {
    assert_eq!(lock_wait(Some("100"), None), 200);
    assert_eq!(lock_wait(None, None), 5400);
    assert_eq!(lock_wait(Some("100"), Some("30")), 200);
    assert_eq!(lock_wait(Some("100"), Some("900")), 900);
}

/// Intervals as (start, end) in a shared clock.
fn overlaps(spans: &[(u64, u64)]) -> bool {
    spans.iter().enumerate().any(|(i, a)| spans.iter().skip(i + 1).any(|b| a.0 < b.1 && b.0 < a.1))
}

struct Timed<'a>(&'a Mutex<Vec<(u64, u64)>>, &'a Wall);
struct Wall;
impl Clock for Wall {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros() as u64
    }
}
impl Gate for Timed<'_> {
    fn gate(&self, _: &str, _: &str, _: &str, _: &str) -> (i32, String) {
        let s = self.1.now_ms();
        std::thread::sleep(std::time::Duration::from_millis(20));
        self.0.lock().unwrap().push((s, self.1.now_ms()));
        (0, String::new())
    }
}

#[test]
fn the_overlap_check_sees_a_deliberate_overlap() {
    let spans = Mutex::new(Vec::new());
    let wall = Wall;
    let t = Timed(&spans, &wall);
    std::thread::scope(|s| {
        s.spawn(|| t.gate("b", "r", "1", "x"));
        s.spawn(|| t.gate("b", "r", "1", "y"));
    });
    assert!(overlaps(&spans.lock().unwrap()), "two gates run at once must read as overlapping");
}

#[test]
fn two_branches_queued_for_one_tree_are_gated_one_at_a_time() {
    let d = tmp("serial");
    let q = GateQueue::new(&d);
    for (b, t) in [("spira/sp-1", "t1"), ("spira/sp-2", "t2"), ("spira/sp-3", "t3")] {
        q.enqueue(&Job::new("r", b, b, t, false)).unwrap();
    }
    let spans = Mutex::new(Vec::new());
    let wall = Wall;
    struct AnyTip;
    impl Branches for AnyTip {
        fn tip(&self, _: &str, b: &str) -> Option<String> {
            Some(format!("t{}", b.trim_start_matches("spira/sp-")))
        }
    }
    let w = Worker { queue: &q, gate: &Timed(&spans, &wall), branches: &AnyTip, clock: &wall, lock_wait: 1, log: &|_| {}, slot: 0 };
    assert_eq!(w.drain(), 3);
    let spans = spans.lock().unwrap();
    assert_eq!(spans.len(), 3);
    assert!(!overlaps(&spans), "gates overlapped: {spans:?}");
    let done: Vec<Done> = ["t1", "t2", "t3"].iter().enumerate().map(|(i, t)| q.take_done("r", &format!("spira/sp-{}", i + 1), t).unwrap()).collect();
    let recorded: Vec<(u64, u64)> = done.iter().map(|d| (d.started_ms, d.finished_ms)).collect();
    assert!(!overlaps(&recorded), "recorded verdict windows overlapped: {recorded:?}");
}

#[test]
fn worker_count_is_certify_par_floored_at_one() {
    assert_eq!(worker_count(0), 1);
    assert_eq!(worker_count(1), 1);
    assert_eq!(worker_count(4), 4);
}

#[test]
fn with_n_2_a_second_worker_gets_slot_1_and_a_third_exits() {
    let d = tmp("slots");
    let (slot_a, held_a) = acquire_slot(&d, 2).expect("slot 0 is free");
    assert_eq!(slot_a, 0);
    let (slot_b, held_b) = acquire_slot(&d, 2).expect("slot 1 is free");
    assert_eq!(slot_b, 1);
    assert!(acquire_slot(&d, 2).is_none(), "both of N=2's slots are held — a third must exit");
    drop(held_a);
    assert_eq!(acquire_slot(&d, 2).map(|(s, _)| s), Some(0), "releasing slot 0 frees it again");
    drop(held_b);
}

#[test]
fn n_1_is_the_old_single_slot_behavior() {
    let d = tmp("n1");
    let (slot, held) = acquire_slot(&d, worker_count(0)).expect("slot 0 is free");
    assert_eq!(slot, 0);
    assert_eq!(lock_path(&d, 0), d.join("worker.lock"), "slot 0 keeps the pre-existing lock name");
    assert!(acquire_slot(&d, worker_count(0)).is_none(), "N=1 admits only one worker, as before");
    drop(held);
}

/// The regression this change exists to prevent, at the `Worker` level (the queue-level
/// proof that a `claim` can never double-rename the same source file lives in
/// `landing_pass::gateq::tests`): two worker instances — one per slot, standing in for two
/// concurrent `gate-worker run` processes — draining the same queue gate every job exactly
/// once between them, and slot 0's drain never touches slot 1's own live, in-flight claim.
#[test]
fn two_workers_at_different_slots_each_gate_a_job_exactly_once() {
    let d = tmp("two-workers");
    let q = GateQueue::new(&d);
    for n in 1..=4 {
        let b = format!("spira/sp-{n}");
        q.enqueue(&Job::new("r", &b, &b, &format!("t{n}"), false)).unwrap();
    }
    struct AnyTip;
    impl Branches for AnyTip {
        fn tip(&self, _: &str, b: &str) -> Option<String> {
            Some(format!("t{}", b.trim_start_matches("spira/sp-")))
        }
    }
    struct Counting<'a>(&'a RefCell<Vec<String>>);
    impl Gate for Counting<'_> {
        fn gate(&self, branch: &str, _: &str, _: &str, _: &str) -> (i32, String) {
            self.0.borrow_mut().push(branch.to_string());
            (0, String::new())
        }
    }
    let calls = RefCell::new(Vec::new());
    let gate = Counting(&calls);

    // Slot 1 is already mid-gate, holding one job claimed — standing in for a second
    // `gate-worker run` process live right now.
    let held = q.claim(1).unwrap();
    assert_eq!(held.branch, "spira/sp-1");

    // Slot 0's drain must see, and gate, only the three jobs still in the inbox; slot 1's
    // claim is in a different directory and never comes near it.
    let w0 = Worker { queue: &q, gate: &gate, branches: &AnyTip, clock: &Tick(AtomicU64::new(0)), lock_wait: 1, log: &|_| {}, slot: 0 };
    assert_eq!(w0.drain(), 3);
    assert!(!calls.borrow().contains(&"spira/sp-1".to_string()), "slot 0 must not gate slot 1's live claim");
    assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Claimed), "slot 1's claim must still be outstanding");

    // Slot 1 now starts its own worker and finishes — the fourth and last verdict, via its
    // own slot's recover (the job it had mid-gate reads as a dead holder's claim, so it is
    // returned to the inbox and re-claimed, same as the single-slot design always did).
    let w1 = Worker { queue: &q, gate: &gate, branches: &AnyTip, clock: &Tick(AtomicU64::new(0)), lock_wait: 1, log: &|_| {}, slot: 1 };
    assert_eq!(w1.drain(), 1);

    let calls = calls.borrow();
    assert_eq!(calls.len(), 4, "the gate itself must run exactly four times, once per job");
    let mut uniq = calls.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 4, "a job was gated more than once: {calls:?}");
    for n in 1..=4 {
        assert_eq!(q.find("r", &format!("spira/sp-{n}"), &format!("t{n}")), Some(Where::Done));
    }
}

#[test]
fn a_superseded_release_stops_after_its_in_flight_job() {
    let d = tmp("stale");
    let q = GateQueue::new(&d);
    for n in ["a", "b"] {
        q.enqueue(&Job::new("r", &format!("spira/sp-{n}"), &format!("sp-{n}"), "t1", false)).unwrap();
    }
    let g = Scripted(RefCell::new(vec![(0, String::new()), (0, String::new())]));
    let w = Worker { queue: &q, gate: &g, branches: &Tip(Some("t1".into())), clock: &Tick(AtomicU64::new(0)), lock_wait: 1, log: &|_| {}, slot: 0 };
    let calls = std::cell::Cell::new(0);
    let filed = w.drain_while(&|| {
        calls.set(calls.get() + 1);
        calls.get() == 1
    });
    assert_eq!(filed, 1);
    assert_eq!(g.0.borrow().len(), 1, "the second job must not be gated");
}

#[test]
fn release_currency_follows_the_sibling_current_link() {
    let d = tmp("rel");
    let old = d.join("old");
    let new = d.join("new");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    assert!(release_is_current(&old), "no current link: a dev checkout is never superseded");
    std::os::unix::fs::symlink("old", d.join("current")).unwrap();
    assert!(release_is_current(&old));
    assert!(!release_is_current(&new));
    std::fs::remove_file(d.join("current")).unwrap();
    std::os::unix::fs::symlink("new", d.join("current")).unwrap();
    assert!(!release_is_current(&old));
}

#[test]
fn a_path_inside_a_release_follows_the_release_not_its_subdirectory() {
    let d = tmp("sub");
    for r in ["old", "new"] {
        std::fs::create_dir_all(d.join(r).join("spira")).unwrap();
    }
    std::os::unix::fs::symlink("old", d.join("current")).unwrap();
    assert!(release_is_current(&d.join("old/spira")));
    assert!(!release_is_current(&d.join("new/spira")));
    assert!(release_is_current(&d.join("current/spira")));
    std::fs::remove_file(d.join("current")).unwrap();
    std::os::unix::fs::symlink("new", d.join("current")).unwrap();
    assert!(!release_is_current(&d.join("old/spira")));
}
