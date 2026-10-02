use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::process::Command;

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

/// sp-os3of: this used to hold the flock on an in-process fd, drop it, and assert the
/// slot free — but other tests in this binary fork real children (containment.rs,
/// repos.rs, resolve.rs all shell out to `git`), and a fork duplicates the open file
/// description. A concurrent fork's child kept a copy of the lock, open past this
/// process's own drop, until that child execs — so `gate_occupancy` after `drop(held)`
/// still saw the slot held and the test flipped red at round 209 on an unchanged tree
/// (law-a-test-that-flips-is-deleted; see DESIGN-admission.md's test plan note).
///
/// Fixed by holding the lock from a CHILD PROCESS this test owns instead of an in-process
/// fd: this process never opens `slot.2.lock` itself, so no concurrent fork anywhere in
/// the binary can ever inherit a copy of a lock it never held. `flock -x <file> sleep 60`
/// itself forks: the grandchild `sleep` is the actual holder, inheriting the locked fd
/// across the `flock` launcher's exec. `holder.kill()` SIGKILLs the whole process group
/// (both the launcher and that grandchild) and reaps the launcher, but the grandchild's
/// own fd-closing teardown is a separate task the kernel schedules independently — same
/// signal, not provably the same instant — so the release assertion polls briefly rather
/// than firing the instant `kill()` returns.
#[test]
fn gate_occupancy_reads_flocks_and_their_holder_sidecars() {
    let d = run_dir();
    let dir = Pool::Gate.dir(&d);
    fs::create_dir_all(&dir).unwrap();
    for n in 1..=3 {
        File::create(dir.join(format!("slot.{n}.lock"))).unwrap();
    }
    let lock_path = dir.join("slot.2.lock");
    let mut holder = testkit::ChildGuard::spawn(Command::new("flock").arg("-x").arg(&lock_path).arg("sleep").arg("60"));
    // Wait for the child to actually have the lock before asserting occupancy.
    let mut tries = 0;
    while gate_occupancy(&d, 3).holders.is_empty() && tries < 500 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        tries += 1;
    }
    fs::write(dir.join("slot.2.holder"), gate_holder_line(4242, 9, "concierge/sp-x", 100)).unwrap();
    let o = gate_occupancy(&d, 3);
    assert_eq!(o.holders.len(), 1);
    assert_eq!((o.holders[0].slot, o.holders[0].who.as_str(), o.holders[0].pid), (2, "concierge/sp-x", 4242));
    // Kill and reap the child BEFORE asserting release: this process never held the lock
    // itself, so once the actual holder (the grandchild `sleep`) finishes exiting, the
    // kernel has already dropped it — poll briefly for that, rather than asserting once.
    holder.kill();
    let mut tries = 0;
    let mut o = gate_occupancy(&d, 3);
    while !o.holders.is_empty() {
        assert!(tries < 500, "the lock was never released after the holder was killed");
        tries += 1;
        std::thread::sleep(std::time::Duration::from_millis(10));
        o = gate_occupancy(&d, 3);
    }
    assert!(o.holders.is_empty());
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

/// THE POOLS AS A MODEL, IN VIRTUAL TIME (sp-f4ig1 acceptance: correctness, no load on the box).
/// Fourteen fake agents released in one instant (the herd) walk compile → test with the phase
/// costs measured on 2026-09-30 (DESIGN-admission.md §5), every scan through the real
/// `try_take` / `release` against a real pool directory. Every fourth agent's build is a
/// release build (weight 4). Each tick checks the pools' invariants.
#[test]
fn fourteen_agents_in_one_instant_are_spread_across_the_phases_and_all_finish() {
    const N: u32 = 14;
    const COMPILE_PAR: u64 = 3;
    const TEST_PAR: u64 = 4;
    let d = run_dir();
    let p = FakeProcs::default();
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Phase {
        WaitCompile,
        Compile(u64),
        WaitTest,
        Test(u64),
        Done,
    }
    // Agent i: its cargo is pid 1000+i, its testenv pid 2000+i (different processes, as live).
    let cargo = |i: u32| 1000 + i;
    let testenv = |i: u32| 2000 + i;
    for i in 0..N {
        p.0.borrow_mut().insert(cargo(i), (i as u64 + 1, 1));
        p.0.borrow_mut().insert(testenv(i), (i as u64 + 1, 1));
    }
    let weight = |i: u32| if i % 4 == 3 { WEIGHT_RELEASE } else { 1 };
    let compile_secs = |i: u32| if weight(i) > 1 { 145 } else { 40 };
    const TEST_SECS: u64 = 78;
    let mut phase = vec![Phase::WaitCompile; N as usize];
    let mut compile_slot = vec![0u64; N as usize];
    let mut test_slot = vec![0u64; N as usize];
    let mut first_admit_order = Vec::new();
    let (mut max_compile_units, mut max_test, mut max_both_phases_busy) = (0u64, 0usize, 0usize);
    let start = 1_000_000u64;
    let mut now = start;
    while phase.iter().any(|ph| *ph != Phase::Done) {
        assert!(now - start < 20_000, "the model must finish: {phase:?}");
        for i in 0..N {
            let k = i as usize;
            match phase[k] {
                Phase::WaitCompile => {
                    let me = hw(cargo(i), i as u64 + 1, &format!("a{i}"), weight(i));
                    if let (Take::Admitted { slot, .. }, _) = try_take(&d, Pool::Compile, COMPILE_PAR, &me, 0, now, &p).unwrap() {
                        compile_slot[k] = slot;
                        first_admit_order.push(i);
                        phase[k] = Phase::Compile(now + compile_secs(i));
                    }
                }
                Phase::Compile(until) if now >= until => {
                    // The cargo exits: its lease is reclaimed by the next scan (never released).
                    p.kill(cargo(i));
                    phase[k] = Phase::WaitTest;
                }
                Phase::WaitTest => {
                    let me = h(testenv(i), i as u64 + 1, &format!("a{i}"));
                    if let (Take::Admitted { slot, .. }, _) = try_take(&d, Pool::Test, TEST_PAR, &me, 0, now, &p).unwrap() {
                        test_slot[k] = slot;
                        phase[k] = Phase::Test(now + TEST_SECS);
                    }
                }
                Phase::Test(until) if now >= until => {
                    let me = h(testenv(i), i as u64 + 1, &format!("a{i}"));
                    assert!(release(&d, Pool::Test, TEST_PAR, test_slot[k], &me, now).is_some());
                    phase[k] = Phase::Done;
                }
                _ => {}
            }
        }
        // Invariants, read back from the pool directories as `status` reads them.
        let c = occupancy(&d, Pool::Compile, COMPILE_PAR, &p);
        let units: u64 = c.holders.iter().map(|l| l.weight).sum();
        assert!(
            units <= COMPILE_PAR || c.holders.len() == 1,
            "compile pool over its size with more than one job: {:?}",
            c.holders
        );
        let t = occupancy(&d, Pool::Test, TEST_PAR, &p);
        assert!(t.holders.len() as u64 <= TEST_PAR, "{:?}", t.holders);
        max_compile_units = max_compile_units.max(units);
        max_test = max_test.max(t.holders.len());
        let compiling = phase.iter().filter(|ph| matches!(ph, Phase::Compile(_))).count();
        let testing = phase.iter().filter(|ph| matches!(ph, Phase::Test(_))).count();
        if compiling > 0 && testing > 0 {
            max_both_phases_busy = max_both_phases_busy.max(compiling + testing);
        }
        now += 1;
    }
    // Everyone finished, in bounded time, and the herd was spread: at some tick agents were
    // compiling AND testing at once.
    assert!(phase.iter().all(|ph| *ph == Phase::Done));
    assert!(max_both_phases_busy >= 2, "phases overlapped across agents");
    assert_eq!(max_test, TEST_PAR as usize, "the test pool filled");
    assert!(max_compile_units >= COMPILE_PAR, "the compile pool filled");
    // First come, first served: the herd (all queued at the same instant) is admitted by pid —
    // and no release build (3, 7, 11) was overtaken by a debug build queued after it.
    for r in [3u32, 7, 11] {
        let pos = first_admit_order.iter().position(|a| *a == r).unwrap();
        assert!(first_admit_order[..pos].iter().all(|a| *a < r), "{first_admit_order:?}");
    }
    // Nothing is left held, and every lease that ended was accounted in telemetry.
    assert!(occupancy(&d, Pool::Compile, COMPILE_PAR, &p).holders.is_empty());
    assert!(occupancy(&d, Pool::Test, TEST_PAR, &p).holders.is_empty());
}

#[test]
fn the_pool_sizes_and_jitter_are_typed_config_keys_exported_to_the_shell() {
    let doc = crate::validate("[spira]\nid_prefix = \"sp\"\ncompile_par = 5\ntest_par = 2\nsummon_jitter = 7\n").unwrap();
    let sh = crate::export_sh(&doc);
    for line in ["COMPILE_PAR='5'", "TEST_PAR='2'", "SUMMON_JITTER='7'"] {
        assert!(sh.lines().any(|l| l == line || l == line.replace('\'', "")), "{line} in {sh}");
    }
    // What conf.sh exports is what the pools read: SPIRA_COMPILE_PAR=5 sizes the compile pool.
    let b = Host { cores: 32, mem_avail_mib: 36_000 };
    assert_eq!(size(Pool::Compile, Some("5"), b), 5);
    assert!(crate::validate("[spira]\nid_prefix = \"sp\"\ncompile_par = \"many\"\n").is_err(), "typed: a non-number is refused");
    // spira.conf spelling converts too.
    let mut w = crate::convert::ConvertWarnings::default();
    let vars: std::collections::BTreeMap<String, String> =
        [("SPIRA_COMPILE_PAR", "6"), ("SPIRA_TEST_PAR", "3"), ("SPIRA_SUMMON_JITTER", "0")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let s = crate::convert::spira_section(&vars, &mut w).unwrap();
    assert!(w.0.is_empty(), "{:?}", w.0);
    assert_eq!((s.compile_par, s.test_par, s.summon_jitter), (Some(6), Some(3), Some(0)));
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
