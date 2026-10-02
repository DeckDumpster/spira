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
    let w = Worker { queue: &q, gate: &g, branches: &Tip(Some("t1".into())), clock: &Tick(AtomicU64::new(0)), lock_wait: 5400, log: &|_| {} };
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
        let w = Worker { queue: &q, gate: &g, branches: &Tip(tip), clock: &Tick(AtomicU64::new(0)), lock_wait: 1, log: &|_| {} };
        assert_eq!(w.drain(), 0);
        assert_eq!(q.find("r", "spira/sp-a", "t1"), None);
    }
}

#[test]
fn a_dead_workers_claim_is_gated_by_the_next() {
    let d = tmp("recover");
    let q = GateQueue::new(&d);
    q.enqueue(&Job::new("r", "spira/sp-a", "sp-a", "t1", false)).unwrap();
    q.claim().unwrap();
    let g = Scripted(RefCell::new(vec![(0, String::new())]));
    let w = Worker { queue: &q, gate: &g, branches: &Tip(Some("t1".into())), clock: &Tick(AtomicU64::new(0)), lock_wait: 1, log: &|_| {} };
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
    let w = Worker { queue: &q, gate: &Timed(&spans, &wall), branches: &AnyTip, clock: &wall, lock_wait: 1, log: &|_| {} };
    assert_eq!(w.drain(), 3);
    let spans = spans.lock().unwrap();
    assert_eq!(spans.len(), 3);
    assert!(!overlaps(&spans), "gates overlapped: {spans:?}");
    let done: Vec<Done> = ["t1", "t2", "t3"].iter().enumerate().map(|(i, t)| q.take_done("r", &format!("spira/sp-{}", i + 1), t).unwrap()).collect();
    let recorded: Vec<(u64, u64)> = done.iter().map(|d| (d.started_ms, d.finished_ms)).collect();
    assert!(!overlaps(&recorded), "recorded verdict windows overlapped: {recorded:?}");
}
