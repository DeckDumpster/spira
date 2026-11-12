//! T1–T6 for `hel.sweep` (a P2 machine on a calendar tick, per the corrections), from the
//! Acceptance section of `wiki/projects/spira/designs/tasks/hel-sweep.md`. Red until ported.

use runtime::units::{Exit, UnitHandle, UnitRan};
use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::hel_sweep::{self as task, Branch, Carry, HelSweep as H, Pass, Phase, Snapshot};

const SPEC: &str = "*-*-* *:35:00";
const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, SPEC), (task::JITTER_KEY, "300"), (task::BUDGET_KEY, "900")];
const AT_35: u64 = 35 * 60_000;

fn branch(id: &str, content_on_base: bool) -> Branch {
    Branch { id: id.into(), br: format!("spira/{id}"), tip: "abc123".into(), ahead: 2, content_on_base, merges_clean: true, has_own_commit: true, ..Branch::default() }
}

/// One repo with one KEEP-unlanded branch.
fn keep() -> Snapshot {
    Snapshot { repo: "spira".into(), branches: vec![branch("sp-k", false)], rows: [("sp-k".to_string(), "SUBMITTED".to_string())].into(), ..Snapshot::default() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_the_next_35() {
    t::t1_registered_and_boots::<H>(SET, AT_35);
}

#[test]
fn t2_a_keep_unlanded_branch_and_a_halted_world_are_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&keep()));
    t::t2_silent::<H>(SET, &json(&Snapshot { repo: "spira".into(), halted: true, ..Snapshot::default() }));
}

#[test]
fn t3_a_pass_observes_acts_and_ends_and_a_tick_while_acting_is_refused() {
    let h = t::handler::<H>(SET);
    let (c, _) = task::decide(&h.cfg, &Carry::default(), &Input::Fire, AT_35).unwrap();
    assert_eq!(c.phase, Phase::Observing);
    let (c, acts) = task::decide(&h.cfg, &c, &Input::Observed(Snapshot { branches: vec![branch("sp-a", true)], ..keep() }), AT_35).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Acting, vec![task::SEND]));
    let refused = task::decide(&h.cfg, &c, &Input::Fire, AT_35 + 3_600_000).unwrap_err();
    assert!(refused.contains("acting"), "{refused}");
    let ran = UnitRan { handle: UnitHandle::of(task::NAME, "spira:spira/sp-a:abc123").unwrap(), exit: Exit::Code(0), duration_ms: 30_000 };
    assert!(ran.key_for(task::NAME).is_some());
    let (c, _) = task::decide(&h.cfg, &c, &Input::Ran(ran), AT_35 + 30_000).unwrap();
    assert_eq!(c.phase, Phase::Idle, "the last leg ended, the pass is done");
    let spent = Carry { phase: Phase::Acting, pass: Some(Pass { started: AT_35, repos_left: vec!["spira".into()], cursor: 0, ..Pass::default() }), ..Carry::default() };
    let (c, acts) = task::decide(&h.cfg, &spent, &Input::Outcome { effect: task::SEND.into(), ok: true, body: "sent".into() }, AT_35 + 901_000).unwrap();
    assert_eq!(c.phase, Phase::BudgetSpent);
    assert!(acts.is_empty(), "past the 900 s budget nothing more is issued: {acts:?}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/hel_sweep.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_one_disposition_per_branch_as_hel_dry_run_would() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let landed = Snapshot { branches: vec![branch("sp-a", true), branch("sp-k", false)], rows: [("sp-a".to_string(), "SUBMITTED".to_string()), ("sp-k".to_string(), "SUBMITTED".to_string())].into(), ..keep() };
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(landed), AT_35).unwrap();
    assert_eq!(names(&acts), [task::SEND], "WOULD send sp-a (content landed); KEEP sp-k");
    let held = Snapshot { holders: [("sp-a".to_string(), "store did not answer".to_string())].into(), ..Snapshot { branches: vec![branch("sp-a", true)], ..keep() } };
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(held), AT_35).unwrap();
    assert!(acts.is_empty(), "a held bead is skipped: {acts:?}");
}

use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::tasks::b::kit::FIRE;

/// A landed branch whose send leg started and has not ended: the pass is acting on one job.
fn acting_on_a_started_leg() -> TaskRig {
    let world = json(&Snapshot { branches: vec![branch("sp-a", true)], rows: [("sp-a".to_string(), "SUBMITTED".to_string())].into(), ..keep() });
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.clone())).double(task::SEND, |_| Ok("started".into()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r.tick(stream, runtime::event::EventKind::new(task::TICK, FIRE)).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&acting_on_a_started_leg(), task::NAME);
    for key in ["phase", "last_fired", "pass", "tally", "jobs/spira:spira/sp-a:abc123"] {
        assert!(rows.iter().any(|k| k == key), "{key} in {rows:?}");
    }
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(acting_on_a_started_leg, || t::handler::<H>(SET).registration(), task::STREAM, runtime::event::EventKind::new(task::TICK, FIRE));
}

#[test]
fn a_store_that_would_not_answer_the_branch_read_fails_the_pass_naming_the_repo_and_the_stores_text() {
    let e = task::store_failure("spira", "reads every bead ever filed and is refused");
    assert!(e.starts_with("hel.sweep: spira:") && e.contains("refused"), "{e}");
}

/// A pass whose scheduler restarts before its next slot: the timer that held the slot is gone,
/// and the boot must run the sweep that was due rather than wait an hour for the next one.
#[test]
fn a_slot_missed_across_a_restart_runs_when_the_scheduler_comes_back() {
    let worlds = [json(&Snapshot { branches: vec![branch("sp-a", true)], rows: [("sp-a".to_string(), "SUBMITTED".to_string())].into(), ..keep() }), json(&Snapshot { branches: vec![branch("sp-b", true)], rows: [("sp-b".to_string(), "SUBMITTED".to_string())].into(), ..keep() })];
    let asked = std::sync::atomic::AtomicUsize::new(0);
    let observe = move |_: &str| Ok(worlds[asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst).min(1)].clone());
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, observe).double(task::SEND, |_| Ok("started".into()));
    let (_, stream, boot) = H::boot();
    r.tick(stream, boot.clone()).unwrap();
    r.tick(stream, runtime::event::EventKind::new(task::TICK, FIRE)).unwrap();
    assert!(r.requested().iter().any(|q| q.name == task::SEND && q.input.contains("sp-a")), "the pass sends sp-a");

    let down_until = 2 * AT_35 / 3 * 2;
    assert!(down_until > AT_35 && down_until < AT_35 + 3_600_000);
    r.clock.advance_to(down_until);
    r.reopen(t::handler::<H>(SET).registration()).unwrap();
    r.tick(stream, boot).unwrap();
    assert!(r.requested().iter().any(|q| q.name == task::SEND && q.input.contains("sp-b")), "the sweep due while the scheduler was down runs at its boot: {:?}", r.requested().iter().map(|q| (&q.name, &q.input)).collect::<Vec<_>>());
}

#[test]
fn a_restart_that_lost_the_probe_of_a_pass_between_repositories_observes_that_repository_again() {
    let cadence = runtime::calendar::Calendar::parse(SPEC).map(spira_runtime::tasks::b::kit::Cadence::Calendar).unwrap();
    let between = Carry { phase: Phase::Observing, pass: Some(Pass { started: AT_35, repos_left: vec![], cursor: 1, observing: "second".into() }), ..Carry::default() };
    assert_eq!(task::catch_up(&between, AT_35 + 60_000, &cadence), Some("second".to_string()));
    let idle_and_current = Carry { last_fired: Some(AT_35), ..Carry::default() };
    assert_eq!(task::catch_up(&idle_and_current, AT_35 + 60_000, &cadence), None, "the next slot has not passed");
    assert_eq!(task::catch_up(&Carry::default(), AT_35 + 7_200_000, &cadence), None, "a fresh journal only arms");
}

fn real_in(run: &std::path::Path) -> hel::real::Real {
    hel::real::Real::minimal(run.to_path_buf(), run.to_path_buf(), None)
}

#[test]
fn the_resume_cursor_is_kept_per_repo_in_the_run_dir_and_only_moves_forward() {
    use hel::ports::World;
    let dir = testkit::TempDir::new("hel-sweep-cursor");
    let real = real_in(dir.path());
    task::mark_handled(&real, "spira", "spira/sp-c");
    task::mark_handled(&real, "spira", "spira/sp-a");
    assert_eq!(real.resume_after("spira").as_deref(), Some("spira/sp-c"), "a late answer does not pull the cursor back");
    assert_eq!(std::fs::read_to_string(dir.path().join("hel-resume.spira")).unwrap().trim(), "spira/sp-c");
    assert_eq!(real.resume_after("other"), None);
    task::mark_handled(&real, "spira", "spira/sp-d");
    assert_eq!(real.resume_after("spira").as_deref(), Some("spira/sp-d"));
}

#[test]
fn the_pace_is_read_from_the_pinned_ms_and_a_non_number_refuses() {
    let get = |ms: &'static str| move |k: &str| Ok::<String, String>(if k == task::PACE_KEY { ms.to_string() } else { "x".to_string() });
    assert_eq!(task::Pinned::read(&get("40")).unwrap().pace, std::time::Duration::from_millis(40));
    assert_eq!(task::Pinned::read(&get("0")).unwrap().pace, std::time::Duration::ZERO);
    assert!(task::Pinned::read(&get("soon")).err().unwrap().contains(task::PACE_KEY));
    assert!(task::KEYS.contains(&task::PACE_KEY));
}

struct Timed(std::sync::Mutex<Vec<std::time::Duration>>);

impl spira_store::lc_call::Transport for Timed {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        let at = std::time::Instant::now();
        let reply = spira_store::lc_call::Reply { code: 0, out: "[]".into(), err: String::new() };
        self.0.lock().unwrap().push(at.elapsed());
        Ok(reply)
    }
}

#[test]
fn a_600_branch_pass_reads_the_store_in_bounded_calls_none_over_a_second() {
    use hel::ports::World;
    let dir = testkit::TempDir::new("hel-sweep-600");
    let timed = std::sync::Arc::new(Timed(Default::default()));
    let real = real_in(dir.path()).with_lc(spira_store::lc_call::LcClient::over(timed.clone(), true));
    let ids: Vec<String> = (0..600).map(|i| format!("sp-{i:03}")).collect();
    real.prefetch(&ids).unwrap();
    let calls = timed.0.lock().unwrap();
    assert_eq!(calls.len(), 12, "6 content reads and 6 list reads of 100 ids, not one per branch");
    assert!(calls.iter().all(|d| *d < std::time::Duration::from_millis(1000)), "{calls:?}");
}
