use std::sync::Mutex;

use runtime::ports::{Clock, VirtualClock};
use serde_json::json;

use super::*;
use crate::lease::acquire;
use crate::ports::{fields, Mark};
use crate::sim::bench::Bench;
use crate::slots::grant;

/// Advances the bench's clock by what it is asked to sleep.
struct Advance(VirtualClock);

impl Sleep for Advance {
    fn sleep_ms(&self, ms: u64) {
        self.0.advance(ms);
    }
}

struct NoSwitch;

impl Switches for NoSwitch {
    fn set(&self, _: &str) -> Option<String> {
        None
    }
}

/// A child that tears down `teardown_ms` after TERM, or never when it ignores TERM; KILL ends
/// it at once unless it is `unkillable`.
struct Stubborn {
    clock: VirtualClock,
    teardown_ms: Option<u64>,
    unkillable: bool,
    gone_at: Mutex<Option<u64>>,
    signals: Mutex<Vec<(u64, Signal)>>,
}

impl Stubborn {
    fn new(clock: &VirtualClock, teardown_ms: Option<u64>, unkillable: bool) -> Stubborn {
        Stubborn {
            clock: clock.clone(),
            teardown_ms,
            unkillable,
            gone_at: Mutex::default(),
            signals: Mutex::default(),
        }
    }
}

impl Child for Stubborn {
    fn poll(&self) -> Option<Result<CertifyEnd, VerbRefusal>> {
        None
    }
    fn mark(&self) -> Option<Mark> {
        None
    }
    fn signal(&self, sig: Signal) -> Result<(), String> {
        let now = self.clock.now_ms();
        crate::lock(&self.signals).push((now, sig));
        let at = match sig {
            Signal::Term => self.teardown_ms.map(|t| now + t),
            Signal::Kill if self.unkillable => None,
            Signal::Kill => Some(now),
        };
        let mut g = crate::lock(&self.gone_at);
        if let Some(at) = at {
            *g = Some(g.map_or(at, |x: u64| x.min(at)));
        }
        Ok(())
    }
    fn gone(&self) -> bool {
        crate::lock(&self.gone_at).is_some_and(|t| t <= self.clock.now_ms())
    }
}

fn env<'a>(sleep: &'a Advance, switches: &'a dyn Switches) -> RunEnv<'a> {
    RunEnv {
        switches,
        sleep,
        poll_ms: 1_000,
        teardown_ms: 30_000,
        kill_grace_ms: 5_000,
    }
}

#[test]
fn kill_child_terminates_the_group_and_waits_out_its_teardown() {
    let b = Bench::new();
    let p = b.ports();
    let sleep = Advance(b.clock.clone());
    let c = Stubborn::new(&b.clock, Some(12_000), false);
    let start = b.clock.now_ms();
    assert_eq!(
        kill_child(&p, &env(&sleep, &NoSwitch), &c, &mut || {}),
        Killed::Term
    );
    assert_eq!(*crate::lock(&c.signals), [(start, Signal::Term)]);
    assert_eq!(
        b.clock.now_ms() - start,
        12_000,
        "it waited for the teardown, no longer"
    );
}

#[test]
fn kill_child_escalates_to_kill_past_the_teardown_deadline() {
    let b = Bench::new();
    let p = b.ports();
    let sleep = Advance(b.clock.clone());
    let c = Stubborn::new(&b.clock, None, false);
    let start = b.clock.now_ms();
    assert_eq!(
        kill_child(&p, &env(&sleep, &NoSwitch), &c, &mut || {}),
        Killed::Kill
    );
    assert_eq!(
        *crate::lock(&c.signals),
        [(start, Signal::Term), (start + 30_000, Signal::Kill)]
    );
}

#[test]
fn a_child_kill_cannot_end_lingers_and_one_already_gone_is_not_signalled() {
    let b = Bench::new();
    let p = b.ports();
    let sleep = Advance(b.clock.clone());
    let c = Stubborn::new(&b.clock, None, true);
    let start = b.clock.now_ms();
    assert_eq!(
        kill_child(&p, &env(&sleep, &NoSwitch), &c, &mut || {}),
        Killed::Lingering
    );
    assert_eq!(
        b.clock.now_ms() - start,
        35_000,
        "the deadline, then the grace"
    );
    let gone = Stubborn::new(&b.clock, Some(0), false);
    *crate::lock(&gone.gone_at) = Some(0);
    assert_eq!(
        kill_child(&p, &env(&sleep, &NoSwitch), &gone, &mut || {}),
        Killed::Term
    );
    assert!(crate::lock(&gone.signals).is_empty());
}

#[test]
fn run_args_take_each_flag_once_with_tokens_as_unit_names_carry_them() {
    let a = parse_args(&[
        "--repo",
        "r",
        "--batch",
        "b1",
        "--lease-token",
        "7-0",
        "--slot",
        "ec2/2",
        "--slot-token",
        "9-1",
    ])
    .unwrap();
    assert_eq!(
        a,
        RunArgs {
            repo: "r".into(),
            batch: "b1".into(),
            lease: Rev::new(7, 0),
            slot: Slot {
                pool: Pool::Ec2,
                n: 2
            },
            slot_token: Rev::new(9, 1),
        }
    );
    assert_eq!(
        token_of_word(&token_word(Rev::new(41, 3))),
        Some(Rev::new(41, 3))
    );
    for (bad, said) in [
        (vec!["--repo", "r"], "--batch is required"),
        (vec!["--repo"], "--repo needs a value"),
        (vec!["--repo", "r", "--repo", "s"], "--repo is given twice"),
        (vec!["--wat", "x"], "unknown argument \"--wat\""),
        (
            vec![
                "--repo",
                "r",
                "--batch",
                "b",
                "--lease-token",
                "7",
                "--slot",
                "proxmox/0",
                "--slot-token",
                "1-0",
            ],
            "--lease-token \"7\" is not <main>-<sub>",
        ),
        (
            vec![
                "--repo",
                "r",
                "--batch",
                "b",
                "--lease-token",
                "7-0",
                "--slot",
                "gcp/0",
                "--slot-token",
                "1-0",
            ],
            "--slot \"gcp/0\" is not <pool>/<n>",
        ),
    ] {
        assert_eq!(parse_args(&bad).unwrap_err(), said);
    }
}

#[test]
fn every_end_has_its_own_exit_and_help_lists_each() {
    let ends = [
        RunEnd::Judged(Judged::Recorded {
            n: 1,
            class: Class::Green,
            ejected: vec![],
        }),
        RunEnd::Judged(Judged::Held {
            n: 1,
            ejected: vec![],
            blocker: judge::Blocker::EjectRefused {
                member: "a".into(),
                suites: vec![],
                text: String::new(),
            },
        }),
        RunEnd::Judged(Judged::Unfinished {
            text: String::new(),
        }),
        RunEnd::Judged(Judged::Preempted),
        RunEnd::Stopped { why: String::new() },
        RunEnd::LeaseLost {
            reason: Lost::Expired,
            gone: true,
        },
        RunEnd::LeaseLost {
            reason: Lost::Deadline,
            gone: true,
        },
        RunEnd::LeaseLost {
            reason: Lost::Superseded,
            gone: false,
        },
        RunEnd::Refused { why: String::new() },
    ];
    let codes: Vec<i32> = ends.iter().map(RunEnd::code).collect();
    assert_eq!(codes, [0, 3, 4, 5, 6, 7, 8, 9, 10]);
    for c in codes {
        assert!(
            EXITS.contains(&format!("\n  {c:<2} ")),
            "exit {c} is not in --help"
        );
    }
    assert_eq!(ends[5].word(), "lease-lost");
}

fn end(exit: i32, stdout: &str) -> CertifyEnd {
    CertifyEnd {
        exit,
        fields: fields(stdout),
        output: "out".into(),
    }
}

fn phases(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

/// The Judge reads these words; the runner must emit exactly them.
#[test]
fn a_red_that_stopped_in_vm_setup_is_image_setup_and_in_the_container_build_is_install() {
    let r = pass_result(&end(1, "red=testenv\n"), &phases(&["vm", "done"]));
    assert_eq!(r.phase.as_deref(), Some(judge::IMAGE_SETUP));
    let r = pass_result(
        &end(1, "red=testenv\n"),
        &phases(&["vm", "build", "salvage", "done"]),
    );
    assert_eq!(r.phase.as_deref(), Some(judge::INSTALL));
    let r = pass_result(
        &end(1, "red=testenv\n"),
        &phases(&["vm", "build", "suites", "done"]),
    );
    assert_eq!(r.phase, None, "a red in the suites names the tree");
    let r = pass_result(&end(0, ""), &phases(&["vm"]));
    assert_eq!(r.phase, None, "only a red has a setup phase");
    assert_eq!(
        pass_result(&end(1, "red=a\n"), &[]).phase,
        None,
        "no marks, no phase"
    );
}

#[test]
fn certify_s_fault_reason_is_the_judge_s_word_and_over_cap_twice_is_over_cap() {
    let r = pass_result(&end(4, "fault=over-cap unreached=3\n"), &[]);
    assert_eq!(r.reason.as_deref(), Some(judge::OVER_CAP));
    let r = pass_result(&end(4, "fault=capacity unreached=3,4\n"), &[]);
    assert_eq!(r.reason.as_deref(), Some(judge::OVER_CAP));
    let r = pass_result(&end(4, "fault=fences-cap\n"), &[]);
    assert_eq!(r.reason.as_deref(), Some(judge::FENCES_CAP));
    assert_eq!(pass_result(&end(4, ""), &[]).reason, None);
    let typed = |stdout: &str| judge::type_pass(&pass_result(&end(4, stdout), &[]));
    assert_eq!(
        typed("fault=fences-cap\n"),
        judge::Typed::Fault(crate::ports::Cause::FencesCap)
    );
    assert_eq!(
        typed("fault=capacity\n"),
        judge::Typed::Fault(crate::ports::Cause::OverCap)
    );
}

#[test]
fn a_red_names_its_steps_carries_its_output_and_one_naming_none_is_no_verdict() {
    let r = pass_result(&end(1, "red=test-a.sh,spira-lint\n"), &[]);
    assert_eq!(
        (r.exit, r.red.clone(), r.output.as_str()),
        (1, phases(&["test-a.sh", "spira-lint"]), "out")
    );
    let none = pass_result(&end(1, "red=,\n"), &[]);
    assert!(none.red.is_empty());
    assert_eq!(
        judge::type_pass(&none),
        judge::Typed::Fault(crate::ports::Cause::Vm)
    );
}

#[test]
fn round_vm_phases_run_under_their_deadlines() {
    assert_eq!(phase_of("vm"), Phase::Boot);
    assert_eq!(phase_of("build"), Phase::Build);
    assert_eq!(phase_of("unit-build"), Phase::Build);
    assert_eq!(phase_of("suites"), Phase::Suites);
    assert_eq!(phase_of("unit-tests"), Phase::Suites);
    assert_eq!(phase_of("done"), Phase::Certify);
}

#[test]
fn the_run_dir_s_switches_are_the_halt_stamp_then_a_scoped_stop_on_rounds() {
    let d = testkit::TempDir::new("rc-run-switches");
    let s = RunDir(d.path().to_path_buf());
    assert_eq!(s.set("home"), None);
    spira_config::scoped_stop::pause_repo(
        d.path(),
        "home",
        &["claims".into()],
        "ops",
        "drill",
        "manual",
        1,
    )
    .unwrap();
    assert_eq!(
        s.set("home"),
        None,
        "a stop on another lane is not this one"
    );
    spira_config::scoped_stop::pause_repo(
        d.path(),
        "home",
        &[ROUNDS_LANE.into()],
        "ops",
        "drill",
        "manual",
        1,
    )
    .unwrap();
    assert_eq!(
        s.set("home").unwrap(),
        "home's rounds lane is stopped by ops: drill"
    );
    assert_eq!(s.set("other"), None);
    std::fs::write(d.path().join(HALTED_FILE), "").unwrap();
    assert!(s.set("other").unwrap().contains("world.halted"));
}

fn seeded(b: &Bench) {
    b.seed(
        "lanes/r",
        json!({ "landing_ref": "base-0", "image_digest": "img", "harness_version": "h" }),
    );
    b.seed(
        "batches/r/b1",
        json!({ "batch_id": "b1", "repo": "r", "state": "CiRunning", "head": "h1", "base": "base-0",
                "pass": 1, "version": 1, "tested_base": "base-0", "members": [{ "id": "a", "tip": "a1" }] }),
    );
}

fn handed(b: &Bench) -> RunArgs {
    let p = b.ports();
    let l = acquire(&p, &Resource::round("r", "b1"), "pass").unwrap();
    let s = grant(&p, &[Pool::Proxmox], "pass").unwrap();
    RunArgs {
        repo: "r".into(),
        batch: "b1".into(),
        lease: l.token(),
        slot: s.slot(),
        slot_token: s.token(),
    }
}

#[test]
fn a_lost_lease_refuses_every_write_through_the_fenced_store_and_ledger() {
    let b = Bench::new();
    seeded(&b);
    let a = handed(&b);
    let p = b.ports();
    let lease = Lease::handed(Resource::round("r", "b1"), a.lease);
    let lost = Cell::new(None);
    let f = Fenced {
        inner: p.store,
        clock: p.clock,
        fence: lease.fence().clone(),
        lost: &lost,
    };
    let put = |n: u64| {
        Txn::new().put(
            "batches/r/b1/progress",
            state_store::Object::new(json!({ "n": n })),
        )
    };
    let req = |n: &str| state_store::Request::new(n, "t", "k");
    assert!(f.txn(&put(1), &req("w1")).unwrap().succeeded);
    lost.set(Some(Lost::Deadline));
    assert!(
        matches!(f.txn(&put(2), &req("w2")), Err(Error::LockLost { .. })),
        "a runner that stopped renewing writes nothing, though its lock is still live"
    );
    let g = Guarded {
        inner: p.ledger,
        lost: &lost,
    };
    assert!(g.open().unwrap_err().contains("deadline"));
    lost.set(None);
    b.clock.advance(p.leases.ttl_ms + 1);
    acquire(&p, &Resource::round("r", "b1"), "other").unwrap();
    assert!(
        matches!(f.txn(&put(3), &req("w3")), Err(Error::LockLost { .. })),
        "a lease taken by another refuses the old holder's write"
    );
    assert_eq!(
        b.store
            .get("batches/r/b1/progress")
            .unwrap()
            .unwrap()
            .value
            .data["n"],
        1
    );
}

#[test]
fn a_lease_on_a_round_that_is_gone_is_lost_to_it_and_a_round_with_no_lane_is_refused() {
    let b = Bench::new();
    seeded(&b);
    let a = handed(&b);
    let p = b.ports();
    let sleep = Advance(b.clock.clone());
    let mut subs = Idle;
    let missing = RunArgs {
        batch: "b9".into(),
        ..a.clone()
    };
    let e = run(&p, &env(&sleep, &NoSwitch), &mut subs, &missing);
    assert_eq!(
        e,
        RunEnd::LeaseLost {
            reason: Lost::Superseded,
            gone: true
        },
        "no lease is held on a round that is gone"
    );
    assert!(
        crate::slots::in_use(&p)
            .unwrap()
            .iter()
            .all(|(_, n)| *n == 0),
        "the slot went back"
    );
    let b2 = Bench::new();
    b2.seed(
        "batches/r/b1",
        json!({ "batch_id": "b1", "repo": "r", "state": "CiRunning", "pass": 1, "version": 1, "members": [] }),
    );
    let a2 = handed(&b2);
    let e = run(
        &b2.ports(),
        &env(&Advance(b2.clock.clone()), &NoSwitch),
        &mut Idle,
        &a2,
    );
    assert_eq!(
        e,
        RunEnd::Refused {
            why: "no lane record for r".into()
        }
    );
}

#[test]
fn an_off_switch_set_before_the_pass_starts_nothing_and_gives_both_locks_back() {
    let b = Bench::new();
    seeded(&b);
    let a = handed(&b);
    let p = b.ports();
    struct Halted;
    impl Switches for Halted {
        fn set(&self, _: &str) -> Option<String> {
            Some("world.halted is present".into())
        }
    }
    let e = run(&p, &env(&Advance(b.clock.clone()), &Halted), &mut Idle, &a);
    assert_eq!(
        e,
        RunEnd::Stopped {
            why: "world.halted is present".into()
        }
    );
    assert!(b.verbs.calls().is_empty(), "no pass-start, no certify");
    assert!(crate::slots::in_use(&p)
        .unwrap()
        .iter()
        .all(|(_, n)| *n == 0));
    assert_eq!(
        crate::lease::holder_of(&p, &Resource::round("r", "b1")).unwrap(),
        None
    );
}

#[test]
fn a_refused_pass_start_runs_no_pass() {
    let b = Bench::new();
    seeded(&b);
    let a = handed(&b);
    let e = run(
        &b.ports(),
        &env(&Advance(b.clock.clone()), &NoSwitch),
        &mut Idle,
        &a,
    );
    let RunEnd::Refused { why } = e else {
        panic!("{e:?}")
    };
    assert!(why.starts_with("pass-start: unscripted call"), "{why}");
    assert!(b.verbs.calls().iter().all(|c| c.verb == Verb::PassStart));
}

#[test]
fn a_runner_whose_first_renewal_fails_starts_nothing_and_writes_nothing() {
    let b = Bench::unserved();
    let p = b.ports();
    let a = RunArgs {
        repo: "r".into(),
        batch: "b1".into(),
        lease: Rev::new(1, 0),
        slot: Slot {
            pool: Pool::Proxmox,
            n: 0,
        },
        slot_token: Rev::new(2, 0),
    };
    let e = run(
        &p,
        &env(&Advance(b.clock.clone()), &NoSwitch),
        &mut Idle,
        &a,
    );
    assert_eq!(
        e,
        RunEnd::LeaseLost {
            reason: Lost::Expired,
            gone: true
        }
    );
    assert_eq!(e.code(), 7);
    assert!(b.verbs.calls().is_empty(), "no pass-start, no certify");
}

/// A sub-pass runner with no VM that launches nothing.
struct Idle;

impl RoundRunner for Idle {
    fn set_members(&mut self, _: &[RoundMember]) {}
    fn set_base(&mut self, _: &str) {}
    fn start_main(&mut self, _: MainKind, _: &[String]) -> Result<(), String> {
        Err("none".into())
    }
    fn poll_main(&mut self) -> Result<MainPoll, String> {
        Ok(MainPoll::default())
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, _: &Job) -> Result<(), String> {
        Err("no sub-pass VM".into())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        vec![]
    }
    fn now(&self) -> u64 {
        0
    }
    fn wait(&mut self) {}
}

impl SubPasses for Idle {
    fn vm(&self) -> Option<&dyn Child> {
        None
    }
    fn close(&mut self) {}
}

/// A sub-pass VM that is up until signalled, and launches into nothing.
struct Up<'c> {
    vm: &'c Stubborn,
    launched: usize,
    clock: VirtualClock,
    /// The round's record goes at the first wait: the queue superseded it.
    supersede: Option<&'c Bench>,
}

impl RoundRunner for Up<'_> {
    fn set_members(&mut self, _: &[RoundMember]) {}
    fn set_base(&mut self, _: &str) {}
    fn start_main(&mut self, _: MainKind, _: &[String]) -> Result<(), String> {
        Err("none".into())
    }
    fn poll_main(&mut self) -> Result<MainPoll, String> {
        Ok(MainPoll::default())
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, _: &Job) -> Result<(), String> {
        self.launched += 1;
        Ok(())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        vec![]
    }
    fn now(&self) -> u64 {
        self.clock.now_ms() / 1000
    }
    fn wait(&mut self) {
        if let Some(b) = self.supersede.take() {
            let t = Txn::new().delete("batches/r/b1");
            b.store
                .txn(&t, &state_store::Request::new("supersede", "queue", "k"))
                .unwrap();
        }
        self.clock.advance(1_000);
    }
}

impl SubPasses for Up<'_> {
    fn vm(&self) -> Option<&dyn Child> {
        Some(self.vm)
    }
    fn close(&mut self) {}
}

#[test]
fn a_sub_pass_that_stalls_past_its_deadline_kills_its_vm_records_nothing_and_frees_the_slot_after()
{
    let b = Bench::new();
    seeded(&b);
    let a = handed(&b);
    let p = b.ports();
    b.seed(
        "batches/r/b1/progress",
        json!({ "pass": 1, "runs": [], "main": { "exit": 1, "red": ["test-a.sh"] } }),
    );
    let vm = Stubborn::new(&b.clock, Some(7_000), false);
    let mut subs = Up {
        vm: &vm,
        launched: 0,
        clock: b.clock.clone(),
        supersede: None,
    };
    let start = b.clock.now_ms();
    let e = run(
        &p,
        &env(&Advance(b.clock.clone()), &NoSwitch),
        &mut subs,
        &a,
    );
    assert_eq!(
        e,
        RunEnd::LeaseLost {
            reason: Lost::Deadline,
            gone: true
        }
    );
    assert_eq!(subs.launched, 1, "the rerun, then nothing more");
    let signals = crate::lock(&vm.signals).clone();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].1, Signal::Term);
    let subpass = p.leases.deadline_ms(Phase::SubPass);
    assert!(signals[0].0 - start >= subpass, "{signals:?}");
    assert!(b.ledger.attempts().is_empty(), "no attempt");
    let progress = b
        .store
        .get("batches/r/b1/progress")
        .unwrap()
        .unwrap()
        .value
        .data;
    assert_eq!(
        progress["runs"],
        json!([]),
        "the faulted rerun was not recorded"
    );
    assert!(crate::slots::in_use(&p)
        .unwrap()
        .iter()
        .all(|(_, n)| *n == 0));
    assert!(
        b.verbs.calls().is_empty(),
        "a resumed pass is not started again"
    );
}

#[test]
fn a_round_superseded_under_a_live_lease_is_seen_at_the_next_renewal_and_its_vm_goes_first() {
    let b = Bench::new();
    seeded(&b);
    let a = handed(&b);
    let p = b.ports();
    b.seed(
        "batches/r/b1/progress",
        json!({ "pass": 1, "runs": [], "main": { "exit": 1, "red": ["test-a.sh"] } }),
    );
    let vm = Stubborn::new(&b.clock, Some(3_000), false);
    let mut subs = Up {
        vm: &vm,
        launched: 0,
        clock: b.clock.clone(),
        supersede: Some(&b),
    };
    let start = b.clock.now_ms();
    let e = run(
        &p,
        &env(&Advance(b.clock.clone()), &NoSwitch),
        &mut subs,
        &a,
    );
    assert_eq!(
        e,
        RunEnd::LeaseLost {
            reason: Lost::Superseded,
            gone: true
        }
    );
    let term = crate::lock(&vm.signals)[0].0;
    assert!(
        term - start <= p.leases.renew_every_ms(),
        "seen within one renewal interval, though the lease itself still holds"
    );
    assert!(crate::lease::holder_of(&p, &Resource::round("r", "b1"))
        .unwrap()
        .is_none());
    assert!(b.ledger.attempts().is_empty());
}

#[test]
fn the_slot_is_kept_while_a_child_tears_down() {
    let b = Bench::new();
    let p = b.ports();
    let sleep = Advance(b.clock.clone());
    let c = Stubborn::new(&b.clock, Some(12_000), false);
    let mut kept = 0;
    kill_child(&p, &env(&sleep, &NoSwitch), &c, &mut || kept += 1);
    assert_eq!(kept, 12, "every poll of the teardown keeps the slot");
}

#[test]
fn only_an_answered_pass_is_a_verdict() {
    for (exit, out, yes) in [
        (0, "", true),
        (1, "red=test-a.sh\n", true),
        (1, "red=,\n", false),
        (1, "", false),
        (2, "", false),
        (judge::FAULT, "fault=over-cap\n", true),
        (judge::PREEMPTED, "", true),
        (judge::VM_LOST, "", true),
        (101, "", false),
    ] {
        assert_eq!(is_verdict(&end(exit, out)), yes, "exit {exit} {out:?}");
    }
}
