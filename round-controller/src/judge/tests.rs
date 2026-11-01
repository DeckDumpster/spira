use super::*;
use crate::sim::run_all;

const HEAD: &str = "description = \"t\"\nrepos = [\"lane\"]\n[run]\nepoch = 1800000000\npasses = 2\nevery_secs = 600\n[judge]\nrepo = \"lane\"\nbatch = \"r1\"\nbase = \"base-1\"\nimage_digest = \"img-2\"\n";

fn expect(name: &str, count: &str, equals: u64) -> String {
    format!("[[expect]]\nname = \"{name}\"\ncount = \"{count}\"\nequals = {equals}\n")
}

fn job(removal: &str, results: &str) -> String {
    format!("[[judge.job]]\nremoval = [{removal}]\nresults = [{results}]\nsecs = 5\n")
}

fn member(id: &str, extra: &str) -> String {
    format!("[[judge.member]]\nid = \"{id}\"\ntip = \"{id}-1\"\n{extra}")
}

const DRIVE: &str = "[[judge.drive]]\npass = 1\n";

fn result(exit: i32, extra: &str) -> String {
    format!("[judge.result]\nexit = {exit}\nred = [\"test-x.sh\"]\n{extra}")
}

fn go(parts: &[String]) -> Result<(), String> {
    run_all(&format!("{HEAD}{}{DRIVE}", parts.concat()), 1).map(|_| ())
}

fn pass(exit: i32, red: &[&str], reason: Option<&str>) -> PassResult {
    PassResult {
        exit,
        red: red.iter().map(|s| s.to_string()).collect(),
        reason: reason.map(str::to_string),
        ..PassResult::default()
    }
}

#[test]
fn a_pass_is_typed_from_certify_s_exit_its_reason_and_its_red_steps() {
    assert_eq!(type_pass(&pass(PREEMPTED, &["a"], None)), Typed::Preempted);
    assert_eq!(
        type_pass(&pass(VM_LOST, &["a"], None)),
        Typed::Fault(Cause::Vm)
    );
    assert_eq!(
        type_pass(&pass(FAULT, &[], Some(OVER_CAP))),
        Typed::Fault(Cause::OverCap)
    );
    assert_eq!(
        type_pass(&pass(FAULT, &[], Some(FENCES_CAP))),
        Typed::Fault(Cause::FencesCap)
    );
    assert_eq!(
        type_pass(&pass(FAULT, &["a"], Some("wall"))),
        Typed::Fault(Cause::Vm)
    );
    assert_eq!(
        type_pass(&pass(1, &["testenv"], None)),
        Typed::Red {
            steps: vec!["testenv".into()]
        }
    );
    assert_eq!(type_pass(&pass(0, &[], None)), Typed::Green);
    assert_eq!(
        type_pass(&pass(1, &[], None)),
        Typed::Fault(Cause::Vm),
        "a non-zero exit naming no red step is no verdict, never green"
    );
}

/// A round of `members`, each having changed `src/<id>.rs`.
fn round(members: &[&str]) -> Round {
    let ms: String = members
        .iter()
        .map(|m| format!("[[member]]\nid = \"{m}\"\ntip = \"{m}-1\"\nchanged = [\"src/{m}.rs\"]\n"))
        .collect();
    let text = format!("repo = \"lane\"\nbatch = \"r1\"\nbase = \"base-1\"\nimage_digest = \"img-2\"\ndrive = []\n{ms}[result]\nexit = 1\n");
    let spec: crate::sim::judge::JudgeSpec = toml::from_str(&text).unwrap();
    spec.round(&crate::sim::profiles().unwrap()[0])
}

#[test]
fn a_lane_fault_is_image_on_image_setup_or_a_base_green_on_another_digest_install_on_install_else_base(
) {
    let r = round(&["sp-a"]);
    let phase = |p: Option<&str>| PassResult {
        phase: p.map(str::to_string),
        ..PassResult::default()
    };
    let image = Cause::Image {
        digest: "img-2".into(),
    };
    assert_eq!(type_lane_fault(&r, &phase(Some(IMAGE_SETUP)), &[]), image);
    assert_eq!(type_lane_fault(&r, &phase(None), &["img-1".into()]), image);
    assert_eq!(
        type_lane_fault(&r, &phase(Some(INSTALL)), &["img-2".into()]),
        Cause::Install
    );
    assert_eq!(
        type_lane_fault(&r, &phase(None), &["img-2".into()]),
        Cause::Base
    );
}

#[test]
fn a_member_reaches_the_failing_code_only_when_the_output_names_one_of_its_paths() {
    let r = round(&["sp-a", "sp-b"]);
    assert_eq!(reaching("panicked at src/sp-a.rs:3", &r), ["sp-a"]);
    assert!(reaching("ensurepip is not available", &r).is_empty());
    assert_eq!(r.identity(&["sp-a".into()]).members.len(), 1);
    assert_eq!(Judged::Preempted.word(), "preempted");
    assert_eq!(
        (result_of("green"), result_of("red"), result_of("??")),
        (JobResult::Green, JobResult::Red, JobResult::Fault)
    );
}

#[test]
fn a_green_or_faulted_pass_is_one_attempt_with_no_sub_pass_and_a_preempted_one_is_none() {
    for (exit, red, class) in [(0, "[]", "green"), (VM_LOST, "[\"a\"]", "fault")] {
        let r = format!("[judge.result]\nexit = {exit}\nred = {red}\n");
        go(&[
            member("sp-a", ""),
            r,
            expect("one", &format!("attempts:{class}"), 1),
            expect("no-run", "launches", 0),
        ])
        .unwrap();
    }
    go(&[
        member("sp-a", ""),
        result(PREEMPTED, ""),
        expect("none", "attempts", 0),
        expect("p", "judged:preempted", 1),
    ])
    .unwrap();
}

#[test]
fn a_sub_pass_that_faults_twice_reads_as_neither_and_the_red_blames_no_one() {
    // The rerun faults twice.
    go(&[
        member("sp-a", ""),
        result(1, ""),
        job("", "\"fault\", \"fault\""),
        expect("red", "attempts:red", 1),
        expect("no-eject", "calls:eject", 0),
    ])
    .unwrap();
    // The first base run faults twice; then the second base run, after a red first one.
    for bases in ["\"fault\", \"fault\"", "\"red\", \"fault\", \"fault\""] {
        go(&[
            member("sp-a", ""),
            result(1, ""),
            job("", "\"red\""),
            job("\"sp-a\"", bases),
            expect("red", "attempts:red", 1),
            expect("no-lane-fault", "lane-faults", 0),
            expect("no-eject", "calls:eject", 0),
        ])
        .unwrap();
    }
}

const EJECT_OK: &str = "[[verb]]\nverb = \"eject\"\nok = { head = \"h2\" }\n";

#[test]
fn a_single_member_red_on_its_tree_and_green_on_the_base_is_ejected_and_marked_proven() {
    go(&[
        member("sp-a", ""),
        result(1, ""),
        job("", "\"red\""),
        job("\"sp-a\"", "\"green\""),
        EJECT_OK.into(),
        expect("ejected", "outcomes:ejected", 1),
        expect("proven", "proven", 1),
        expect("recorded", "judged:recorded", 1),
        expect("no-judge-pass", "judge-passes", 0),
    ])
    .unwrap();
}

#[test]
fn an_owner_leaves_with_its_stacked_dependent_and_only_the_owner_is_proven() {
    go(&[
        member("sp-a", ""),
        member("sp-b", "stack = [\"sp-a\"]\n"),
        member("sp-c", ""),
        result(1, ""),
        job("", "\"red\""),
        job("\"sp-a\", \"sp-b\", \"sp-c\"", "\"green\""),
        job("\"sp-a\", \"sp-b\"", "\"green\""),
        job("\"sp-b\"", "\"red\""),
        job("\"sp-c\"", "\"red\""),
        EJECT_OK.into(),
        EJECT_OK.into(),
        expect("two-ejects", "outcomes:ejected", 2),
        expect("one-proven", "proven", 1),
    ])
    .unwrap();
}

#[test]
fn with_no_removal_green_the_one_member_whose_diff_reaches_the_failing_code_is_ejected() {
    let base = [
        member("sp-a", "changed = [\"src/a.rs\"]\n"),
        member("sp-b", "changed = [\"src/b.rs\"]\n"),
        job("", "\"red\""),
        job("\"sp-a\", \"sp-b\"", "\"green\""),
        job("\"sp-a\"", "\"red\""),
        job("\"sp-b\"", "\"red\""),
    ]
    .concat();
    go(&[
        base.clone(),
        result(1, "output = \"failed at src/a.rs:3\"\n"),
        EJECT_OK.into(),
        expect("ejected", "outcomes:ejected", 1),
        expect("judge-passes", "judge-passes", 2),
        expect("not-a-planner-repeat", "planner-repeats", 0),
    ])
    .unwrap();
    // Both diffs reach it: nothing points to one member, so nobody is ejected.
    go(&[
        base,
        result(1, "output = \"src/a.rs and src/b.rs\"\n"),
        expect("none", "calls:eject", 0),
        // The round's red and its two single-member removal sets, each its own identity.
        expect("red", "attempts:red", 3),
    ])
    .unwrap();
}

#[test]
fn a_removal_set_whose_identity_holds_a_verdict_is_reused_and_not_run() {
    go(&[
        member("sp-a", ""),
        member("sp-b", ""),
        result(1, ""),
        job("", "\"red\""),
        job("\"sp-a\", \"sp-b\"", "\"green\""),
        "[[judge.held]]\nremoval = [\"sp-a\"]\nclass = \"green\"\n[[judge.held]]\nremoval = [\"sp-b\"]\nclass = \"red\"\n".into(),
        EJECT_OK.into(),
        expect("no-removal-run", "launches:without", 0),
        expect("ejected", "outcomes:ejected", 1),
    ])
    .unwrap();
}

#[test]
fn a_removal_set_that_faults_is_recorded_a_fault_and_retried_once() {
    go(&[
        member("sp-a", ""),
        member("sp-b", ""),
        result(1, ""),
        job("", "\"red\""),
        job("\"sp-a\", \"sp-b\"", "\"green\""),
        job("\"sp-a\"", "\"fault\", \"fault\""),
        job("\"sp-b\"", "\"fault\", \"fault\""),
        expect("faults", "attempts:fault", 4),
        expect("red", "attempts:red", 1),
        expect("none", "calls:eject", 0),
    ])
    .unwrap();
}

#[test]
fn the_judge_s_counts_and_its_table_are_checked_by_name() {
    for bad in [
        "attempts:blue",
        "lane-faults:vm",
        "judged:x",
        "launches:x",
        "lane-faults-naming:",
    ] {
        let e = crate::sim::parse(&format!(
            "{HEAD}{}{}{DRIVE}{}",
            member("sp-a", ""),
            result(1, ""),
            expect("n", bad, 0)
        ))
        .unwrap_err();
        assert!(e.contains("which is nothing the harness counts"), "{e}");
    }
    let e = crate::sim::parse(&format!(
        "{HEAD}{}{}{}{DRIVE}",
        member("sp-a", ""),
        result(1, ""),
        job("\"sp-z\"", "\"red\"")
    ))
    .unwrap_err();
    assert!(e.contains("removes sp-z, which is not a member"), "{e}");
    let e = crate::sim::parse(&format!(
        "{HEAD}{}{}{}{DRIVE}",
        member("sp-a", ""),
        result(1, ""),
        job("", "\"blue\"")
    ))
    .unwrap_err();
    assert!(e.contains("\"blue\" is not green, red or fault"), "{e}");
    let e = crate::sim::parse(&format!(
        "{HEAD}{}{}[[judge.held]]\nremoval = []\nclass = \"flaky\"\n{DRIVE}",
        member("sp-a", ""),
        result(1, "")
    ))
    .unwrap_err();
    assert!(e.contains("class \"flaky\" is not green or red"), "{e}");
}

#[test]
fn an_unscripted_sub_pass_breaks_the_harness_invariant() {
    let e = go(&[member("sp-a", ""), result(1, "")]).unwrap_err();
    assert!(
        e.contains(
            "invariant no-unscripted-fault violated under default: unscripted sub-pass without []"
        ),
        "{e}"
    );
}

/// A runner that answers every job green at once.
struct Instant(Vec<u64>);

impl RoundRunner for Instant {
    fn set_members(&mut self, _: &[RoundMember]) {}
    fn set_base(&mut self, _: &str) {}
    fn start_main(&mut self, _: batcher::drive::MainKind, _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn poll_main(&mut self) -> Result<batcher::drive::MainPoll, String> {
        Ok(batcher::drive::MainPoll::default())
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, job: &Job) -> Result<(), String> {
        self.0.push(job.id);
        Ok(())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        self.0.drain(..).map(|id| (id, JobResult::Green)).collect()
    }
    fn now(&self) -> u64 {
        0
    }
    fn wait(&mut self) {}
}

struct World {
    clock: runtime::ports::VirtualClock,
    store: state_store::Contract<state_store::MemBackend>,
    ledger: crate::sim::judge::FakeLedger,
    verbs: crate::sim::FakeVerbs,
    rng: std::sync::Mutex<runtime::ports::SeededRng>,
    log: crate::outcome::MemLog,
    units: crate::sim::bench::FakeUnits,
    vms: crate::sim::bench::FakeVms,
    launch: crate::sim::bench::FakeLaunch,
    leases: crate::liveness::LeaseSettings,
    slots: crate::slots::SlotSettings,
}

fn world() -> World {
    let clock = runtime::ports::VirtualClock::new(1_800_000_000_000);
    let p = crate::sim::profiles().unwrap().remove(0);
    let vm = crate::sim::FakeVm::new(vec![], p, clock.clone());
    World {
        store: state_store::Contract::open(
            state_store::MemBackend::new(),
            state_store::Ports::virtual_clock(&clock),
            state_store::Config {
                min_lock_ttl_ms: 1000,
            },
        )
        .unwrap(),
        ledger: crate::sim::judge::FakeLedger::default(),
        verbs: crate::sim::FakeVerbs::new(vec![], vm, clock.clone()).unwrap(),
        rng: std::sync::Mutex::new(runtime::ports::SeededRng::new(1)),
        log: crate::outcome::MemLog::default(),
        units: crate::sim::bench::FakeUnits::new(clock.clone()),
        vms: crate::sim::bench::FakeVms::new(clock.clone()),
        launch: crate::sim::bench::FakeLaunch::default(),
        leases: crate::sim::bench::bench_settings(),
        slots: crate::sim::bench::bench_slot_settings(),
        clock,
    }
}

fn ports<'a>(
    w: &'a World,
    store: &'a dyn crate::ports::Store,
    ledger: &'a dyn crate::ports::Ledger,
) -> Ports<'a> {
    Ports {
        clock: &w.clock,
        proc_: &runtime::ports::SysProc,
        rng: &w.rng,
        store,
        verbs: &w.verbs,
        ledger,
        log: &w.log,
        units: &w.units,
        vms: &w.vms,
        launcher: &w.launch,
        git: &crate::sim::git::NO_GIT,
        leases: &w.leases,
        slots: &w.slots,
        lc: &crate::real::Unserved,
        stops: &crate::switches::NoStops,
    }
}

#[test]
fn a_store_or_ledger_that_refuses_leaves_the_judgement_unfinished() {
    let w = world();
    let r = round(&["sp-a"]);
    let red = pass(1, &["t"], None);
    let unserved = crate::real::Unserved;
    let p = ports(&w, &unserved, &w.ledger);
    let j = judge(&p, &mut Instant(vec![]), &r, &red);
    assert!(
        matches!(&j, Judged::Unfinished { text } if text.starts_with("progress:")),
        "{j:?}"
    );
    let p = ports(&w, &w.store, &unserved);
    let j = judge(&p, &mut Instant(vec![]), &r, &pass(0, &[], None));
    assert!(
        matches!(&j, Judged::Unfinished { text } if text.contains("verdict ledger")),
        "{j:?}"
    );
}

/// A runner that refuses every launch.
struct Dead;

impl RoundRunner for Dead {
    fn set_members(&mut self, _: &[RoundMember]) {}
    fn set_base(&mut self, _: &str) {}
    fn start_main(&mut self, _: batcher::drive::MainKind, _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn poll_main(&mut self) -> Result<batcher::drive::MainPoll, String> {
        Ok(batcher::drive::MainPoll::default())
    }
    fn stop_main(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn launch(&mut self, _: &Job) -> Result<(), String> {
        Err("no slot".into())
    }
    fn poll_jobs(&mut self) -> Vec<(u64, JobResult)> {
        vec![]
    }
    fn now(&self) -> u64 {
        0
    }
    fn wait(&mut self) {}
}

#[test]
fn progress_of_another_pass_is_not_resumed_and_a_judgement_clears_its_progress() {
    let w = world();
    let r = round(&["sp-a"]);
    let stale = Progress {
        pass: 9,
        runs: vec![Ran {
            removal: vec![],
            result: "green".into(),
        }],
        ..Progress::default()
    };
    w.store
        .create(
            &r.progress_key(),
            Object::new(serde_json::to_value(&stale).unwrap()),
            &Request::new("seed", "t", "t"),
        )
        .unwrap();
    let p = ports(&w, &w.store, &w.ledger);
    let j = judge(&p, &mut Dead, &r, &pass(1, &["t"], None));
    assert!(
        matches!(&j, Judged::Unfinished { text } if text.contains("did not launch: no slot")),
        "a stale green rerun is not taken: {j:?}"
    );
    let j = judge(&p, &mut Instant(vec![]), &r, &pass(1, &["t"], None));
    assert_eq!(
        j,
        Judged::Recorded {
            n: 1,
            class: Class::Flaky,
            ejected: vec![]
        }
    );
    assert!(
        w.store.get(&r.progress_key()).unwrap().is_none(),
        "progress cleared"
    );
    let bad = Object::new(serde_json::json!("not progress"));
    w.store
        .create(&r.progress_key(), bad, &Request::new("seed2", "t", "t"))
        .unwrap();
    let j = judge(&p, &mut Instant(vec![]), &r, &pass(1, &["t"], None));
    assert!(
        matches!(&j, Judged::Unfinished { text } if text.starts_with("progress:")),
        "{j:?}"
    );
}

#[test]
fn a_held_round_re_driven_once_the_eject_is_accepted_records_nothing_again_and_ejects_once() {
    go(&[
        member("sp-a", ""),
        result(1, ""),
        job("", "\"red\""),
        job("\"sp-a\"", "\"green\""),
        "[[verb]]\nverb = \"eject\"\nrefuse = { code = 1, text = \"no\" }\n".into(),
        EJECT_OK.into(),
        "[[judge.drive]]\npass = 2\n".into(),
        expect("held-then-recorded", "judged:held", 1),
        expect("recorded", "judged:recorded", 1),
        expect("one-attempt", "attempts", 1),
        expect("each-sub-pass-once", "launches", 2),
        expect("one-ejected-line", "outcomes:ejected", 1),
        expect("proven-once", "proven", 1),
    ])
    .unwrap();
}

#[test]
fn a_judge_count_in_a_scenario_with_no_judge_is_refused_by_name() {
    let e = crate::sim::parse(&format!(
        "description = \"t\"\nrepos = [\"a\"]\n[run]\nepoch = 1\npasses = 1\nevery_secs = 1\n{}",
        expect("n", "attempts", 0)
    ))
    .unwrap_err();
    assert!(e.contains("which only a [judge] table runs"), "{e}");
}
