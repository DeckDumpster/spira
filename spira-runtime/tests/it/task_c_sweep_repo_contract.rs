//! `sweep.repo_contract`: a repo that breaks the landing contract is stopped on its claims lane,
//! and the stop is lifted by the next tick that finds the contract met.

use std::path::Path;

use runtime::event::EventKind;
use runtime::ports::{Proc, ProcOutput, ProcSpec};
use runtime::taskrig::{TaskRig, DAY_MS};
use spira_config::scoped_stop;
use spira_runtime::tasks::b::kit::{names, testing as t, Input, Stub, FIRE};
use spira_runtime::tasks::c::sweep_repo_contract::{self as task, Carry, Snapshot, SweepRepoContract as H};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "*-*-* 05:10:00"), (task::JITTER_KEY, "600")];
const NOW: u64 = 40 * DAY_MS;

const NOT_MET: &str = "not met environment: flake.nix is not in the tree";

fn repo(name: &str, verdict: task::Verdict, lines: &[&str]) -> task::RepoState {
    task::RepoState { repo: name.into(), verdict, lines: lines.iter().map(|l| l.to_string()).collect() }
}

fn snap(repos: Vec<task::RepoState>, stops: &[(&str, &str)]) -> Snapshot {
    Snapshot { repos, stops: stops.iter().map(|(r, o)| task::StopView { repo: r.to_string(), owner: o.to_string(), reason: NOT_MET.into() }).collect() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

/// A rig whose observe answers `s` and whose stop and lift effects run for real against `run`.
fn rig(s: &Snapshot, run: &Path) -> TaskRig {
    let (world, a, b) = (json(s), run.to_path_buf(), run.to_path_buf());
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration())
        .unwrap()
        .double(task::OBSERVE, move |_| Ok(world.clone()))
        .double(task::STOP, move |i| task::stop(&a, i))
        .double(task::LIFT, move |i| task::lift(&b, i))
        .double(task::INBOX_APPEND, |_| Ok("appended".into()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r.clock.advance_to(NOW);
    r.tick(task::STREAM, EventKind::new(task::TICK, FIRE)).unwrap();
    r
}

#[test]
fn t1_registered_boots_and_arms_the_next_daily_tick() {
    t::t1_registered_and_boots::<H>(SET, 5 * 3_600_000 + 10 * 60_000);
}

#[test]
fn a_task_row_and_a_calendar_key_exist_for_it() {
    assert!(spira_runtime::tasks::all().any(|t| t.name == task::NAME), "sweep.repo_contract is not in the task registry");
    let row = spira_runtime::tasks::all().find(|t| t.name == task::NAME).unwrap();
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == task::CADENCE_KEY), "the calendar key is not one the row declares");
    assert!(<H as Stub>::from_cfg(&|_| Err("unset".into())).is_err(), "an unset calendar must refuse the build");
    assert!(<H as Stub>::from_cfg(&|k| Ok(if k == task::CADENCE_KEY { String::new() } else { "600".into() })).is_err(), "an empty calendar must refuse the build");
}

#[test]
fn a_not_met_repo_is_stopped_with_owner_reason_and_until() {
    let d = testkit::TempDir::new("repo-contract-stop");
    let r = rig(&snap(vec![repo("shop", task::Verdict::NotMet, &[NOT_MET])], &[]), d.path());
    let s = scoped_stop::stop_for(d.path(), "shop", "claims").expect("claims stopped");
    assert_eq!((s.owner.as_str(), s.reason.as_str(), s.until.as_str()), ("repo-contract", NOT_MET, "repo-contract check shop met"));
    assert_eq!(s.lanes, ["claims"]);
    let inbox: Vec<_> = r.requested().iter().filter(|q| q.name == task::INBOX_APPEND && q.at >= NOW).collect();
    assert_eq!(inbox.len(), 1);
    assert!(inbox[0].input.contains("shop") && inbox[0].input.contains("environment"), "{}", inbox[0].input);
}

#[test]
fn a_met_repo_with_its_own_stop_is_lifted() {
    let d = testkit::TempDir::new("repo-contract-lift");
    scoped_stop::pause_repo(d.path(), "shop", &["claims".into()], "repo-contract", NOT_MET, "repo-contract check shop met", 1).unwrap();
    let r = rig(&snap(vec![repo("shop", task::Verdict::Met, &[])], &[("shop", "repo-contract")]), d.path());
    assert!(scoped_stop::stop_for(d.path(), "shop", "claims").is_none());
    assert_eq!(r.requested().iter().filter(|q| q.name == task::INBOX_APPEND && q.at >= NOW).count(), 1);
}

#[test]
fn a_stop_owned_by_anyone_else_is_untouched() {
    let d = testkit::TempDir::new("repo-contract-foreign");
    let theirs = scoped_stop::pause_repo(d.path(), "shop", &["claims".into()], "operator", "freeze", "operator says", 1).unwrap();
    for verdict in [task::Verdict::NotMet, task::Verdict::Met] {
        let r = rig(&snap(vec![repo("shop", verdict, &[NOT_MET])], &[("shop", "operator")]), d.path());
        assert_eq!(scoped_stop::stop_for(d.path(), "shop", "claims"), Some(theirs.clone()));
        assert!(r.requested().iter().all(|q| q.name != task::STOP && q.name != task::LIFT), "{:?}", r.requested());
    }
}

#[test]
fn a_cannot_tell_repo_is_neither_stopped_nor_lifted_and_says_so_once() {
    let h = t::handler::<H>(SET);
    let line = "cannot tell submit-checks: work checks did not run";
    let unsure = snap(vec![repo("shop", task::Verdict::CannotTell, &[line])], &[]);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(unsure.clone()), NOW).unwrap();
    assert_eq!(names(&acts), [task::INBOX_APPEND]);
    let (_, again) = task::decide(&h.cfg, &c, &Input::Observed(unsure), NOW + DAY_MS).unwrap();
    assert!(again.is_empty(), "the second tick that cannot tell repeats nothing: {again:?}");
    let own = snap(vec![repo("shop", task::Verdict::CannotTell, &[line])], &[("shop", "repo-contract")]);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(own), NOW).unwrap();
    assert!(acts.iter().all(|a| a.name == task::INBOX_APPEND), "{acts:?}");
}

#[test]
fn a_not_met_repo_that_is_already_stopped_by_this_task_is_kept() {
    let h = t::handler::<H>(SET);
    let s = snap(vec![repo("shop", task::Verdict::NotMet, &[NOT_MET])], &[("shop", "repo-contract")]);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(s), NOW).unwrap();
    assert!(acts.is_empty(), "{acts:?}");
}

#[test]
fn an_own_stop_naming_other_lines_is_refreshed_without_a_second_inbox_line() {
    let h = t::handler::<H>(SET);
    let s = snap(vec![repo("shop", task::Verdict::NotMet, &["not met deploy: gone"])], &[("shop", "repo-contract")]);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(s), NOW).unwrap();
    assert_eq!(names(&acts), [task::STOP]);
}

#[test]
fn an_unreadable_stop_file_stops_the_pass_instead_of_hiding_a_stop_that_would_be_overwritten() {
    let d = testkit::TempDir::new("repo-contract-unreadable");
    std::fs::create_dir_all(scoped_stop::dir(d.path())).unwrap();
    std::fs::write(scoped_stop::dir(d.path()).join("repo-shop.json"), "{not json").unwrap();
    let p = Fake { out: "== shop\nnot met deploy: x\n", code: 1, seen: Default::default() };
    assert!(task::observe(&p, d.path(), "home").is_err());
    let input = serde_json::to_string(&task::Stop { repo: "shop".into(), reason: "r".into(), until: "u".into(), now_secs: 1 }).unwrap();
    assert!(task::stop(d.path(), &input).is_err());
}

struct Fake {
    out: &'static str,
    code: i32,
    seen: std::sync::Mutex<Vec<Vec<String>>>,
}

impl Proc for Fake {
    fn run(&self, spec: &ProcSpec) -> Result<ProcOutput, String> {
        assert_eq!(spec.prog, "repo-contract");
        self.seen.lock().unwrap().push(spec.args.clone());
        Ok(ProcOutput { code: Some(self.code), stdout: self.out.as_bytes().to_vec(), stderr: vec![] })
    }
}

const OUT: &str = "== shop\nmet environment: ok\nnot met submit-checks: no checks\nmet test-tiers: ok\nmet landing: ok\nmet deploy: ok\n== home\nnot met deploy: none\n== calm\nmet environment: a\nmet submit-checks: b\nmet test-tiers: c\nmet landing: d\nmet deploy: e\n== odd\nmet environment: a\ncannot tell deploy: no ref\n== gone\n";

#[test]
fn the_home_repo_is_never_observed_and_each_repo_is_judged_from_its_own_lines() {
    let p = Fake { out: OUT, code: 1, seen: Default::default() };
    let d = testkit::TempDir::new("repo-contract-observe");
    let s: Snapshot = serde_json::from_str(&task::observe(&p, d.path(), "home").unwrap()).unwrap();
    assert_eq!(*p.seen.lock().unwrap(), [["check", "--all"]], "one pass over every product repo, never a named one");
    let got: Vec<_> = s.repos.iter().map(|r| (r.repo.as_str(), r.verdict)).collect();
    assert_eq!(got, [("shop", task::Verdict::NotMet), ("calm", task::Verdict::Met), ("odd", task::Verdict::CannotTell), ("gone", task::Verdict::CannotTell)]);
    assert_eq!(s.repos[0].lines, ["not met submit-checks: no checks"]);
}

#[test]
fn a_run_that_prints_nothing_decides_nothing() {
    let p = Fake { out: "", code: 2, seen: Default::default() };
    let d = testkit::TempDir::new("repo-contract-silent");
    assert!(task::observe(&p, d.path(), "home").is_err());
}

#[test]
fn only_this_tasks_stops_are_read_back() {
    let d = testkit::TempDir::new("repo-contract-read");
    scoped_stop::pause_repo(d.path(), "a", &["claims".into()], "repo-contract", "x", "y", 1).unwrap();
    scoped_stop::pause_repo(d.path(), "b", &["claims".into()], "operator", "x", "y", 1).unwrap();
    let p = Fake { out: "== a\nmet environment: ok\n", code: 0, seen: Default::default() };
    let s: Snapshot = serde_json::from_str(&task::observe(&p, d.path(), "home").unwrap()).unwrap();
    let mut got: Vec<_> = s.stops.iter().map(|s| (s.repo.as_str(), s.owner.as_str())).collect();
    got.sort();
    assert_eq!(got, [("a", "repo-contract"), ("b", "operator")]);
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_repo_contract.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let d = testkit::TempDir::new("repo-contract-rows");
    let r = rig(&snap(vec![repo("shop", task::Verdict::NotMet, &[NOT_MET])], &[]), d.path());
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&r, task::NAME);
    for key in ["last_fired", "unsure"] {
        assert!(rows.iter().any(|k| k == key), "{key} in {rows:?}");
    }
}

const SLOT_MS: u64 = 5 * 3_600_000 + 10 * 60_000;

fn fresh(hang: bool) -> TaskRig {
    let world = json(&snap(vec![repo("shop", task::Verdict::Met, &[])], &[]));
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration())
        .unwrap()
        .double(task::OBSERVE, move |_| Ok(world.clone()))
        .double(task::INBOX_APPEND, |_| Ok("appended".into()));
    if hang {
        r = r.hanging(task::OBSERVE);
    }
    r.clock.advance_to(NOW);
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r
}

fn observes(r: &TaskRig) -> usize {
    r.requested().iter().filter(|q| q.name == task::OBSERVE).count()
}

fn restart(r: &mut TaskRig) {
    r.reopen(t::handler::<H>(SET).registration()).unwrap();
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
}

#[test]
fn a_pass_lost_to_a_restart_is_not_restarted() {
    let mut r = fresh(true);
    r.advance(SLOT_MS).unwrap();
    assert_eq!(observes(&r), 1);
    for _ in 0..3 {
        r.advance(60_000).unwrap();
        restart(&mut r);
    }
    assert_eq!(observes(&r), 1, "a restart restarted the lost pass");
    r.advance(DAY_MS).unwrap();
    assert_eq!(observes(&r), 2);
}

#[test]
fn a_task_that_never_fired_runs_nothing_at_boot() {
    let mut r = fresh(false);
    for _ in 0..3 {
        restart(&mut r);
    }
    assert_eq!(observes(&r), 0);
    r.advance(SLOT_MS).unwrap();
    assert_eq!(observes(&r), 1);
}

#[test]
fn a_completed_pass_is_not_repeated_by_a_restart() {
    let mut r = fresh(false);
    r.advance(SLOT_MS).unwrap();
    assert_eq!(observes(&r), 1);
    for _ in 0..3 {
        restart(&mut r);
    }
    assert_eq!(observes(&r), 1);
    r.advance(DAY_MS).unwrap();
    assert_eq!(observes(&r), 2);
}

#[test]
fn the_stamp_is_the_start_of_the_pass_and_survives_a_restart() {
    let mut r = fresh(false);
    r.advance(SLOT_MS).unwrap();
    let slot = r.now();
    r.advance(60_000).unwrap();
    restart(&mut r);
    assert_eq!(t::recorded::<H>(&r).unwrap().last_fired, Some(slot));
}

#[test]
fn a_pass_that_timed_out_is_not_restarted_by_a_restart() {
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().timing_out();
    r.clock.advance_to(NOW);
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r.advance(SLOT_MS).unwrap();
    assert_eq!(observes(&r), 1);
    for _ in 0..3 {
        restart(&mut r);
    }
    assert_eq!(observes(&r), 1);
}

#[test]
fn fire_stamps_the_start_and_a_completed_observe_keeps_it() {
    let started = task::decide(&Default::default(), &Carry::default(), &Input::Fire, 7_000).unwrap().0;
    assert_eq!(started.last_fired, Some(7_000));
    let done = task::decide(&Default::default(), &started, &Input::Observed(Snapshot::default()), 9_000).unwrap().0;
    assert_eq!(done.last_fired, Some(7_000));
}

fn first_fire(set: &[(&str, &str)], seed: u64) -> u64 {
    let mut r = TaskRig::new(seed, t::handler::<H>(set).registration()).unwrap();
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r.engine().next_timer_at().expect("the boot tick arms the first tick")
}

#[test]
fn a_calendar_tick_fires_at_its_slot_plus_a_seeded_offset_below_the_jitter() {
    let (a, b) = (first_fire(SET, 1), first_fire(SET, 2));
    for at in [a, b] {
        assert!((SLOT_MS..SLOT_MS + 600_000).contains(&at), "{at} outside the jitter window");
    }
    assert_ne!(a, b, "two seeds draw two offsets");
    assert_eq!(first_fire(SET, 1), a, "one seed draws the same offset");
}

#[test]
fn a_jitter_of_zero_fires_exactly_on_the_slot() {
    let zero = [(task::CADENCE_KEY, "*-*-* 05:10:00"), (task::JITTER_KEY, "0")];
    assert_eq!(first_fire(&zero, 1), SLOT_MS);
    assert_eq!(first_fire(&zero, 2), SLOT_MS);
}

#[test]
fn a_fired_tick_rearms_the_next_slot_plus_a_seeded_offset() {
    let d = testkit::TempDir::new("repo-contract-rearm");
    let r = rig(&snap(vec![], &[]), d.path());
    let next = r.engine().next_timer_at().expect("the fire re-arms");
    let slot = NOW + SLOT;
    assert!((slot..slot + 600_000).contains(&next), "{next} outside [{slot}, +600 s)");
}

#[test]
fn a_jitter_that_overflows_milliseconds_refuses_the_build() {
    let big = [(task::CADENCE_KEY, "*-*-* 05:10:00"), (task::JITTER_KEY, "18446744073709551615")];
    assert!(<H as Stub>::from_cfg(&t::lookup::<H>(&big)).is_err());
}

#[test]
fn only_the_tasks_that_declare_no_jitter_key_have_none() {
    use spira_runtime::tasks::c::{sweep_escape_census, sweep_tsd};
    assert!(task::SPEC.jitter_key == Some(task::JITTER_KEY));
    assert!(sweep_escape_census::SPEC.jitter_key.is_none() && sweep_tsd::SPEC.jitter_key.is_none());
}
