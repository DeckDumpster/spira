use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// A process table: pid → (starttime, ppid).
#[derive(Default)]
struct FakeProcs(RefCell<HashMap<u32, (u64, u32)>>);

impl FakeProcs {
    fn with(rows: &[(u32, u64, u32)]) -> FakeProcs {
        let p = FakeProcs::default();
        for (pid, start, ppid) in rows {
            p.0.borrow_mut().insert(*pid, (*start, *ppid));
        }
        p
    }
    fn kill(&self, pid: u32) {
        self.0.borrow_mut().remove(&pid);
    }
}

impl Procs for FakeProcs {
    fn start_of(&self, pid: u32) -> Option<u64> {
        self.0.borrow().get(&pid).map(|r| r.0)
    }
    fn ppid_of(&self, pid: u32) -> Option<u32> {
        self.0.borrow().get(&pid).map(|r| r.1)
    }
}

fn h(pid: u32, start: u64, who: &str) -> Holder {
    Holder { pid, start, who: who.into(), weight: 1 }
}

fn hw(pid: u32, start: u64, who: &str, weight: u64) -> Holder {
    Holder { pid, start, who: who.into(), weight }
}

fn run_dir() -> testkit::TempDir {
    testkit::TempDir::new("spira-admission")
}

#[test]
fn derived_sizes_follow_the_measured_costs() {
    // This box on 2026-09-30: 32 cores, ~36 GiB available.
    let b = Host { cores: 32, mem_avail_mib: 36_000 };
    assert_eq!(derive(Pool::Compile, b), 3);
    assert_eq!(derive(Pool::Test, b), 4);
    assert_eq!(derive(Pool::Gate, b), 8);
    // Memory binds before cores on a starved box; never below 1.
    let starved = Host { cores: 32, mem_avail_mib: 9_000 };
    assert_eq!(derive(Pool::Compile, starved), 2);
    assert_eq!(derive(Pool::Test, starved), 1);
    let tiny = Host { cores: 2, mem_avail_mib: 500 };
    for p in Pool::ALL {
        assert_eq!(derive(p, tiny), 1, "{p:?}");
    }
}

#[test]
fn a_positive_key_wins_anything_else_derives() {
    let b = Host { cores: 32, mem_avail_mib: 36_000 };
    assert_eq!(size(Pool::Compile, Some("5"), b), 5);
    assert_eq!(size(Pool::Compile, Some(" 2 "), b), 2);
    for v in [None, Some(""), Some("0"), Some("x"), Some("-1"), Some("2.5")] {
        assert_eq!(size(Pool::Compile, v, b), 3, "{v:?}");
    }
}

#[test]
fn pools_have_their_own_directories_and_keys() {
    let r = Path::new("/run/spira");
    assert_eq!(Pool::Gate.dir(r), Path::new("/run/spira/gate-admission"), "the gate's pool stays where landing-pass probes it");
    assert_eq!(Pool::Compile.dir(r), Path::new("/run/spira/compile-admission"));
    assert_eq!(Pool::Test.dir(r), Path::new("/run/spira/test-admission"));
    assert_eq!(Pool::Cold.dir(r), Path::new("/run/spira/cold-admission"));
    assert_eq!(Pool::Gate.size_env(), "SPIRA_CERTIFY_PAR");
    assert_eq!(Pool::parse("test"), Some(Pool::Test));
    assert_eq!(Pool::parse("tests"), None);
}

#[test]
fn a_lease_round_trips_and_a_blank_who_is_a_dash() {
    let l = Lease { slot: 2, pid: 10, start: 99, who: "sp-abc".into(), since: 5, waited: 3, last: 7, weight: 4 };
    assert_eq!(Lease::parse(2, &l.render()), Some(l));
    let blank = Lease { slot: 1, pid: 10, start: 99, who: "a b".into(), since: 5, waited: 0, last: 5, weight: 1 };
    // A lease written before weights existed reads as weight 1.
    assert_eq!(Lease::parse(1, "pid=10 start=99 who=x since=5 waited=0 last=5").unwrap().weight, 1);
    assert!(blank.render().contains("who=a_b "));
    assert_eq!(Lease::parse(1, "garbage"), None);
    assert_eq!(Lease::parse(1, ""), None);
}

#[test]
fn slots_fill_in_order_and_the_third_holder_waits_naming_the_two() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1), (30, 3, 1)]);
    let (t, _) = try_take(&d, Pool::Test, 2, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: true });
    let (t, _) = try_take(&d, Pool::Test, 2, &h(20, 2, "sp-b"), 4, 101, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 2, fresh: true });
    let (t, _) = try_take(&d, Pool::Test, 2, &h(30, 3, "sp-c"), 0, 130, &p).unwrap();
    let Take::Busy { holders } = t else { panic!("{t:?}") };
    assert_eq!(holders.iter().map(|l| l.who.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b"]);
    assert_eq!(
        wait_line(Pool::Test, 2, &holders, 130),
        "waiting for a test slot: 2 of 2 held by sp-a (pid 10, 30s), sp-b (pid 20, 29s)"
    );
    assert!(d.join("test-admission/wait.30").is_file(), "a waiter is visible to status");
    assert_eq!(occupancy(&d, Pool::Test, 2, &p).waiting, 1);
}

#[test]
fn the_same_holder_keeps_its_slot_and_refreshes_last() {
    // One cargo's many rustc invocations share one lease.
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1)]);
    try_take(&d, Pool::Compile, 1, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    let (t, _) = try_take(&d, Pool::Compile, 1, &h(10, 1, "sp-a"), 0, 160, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: false });
    let l = Lease::parse(1, &fs::read_to_string(d.join("compile-admission/slot.1")).unwrap()).unwrap();
    assert_eq!((l.since, l.last), (100, 160));
}

#[test]
fn a_descendant_of_a_holder_inherits_its_slot() {
    // testenv (pid 10) holds a compile lease; the cargo under it (pid 12, grandchild) runs on it.
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (11, 2, 10), (12, 3, 11)]);
    try_take(&d, Pool::Compile, 1, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    let (t, _) = try_take(&d, Pool::Compile, 1, &h(12, 3, "sp-a"), 0, 101, &p).unwrap();
    assert_eq!(t, Take::Inherited { slot: 1, pid: 10 });
    // POSITIVE CONTROL: an unrelated process with the pool full waits.
    p.0.borrow_mut().insert(40, (4, 1));
    let (t, _) = try_take(&d, Pool::Compile, 1, &h(40, 4, "sp-b"), 0, 101, &p).unwrap();
    assert!(matches!(t, Take::Busy { .. }), "{t:?}");
}

#[test]
fn release_removes_only_its_own_lease() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1)]);
    try_take(&d, Pool::Test, 2, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    assert_eq!(release(&d, Pool::Test, 2, 1, &h(20, 2, "sp-b"), 150), None);
    assert!(d.join("test-admission/slot.1").is_file());
    let e = release(&d, Pool::Test, 2, 1, &h(10, 1, "sp-a"), 150).unwrap();
    assert_eq!((e.held, e.end), (50, "released"));
    assert!(!d.join("test-admission/slot.1").exists());
}

#[test]
fn lowering_the_size_never_evicts_a_holder() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1), (30, 3, 1)]);
    try_take(&d, Pool::Test, 2, &h(10, 1, "a"), 0, 100, &p).unwrap();
    try_take(&d, Pool::Test, 2, &h(20, 2, "b"), 0, 100, &p).unwrap();
    // Size lowered to 1: both holders keep their slots; a newcomer waits.
    let (t, _) = try_take(&d, Pool::Test, 1, &h(20, 2, "b"), 0, 101, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 2, fresh: false });
    let (t, _) = try_take(&d, Pool::Test, 1, &h(30, 3, "c"), 0, 101, &p).unwrap();
    assert!(matches!(t, Take::Busy { .. }));
    // The pool drains to its new size before anyone is admitted: b's one unit fills it.
    p.kill(10);
    let (t, _) = try_take(&d, Pool::Test, 1, &h(30, 3, "c"), 0, 102, &p).unwrap();
    assert!(matches!(t, Take::Busy { .. }), "{t:?}");
    p.kill(20);
    let (t, _) = try_take(&d, Pool::Test, 1, &h(30, 3, "c"), 0, 103, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: true });
}

#[test]
fn acquire_waits_visibly_then_admits_and_the_guard_releases() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1)]);
    try_take(&d, Pool::Test, 1, &h(10, 1, "sp-held"), 0, now_epoch(), &p).unwrap();
    let mut said = Vec::new();
    let sleeps = RefCell::new(0);
    // The holder finishes during the first sleep.
    let sleep = |_: Duration| {
        *sleeps.borrow_mut() += 1;
        p.kill(10);
    };
    let q = Request { run: &d, pool: Pool::Test, holder_pid: 20, who: "sp-new", inherit: None, weight: 1 };
    let g = acquire(&q, &Seams { size_of: &|| 1, procs: &p, sleep: &sleep }, &mut |l: &str| said.push(l.to_string()));
    assert!(g.held());
    assert_eq!((g.slot(), g.token.as_str()), (1, "test:1"));
    assert_eq!(*sleeps.borrow(), 1);
    assert!(said[0].starts_with("waiting for a test slot: 1 of 1 held by sp-held (pid 10, "), "{said:?}");
    assert!(said[1].starts_with("admitted to test slot 1 after "), "{said:?}");
    assert!(!d.join("test-admission/wait.20").exists(), "an admitted waiter is no longer waiting");
    drop(g);
    assert!(!d.join("test-admission/slot.1").exists(), "the guard releases its slot");
    let rows = fs::read_to_string(d.join("tsd/admission.jsonl")).unwrap();
    assert!(rows.contains("\"end\":\"reclaimed\"") && rows.contains("\"end\":\"released\""), "{rows}");
}

#[test]
fn acquire_under_an_admitted_job_takes_nothing_and_never_waits() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1)]);
    try_take(&d, Pool::Test, 1, &h(10, 1, "sp-held"), 0, 100, &p).unwrap();
    let never = |_: Duration| panic!("an inherited job must not wait");
    let seams = Seams { size_of: &|| 1, procs: &p, sleep: &never };
    let g = acquire(&Request { run: &d, pool: Pool::Test, holder_pid: 20, who: "x", inherit: Some("gate:2"), weight: 1 }, &seams, &mut |_| {});
    assert!(!g.held());
    assert_eq!(g.token, "gate:2");
    // An empty value is not an inheritance.
    p.kill(10);
    let g = acquire(&Request { run: &d, pool: Pool::Test, holder_pid: 20, who: "x", inherit: Some(" "), weight: 1 }, &seams, &mut |_| {});
    assert!(g.held());
}

#[test]
fn an_unreadable_holder_runs_unadmitted_rather_than_failing() {
    let d = run_dir();
    let p = FakeProcs::default();
    let mut said = Vec::new();
    let q = Request { run: &d, pool: Pool::Compile, holder_pid: 77, who: "x", inherit: None, weight: 1 };
    let g = acquire(&q, &Seams { size_of: &|| 1, procs: &p, sleep: &|_| {} }, &mut |l: &str| said.push(l.to_string()));
    assert!(!g.held());
    assert!(said[0].contains("running without a compile slot"), "{said:?}");
}

#[test]
fn real_procs_reads_this_process() {
    let me = std::process::id();
    let start = RealProcs.start_of(me).expect("our own starttime");
    assert!(start > 0);
    assert_eq!(RealProcs.ppid_of(me), Some(std::os::unix::process::parent_id()));
    assert_eq!(RealProcs.start_of(u32::MAX - 1), None);
}

#[test]
fn only_a_crate_compile_is_a_build() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(is_compile(&v(&["--crate-name", "gate", "--edition=2021", "gate/src/main.rs"])));
    assert!(!is_compile(&v(&["-vV"])));
    let cargo_probe = ["-", "--crate-name", "___", "--print=file-names", "--crate-type", "bin", "--print=sysroot", "--print=cfg", "-Wwarnings"];
    assert!(!is_compile(&v(&cargo_probe)), "cargo's first probe needs no config and no slot");
    assert_eq!(wrapper_action(None, &v(&cargo_probe)), WrapperAction::Exec);
    assert!(is_compile(&v(&["--crate-name", "x", "src/main.rs", "--print=native-static-libs"])), "a compile that prints is still a compile");
}

#[test]
fn a_release_build_weighs_the_whole_pool_and_runs_alone() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1), (30, 3, 1)]);
    // Size 3 units: two debug builds leave one unit — not enough for a release build (4).
    try_take(&d, Pool::Compile, 3, &h(10, 1, "dbg-a"), 0, 100, &p).unwrap();
    let (t, _) = try_take(&d, Pool::Compile, 3, &hw(20, 2, "rel", WEIGHT_RELEASE), 0, 101, &p).unwrap();
    let Take::Busy { holders } = t else { panic!("{t:?}") };
    assert_eq!(wait_line(Pool::Compile, 3, &holders, 110), "waiting for a compile slot: 1 of 3 held by dbg-a (pid 10, 10s)");
    // FIFO: a later debug build that would fit does not jump the queued release build.
    let (t, _) = try_take(&d, Pool::Compile, 3, &h(30, 3, "dbg-late"), 0, 102, &p).unwrap();
    assert!(matches!(t, Take::Busy { .. }), "first come, first served: {t:?}");
    // The debug build ends: the pool is empty, so the heavier-than-the-pool build runs alone.
    p.kill(10);
    let (t, _) = try_take(&d, Pool::Compile, 3, &hw(20, 2, "rel", WEIGHT_RELEASE), 0, 120, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: true });
    let (t, _) = try_take(&d, Pool::Compile, 3, &h(30, 3, "dbg-late"), 0, 121, &p).unwrap();
    let Take::Busy { holders } = t else { panic!("{t:?}") };
    assert_eq!(wait_line(Pool::Compile, 3, &holders, 130), "waiting for a compile slot: 4 of 3 held by rel ×4 (pid 20, 10s)");
}

#[test]
fn the_queue_position_is_the_first_wait_not_the_latest_scan() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1), (30, 3, 1)]);
    try_take(&d, Pool::Test, 1, &h(10, 1, "held"), 0, 100, &p).unwrap();
    try_take(&d, Pool::Test, 1, &h(20, 2, "first"), 0, 101, &p).unwrap();
    try_take(&d, Pool::Test, 1, &h(30, 3, "second"), 0, 102, &p).unwrap();
    // "first" scans again later; it keeps its place.
    try_take(&d, Pool::Test, 1, &h(20, 2, "first"), 0, 150, &p).unwrap();
    p.kill(10);
    let (t, _) = try_take(&d, Pool::Test, 1, &h(30, 3, "second"), 0, 160, &p).unwrap();
    assert!(matches!(t, Take::Busy { .. }), "second may not jump first: {t:?}");
    let (t, _) = try_take(&d, Pool::Test, 1, &h(20, 2, "first"), 0, 161, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: true });
    // Then "second" is at the head of the queue.
    let (t, _) = try_take(&d, Pool::Test, 2, &h(30, 3, "second"), 0, 162, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 2, fresh: true });
}

#[test]
fn a_dead_waiter_loses_its_place() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1), (30, 3, 1)]);
    try_take(&d, Pool::Test, 1, &h(10, 1, "held"), 0, 100, &p).unwrap();
    try_take(&d, Pool::Test, 1, &h(20, 2, "dies"), 0, 101, &p).unwrap();
    try_take(&d, Pool::Test, 1, &h(30, 3, "lives"), 0, 102, &p).unwrap();
    p.kill(10);
    p.kill(20);
    let (t, _) = try_take(&d, Pool::Test, 1, &h(30, 3, "lives"), 0, 103, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: true });
    assert!(!d.join("test-admission/wait.20").exists());
}

#[test]
fn release_like_rustc_invocations_weigh_more() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(build_weight(&v(&["--crate-name", "x", "-C", "opt-level=z", "-C", "codegen-units=1"])), WEIGHT_RELEASE);
    assert_eq!(build_weight(&v(&["--crate-name", "x", "-Copt-level=3"])), WEIGHT_RELEASE);
    assert_eq!(build_weight(&v(&["--crate-name", "x", "-C", "lto"])), WEIGHT_RELEASE);
    assert_eq!(build_weight(&v(&["--crate-name", "x", "-C", "opt-level=0", "-C", "debuginfo=1"])), 1);
    assert_eq!(build_weight(&v(&["--crate-name", "x", "-C", "lto=off"])), 1);
    assert_eq!(build_weight(&v(&["--crate-name", "x"])), 1);
    assert_eq!(profile_weight("release"), WEIGHT_RELEASE);
    assert_eq!(profile_weight("dev"), 1);
}

#[test]
fn the_wrapper_queues_agents_takes_now_for_gates_and_execs_probes() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let compile = v(&["--crate-name", "gate", "-C", "opt-level=2"]);
    assert_eq!(wrapper_action(None, &compile), WrapperAction::Wait, "an agent's build queues");
    assert_eq!(wrapper_action(Some(" "), &compile), WrapperAction::Wait, "an empty token is not an admission");
    assert_eq!(wrapper_action(Some("gate"), &compile), WrapperAction::TakeNow);
    assert_eq!(wrapper_action(Some("gate:2"), &compile), WrapperAction::TakeNow);
    assert_eq!(wrapper_action(Some("test:1"), &compile), WrapperAction::Exec, "admitted elsewhere");
    assert_eq!(wrapper_action(None, &v(&["-vV"])), WrapperAction::Exec, "probes never queue");
    assert_eq!(wrapper_action(Some("gate"), &v(&["-vV"])), WrapperAction::Exec);
}

/// D11: with the pool full of agent leases, a gate build starts at once (oversubscribing), and a
/// new agent build waits until the gate build releases — even after an agent build finishes.
#[test]
fn a_gate_build_starts_at_once_on_a_full_pool_and_new_agent_builds_queue_behind_it() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (11, 2, 1), (12, 3, 1), (50, 5, 1), (60, 6, 1)]);
    for (pid, st, who) in [(10, 1, "agent-a"), (11, 2, "agent-b"), (12, 3, "agent-c")] {
        let (t, _) = try_take(&d, Pool::Compile, 3, &h(pid, st, who), 0, 100, &p).unwrap();
        assert!(matches!(t, Take::Admitted { .. }));
    }
    let gate = h(50, 5, "gate:concierge/sp-x");
    let (n, _) = take_now(&d, Pool::Compile, 3, &gate, 101, &p).unwrap();
    assert_eq!(n, Now::Took { slot: 4, held_before: 3, fresh: true }, "no wait, slot 4 of a pool of 3");
    assert_eq!(take_now_line(Pool::Compile, 3, 4, 1, 3), "gate build took compile slot 4 (weight 1) without waiting — oversubscribed: 4 of 3 held; new agent builds queue behind it");
    // An agent build finishes; the newcomer still waits: the gate's unit fills the freed room.
    p.kill(10);
    let (t, _) = try_take(&d, Pool::Compile, 3, &h(60, 6, "agent-new"), 0, 110, &p).unwrap();
    let Take::Busy { holders } = t else { panic!("{t:?}") };
    assert!(holders.iter().any(|l| l.who == "gate:concierge/sp-x"), "{holders:?}");
    // The gate build releases: now the newcomer is admitted.
    let e = release(&d, Pool::Compile, 3, 4, &gate, 130).unwrap();
    assert_eq!((e.held, e.end), (29, "released"));
    let (t, _) = try_take(&d, Pool::Compile, 3, &h(60, 6, "agent-new"), 0, 131, &p).unwrap();
    assert!(matches!(t, Take::Admitted { fresh: true, .. }), "{t:?}");
    // The same gate cargo asking again (its next rustc) keeps its lease; a gate build on an
    // empty pool is an ordinary lease, not an oversubscription.
    let d2 = run_dir();
    let (n, _) = take_now(&d2, Pool::Compile, 3, &gate, 200, &p).unwrap();
    assert_eq!(n, Now::Took { slot: 1, held_before: 0, fresh: true });
    let (n, _) = take_now(&d2, Pool::Compile, 3, &gate, 201, &p).unwrap();
    assert_eq!(n, Now::Took { slot: 1, held_before: 0, fresh: false });
    assert_eq!(take_now_line(Pool::Compile, 3, 1, 1, 0), "gate build took compile slot 1 (weight 1) without waiting; new agent builds queue behind it");
}

#[test]
fn status_lists_each_waiter_and_says_when_the_head_cannot_fit() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (30, 3, 1), (40, 4, 1)]);
    try_take(&d, Pool::Compile, 3, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    let (t, _) = try_take(&d, Pool::Compile, 3, &hw(30, 3, "sp-rel", 4), 0, 110, &p).unwrap();
    assert!(matches!(t, Take::Busy { .. }));
    let (t, _) = try_take(&d, Pool::Compile, 3, &h(40, 4, "sp-small"), 0, 120, &p).unwrap();
    assert!(matches!(t, Take::Busy { .. }), "FIFO holds the small job behind the head");
    let o = occupancy(&d, Pool::Compile, 3, &p);
    assert_eq!(o.waiting, 2);
    assert_eq!(
        o.waiters.iter().map(|w| (w.who.as_str(), w.pid, w.weight, w.since)).collect::<Vec<_>>(),
        [("sp-rel", 30, 4, 110), ("sp-small", 40, 1, 120)]
    );
    assert_eq!(o.head_blocked().as_deref(), Some("head needs 4 of 3: waits for an empty pool"));
}

#[test]
fn a_full_pool_or_a_fitting_head_is_not_reported_as_blocked() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1)]);
    try_take(&d, Pool::Test, 1, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    try_take(&d, Pool::Test, 1, &h(20, 2, "sp-b"), 0, 110, &p).unwrap();
    assert_eq!(occupancy(&d, Pool::Test, 1, &p).head_blocked(), None);
}

/// A process table whose compiler subtree CPU is a set value.
struct TickProcs {
    base: FakeProcs,
    ticks: RefCell<u64>,
}

impl Procs for TickProcs {
    fn start_of(&self, pid: u32) -> Option<u64> {
        self.base.start_of(pid)
    }
    fn ppid_of(&self, pid: u32) -> Option<u32> {
        self.base.ppid_of(pid)
    }
    fn compiler_ticks(&self, _pid: u32) -> Option<u64> {
        Some(*self.ticks.borrow())
    }
}

#[test]
fn stall_bound_zero_and_unknown_ticks_never_reclaim() {
    let d = run_dir();
    let p = TickProcs { base: FakeProcs::with(&[(10, 1, 1)]), ticks: RefCell::new(0) };
    try_take(d.path(), Pool::Compile, 1, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    let dir = Pool::Compile.dir(d.path());
    reap(&dir, Pool::Compile, 1, &p, 100, 0);
    let (live, _, _) = reap(&dir, Pool::Compile, 1, &p, 99_999, 0);
    assert_eq!(live.len(), 1);
    let q = FakeProcs::with(&[(10, 1, 1)]);
    let (live, _, _) = reap(&dir, Pool::Compile, 1, &q, 99_999, 600);
    assert_eq!(live.len(), 1);
    // The test pool is never stall-reclaimed.
    let t = Pool::Test.dir(d.path());
    try_take(d.path(), Pool::Test, 1, &h(10, 1, "sp-a"), 0, 100, &p).unwrap();
    reap(&t, Pool::Test, 1, &p, 100, 600);
    let (live, _, _) = reap(&t, Pool::Test, 1, &p, 99_999, 600);
    assert_eq!(live.len(), 1);
}

#[test]
fn a_fresh_lease_waits_out_the_stagger_while_others_are_held() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1), (30, 3, 1)]);
    let take = |pid, st, who, now| try_take_staggered(&d, Pool::Test, 3, &h(pid, st, who), 0, now, &p, 30).unwrap().0;
    assert_eq!(take(10, 1, "a", 100), Take::Admitted { slot: 1, fresh: true });
    assert!(matches!(take(20, 2, "b", 110), Take::Busy { .. }));
    assert_eq!(take(20, 2, "b", 130), Take::Admitted { slot: 2, fresh: true });
    assert!(matches!(take(30, 3, "c", 131), Take::Busy { .. }));
    assert_eq!(take(10, 1, "a", 140), Take::Admitted { slot: 1, fresh: false });
}

#[test]
fn the_stagger_never_holds_an_empty_pool_or_a_disabled_one() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1)]);
    let (t, _) = try_take_staggered(&d, Pool::Compile, 2, &h(10, 1, "a"), 0, 100, &p, 30).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: true });
    let (t, _) = try_take_staggered(&d, Pool::Compile, 2, &h(20, 2, "b"), 0, 101, &p, 0).unwrap();
    assert_eq!(t, Take::Admitted { slot: 2, fresh: true });
}

#[test]
fn a_second_cold_build_waits_on_the_cold_slot_until_the_first_releases() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1)]);
    let size = || derive(Pool::Cold, Host { cores: 32, mem_avail_mib: 36_000 });
    assert_eq!(size(), 1);
    let first = acquire(&Request { run: &d, pool: Pool::Cold, holder_pid: 10, who: "sp-a", inherit: None, weight: 1 }, &Seams { size_of: &size, procs: &p, sleep: &|_| panic!("the first cold build must not wait") }, &mut |_| {});
    assert!(first.held());
    let first = RefCell::new(Some(first));
    let mut said = Vec::new();
    let sleeps = RefCell::new(0);
    let sleep = |_: Duration| {
        *sleeps.borrow_mut() += 1;
        first.borrow_mut().take();
    };
    let second = acquire(&Request { run: &d, pool: Pool::Cold, holder_pid: 20, who: "sp-b", inherit: None, weight: 1 }, &Seams { size_of: &size, procs: &p, sleep: &sleep }, &mut |l: &str| said.push(l.to_string()));
    assert!(second.held());
    assert_eq!(*sleeps.borrow(), 1, "held for exactly as long as the first held");
    assert!(said[0].starts_with("waiting for a cold slot: 1 of 1 held by sp-a"), "{said:?}");
}

#[test]
fn only_a_toolchain_change_or_an_empty_target_is_cold() {
    let files = |f: &[&str]| f.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(cold_reason("Upgrade the Rust toolchain to 1.91", &[]).is_some());
    assert!(cold_reason("Fix a typo", &files(&["rust-toolchain.toml"])).is_some());
    assert!(cold_reason("Bump", &files(&["Cargo.lock"])).is_some());
    assert!(cold_reason("Bump", &files(&["flake.lock"])).is_some());
    assert_eq!(cold_reason("Fix a typo", &files(&["dwarf/Cargo.lock", "dwarf/src/run.rs"])), None, "only the top-level lock");
    let t = run_dir();
    assert!(target_is_cold(t.path()));
    fs::write(t.join(".rustc_info.json"), "{}").unwrap();
    assert!(!target_is_cold(t.path()));
}

#[test]
fn a_second_unit_tests_batch_waits_for_the_first_and_names_it_while_other_pools_are_free() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1), (30, 3, 1)]);
    let host = Host { cores: 32, mem_avail_mib: 36_000 };
    let unit = || derive(Pool::Unit, host);
    assert_eq!(unit(), 1);
    let first = acquire(&Request { run: &d, pool: Pool::Unit, holder_pid: 10, who: "sp-a", inherit: None, weight: 1 }, &Seams { size_of: &unit, procs: &p, sleep: &|_| panic!("the first unit batch must not wait") }, &mut |_| {});
    assert!(first.held());

    let test = || derive(Pool::Test, host);
    let other = acquire(&Request { run: &d, pool: Pool::Test, holder_pid: 30, who: "sp-c", inherit: None, weight: 1 }, &Seams { size_of: &test, procs: &p, sleep: &|_| panic!("a non-unit batch must not wait on the unit slot") }, &mut |_| {});
    assert!(other.held());

    let first = RefCell::new(Some(first));
    let mut said = Vec::new();
    let sleep = |_: Duration| {
        first.borrow_mut().take();
    };
    let second = acquire(&Request { run: &d, pool: Pool::Unit, holder_pid: 20, who: "sp-b", inherit: None, weight: 1 }, &Seams { size_of: &unit, procs: &p, sleep: &sleep }, &mut |l: &str| said.push(l.to_string()));
    assert!(second.held());
    assert!(said[0].starts_with("waiting for a unit slot: 1 of 1 held by sp-a"), "{said:?}");
}

#[test]
fn the_run_ceiling_is_the_cores_over_one_runs_cargo_jobs_and_never_below_one() {
    let big = Host { cores: 32, mem_avail_mib: 1 };
    assert_eq!(derive(Pool::Run, big), 5);
    assert_eq!(derive(Pool::Run, Host { cores: 4, mem_avail_mib: 1 }), 1);
    assert_eq!(size(Pool::Run, Some("3"), big), 3);
    assert_eq!(Pool::Run.size_env(), "SPIRA_TESTENV_MAX_RUNS");
}

#[test]
fn a_run_slot_waiter_says_what_it_waits_for() {
    let holders = [Lease { slot: 1, pid: 7, start: 1, who: "sp-a".into(), since: 100, waited: 0, last: 100, weight: 1 }];
    assert_eq!(wait_line(Pool::Run, 1, &holders, 130), "testenv: waiting for a CPU slot (1 running)");
}
