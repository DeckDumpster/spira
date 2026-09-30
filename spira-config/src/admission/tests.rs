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
fn a_dead_or_recycled_holder_is_reclaimed_with_a_telemetry_row() {
    let d = run_dir();
    let p = FakeProcs::with(&[(10, 1, 1), (20, 2, 1)]);
    try_take(&d, Pool::Compile, 1, &h(10, 1, "sp-a"), 7, 100, &p).unwrap();
    try_take(&d, Pool::Compile, 1, &h(10, 1, "sp-a"), 0, 140, &p).unwrap();
    // pid 10 reused by another process: a different starttime is a dead lease.
    p.0.borrow_mut().insert(10, (555, 1));
    let (t, ended) = try_take(&d, Pool::Compile, 1, &h(20, 2, "sp-b"), 0, 200, &p).unwrap();
    assert_eq!(t, Take::Admitted { slot: 1, fresh: true });
    assert_eq!(ended.len(), 1);
    let e = &ended[0];
    assert_eq!((e.lease.who.as_str(), e.held, e.end, e.lease.waited), ("sp-a", 40, "reclaimed", 7));
    let v: serde_json::Value = serde_json::from_str(&row(e, "2026-09-30T00:00:00Z", "box").unwrap()).unwrap();
    assert_eq!(v["family"], "admission");
    assert_eq!(v["pool"], "compile");
    assert_eq!(v["held_secs"], 40);
    assert_eq!(v["waited_secs"], 7);
    assert_eq!(v["end"], "reclaimed");
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
fn gate_occupancy_reads_flocks_and_their_holder_sidecars() {
    let d = run_dir();
    let dir = Pool::Gate.dir(&d);
    fs::create_dir_all(&dir).unwrap();
    for n in 1..=3 {
        File::create(dir.join(format!("slot.{n}.lock"))).unwrap();
    }
    let held = OpenOptions::new().write(true).open(dir.join("slot.2.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    fs::write(dir.join("slot.2.holder"), gate_holder_line(4242, 9, "concierge/sp-x", 100)).unwrap();
    let o = gate_occupancy(&d, 3);
    assert_eq!(o.holders.len(), 1);
    assert_eq!((o.holders[0].slot, o.holders[0].who.as_str(), o.holders[0].pid), (2, "concierge/sp-x", 4242));
    drop(held);
    assert!(gate_occupancy(&d, 3).holders.is_empty());
}

#[test]
fn jitter_is_bounded_spread_and_zero_disables() {
    assert_eq!(jitter(0, 12345), 0);
    let vals: Vec<u64> = (0..200).map(|s| jitter(20, s)).collect();
    assert!(vals.iter().all(|v| *v <= 20));
    let distinct: std::collections::BTreeSet<u64> = vals.iter().copied().collect();
    assert!(distinct.len() > 15, "consecutive seeds spread across the range: {distinct:?}");
}

#[test]
fn only_a_crate_compile_is_a_build() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(is_compile(&v(&["--crate-name", "gate", "--edition=2021", "gate/src/main.rs"])));
    assert!(!is_compile(&v(&["-vV"])));
    assert!(!is_compile(&v(&["-", "--crate-name___", "--print=file-names"])));
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
    assert_eq!(profile_weight("aeon"), 1);
}
