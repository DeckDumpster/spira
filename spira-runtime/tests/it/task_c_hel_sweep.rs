//! T1–T6 for `hel.sweep` (a P2 machine on a calendar tick, per the corrections), from the
//! Acceptance section of `wiki/projects/spira/designs/tasks/hel-sweep.md`. Red until ported.

use runtime::units::{Exit, UnitHandle, UnitRan};
use spira_runtime::tasks::b::kit::{names, testing as t, Act, Input};
use spira_runtime::tasks::c::hel_sweep::{self as task, Base, Bead, Branch, Carry, HelSweep as H, Pass, Phase, Snapshot};

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

#[test_id(t1kj4z0)]
#[test]
fn t1_registered_boots_and_arms_the_next_35() {
    t::t1_registered_and_boots::<H>(SET, AT_35);
}

#[test_id(ts5tfeo)]
#[test]
fn t2_a_keep_unlanded_branch_and_a_halted_world_are_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&keep()));
    t::t2_silent::<H>(SET, &json(&Snapshot { repo: "spira".into(), halted: true, ..Snapshot::default() }));
}

#[test_id(t4f2sz0)]
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

#[test_id(tazu7yx)]
#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/hel_sweep.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test_id(tiqwsga)]
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

#[test_id(t7ripcu)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&acting_on_a_started_leg(), task::NAME);
    for key in ["phase", "last_fired", "pass", "tally", "jobs/spira:spira/sp-a:abc123"] {
        assert!(rows.iter().any(|k| k == key), "{key} in {rows:?}");
    }
    let mut r = acting_on_a_started_leg();
    r.deliver(task::STREAM, leg_ran(1).event_kind()).unwrap();
    let rows = kit::assert_kept_as_rows(&r, task::NAME);
    assert!(rows.iter().any(|k| k == "refused/spira:spira/sp-a"), "{rows:?}");
}

fn leg_ran(code: i32) -> UnitRan {
    UnitRan { handle: UnitHandle::of(task::NAME, "spira:spira/sp-a:abc123").unwrap(), exit: Exit::Code(code), duration_ms: 30_000 }
}

/// The branch `sp-a`, content landed, with the three inputs a refusal is recorded against.
fn landed(tip: &str, row: &str, base: &str) -> Snapshot {
    let b = Branch { tip: tip.into(), ..branch("sp-a", true) };
    Snapshot { branches: vec![b], rows: [("sp-a".to_string(), row.to_string())].into(), base: Base { sha: base.into(), ..Base::default() }, ..keep() }
}

fn same() -> Snapshot {
    landed("abc123", "SUBMITTED", "base1")
}

/// `sp-a` with no bead and no row: the sweep archives it.
fn unbeaded() -> Snapshot {
    Snapshot { rows: Default::default(), branches: vec![branch("sp-a", false)], ..same() }
}

/// `sp-a` superseded with unlanded work in a worktree: the sweep frees the worktree.
fn superseded_unsafe() -> Snapshot {
    let b = Branch { worktree: Some("run/worktree/sp-a".into()), ..branch("sp-a", false) };
    Snapshot { branches: vec![b], beads: [("sp-a".to_string(), Bead { supersedes: vec!["supersedes".into()], ..Bead::default() })].into(), ..same() }
}

fn step(carry: &Carry, input: Input<Snapshot>) -> (Carry, Vec<Act>) {
    task::decide(&t::handler::<H>(SET).cfg, carry, &input, AT_35).unwrap()
}

fn pass(carry: &Carry, world: Snapshot) -> (Carry, Vec<Act>) {
    let (c, _) = step(carry, Input::Fire);
    step(&c, Input::Observed(world))
}

fn answer(effect: &str, ok: bool, body: &str) -> Input<Snapshot> {
    Input::Outcome { effect: effect.into(), ok, body: body.into() }
}

/// A pass over `world` that plans `effect`, ended by `result`.
fn failed(world: Snapshot, effect: &str, result: Input<Snapshot>) -> Carry {
    let (c, acts) = pass(&Carry::default(), world);
    assert_eq!(names(&acts), [effect]);
    let (c, _) = step(&c, result);
    assert_eq!((c.phase, c.tally.failed), (Phase::Idle, 1));
    c
}

fn send_failed() -> Carry {
    failed(same(), task::SEND, Input::Ran(leg_ran(1)))
}

#[test_id(txljyo3)]
#[test]
fn a_failed_leg_is_not_retried_while_the_tip_row_state_and_base_stand() {
    let (c, acts) = pass(&send_failed(), same());
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(c.tally.refused, 1);
    assert!(c.refused.contains_key("spira:spira/sp-a"), "{c:?}");
}

#[test_id(tgpsqzx)]
#[test]
fn a_moved_tip_row_state_or_base_drops_the_record_and_sends_again() {
    for moved in [landed("def456", "SUBMITTED", "base1"), landed("abc123", "CERTIFIED", "base1"), landed("abc123", "SUBMITTED", "base2")] {
        let (c, acts) = pass(&send_failed(), moved.clone());
        assert_eq!(names(&acts), [task::SEND], "{moved:?}");
        assert!(c.refused.is_empty(), "{moved:?}: {c:?}");
        assert_eq!(c.tally.refused, 0);
    }
}

#[test_id(tql5ali)]
#[test]
fn a_send_that_failed_to_start_a_failed_archive_and_a_failed_free_are_not_retried() {
    for (world, effect) in [(same(), task::SEND), (unbeaded(), task::ARCHIVE), (superseded_unsafe(), task::FREE_WORKTREE)] {
        let c = failed(world.clone(), effect, answer(effect, false, "refused"));
        let (c, acts) = pass(&c, world.clone());
        assert!(acts.is_empty(), "{effect}: {acts:?}");
        assert_eq!(c.tally.refused, 1, "{effect}");
        let (c, acts) = pass(&c, Snapshot { base: Base { sha: "base2".into(), ..Base::default() }, ..world });
        assert_eq!(names(&acts), [effect]);
        assert!(c.refused.is_empty(), "{effect}: {c:?}");
    }
}

#[test_id(tz49mwj)]
#[test]
fn the_leg_chained_on_an_archive_records_its_failure_against_the_observed_inputs() {
    let (c, _) = pass(&Carry::default(), unbeaded());
    let (c, acts) = step(&c, answer(task::ARCHIVE, true, "archive:spira:sp-a"));
    assert_eq!(names(&acts), [task::SEND]);
    let (c, _) = step(&c, Input::Ran(leg_ran(1)));
    let (c, acts) = pass(&c, unbeaded());
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(c.tally.refused, 1);
}

#[test_id(t7ticwb)]
#[test]
fn a_queued_leg_records_nothing_and_a_successful_act_clears_the_record() {
    let (c, _) = pass(&Carry::default(), same());
    let (c, _) = step(&c, Input::Ran(leg_ran(2)));
    assert!(c.refused.is_empty(), "{c:?}");
    for (world, effect, result) in [(same(), task::SEND, Input::Ran(leg_ran(0))), (superseded_unsafe(), task::FREE_WORKTREE, answer(task::FREE_WORKTREE, true, ""))] {
        let (mut c, _) = pass(&Carry::default(), world);
        c.refused.insert("spira:spira/sp-a".into(), "stale".into());
        let (c, _) = step(&c, result);
        assert!(c.refused.is_empty(), "{effect}: {c:?}");
    }
}

#[test_id(ty02i2x)]
#[test]
fn a_record_is_dropped_when_its_branch_is_gone_but_not_when_the_observe_did_not_reach_it() {
    let gone = Snapshot { branches: vec![], ..same() };
    let (c, _) = pass(&send_failed(), gone.clone());
    assert!(c.refused.is_empty(), "{c:?}");
    for partial in [
        Snapshot { resumed_after: "spira/sp-b".into(), ..gone.clone() },
        Snapshot { cut_at: "spira/sp-0".into(), ..gone.clone() },
        Snapshot { unread: true, base: Base::default(), ..gone.clone() },
        Snapshot { repo: "other".into(), ..gone.clone() },
    ] {
        let (c, _) = pass(&send_failed(), partial.clone());
        assert!(c.refused.contains_key("spira:spira/sp-a"), "{partial:?}: {c:?}");
    }
}

#[test_id(th594qx)]
#[test]
fn a_pass_forgotten_past_twice_its_budget_keeps_the_records() {
    let (mut c, _) = pass(&Carry::default(), same());
    c.refused.insert("spira:spira/sp-z".into(), "t|-|b".into());
    let (c, _) = task::decide(&t::handler::<H>(SET).cfg, &c, &answer(task::SEND, true, ""), AT_35 + 1_801_000).unwrap();
    assert_eq!(c.phase, Phase::Idle);
    assert!(c.refused.contains_key("spira:spira/sp-z"), "{c:?}");
}

#[test_id(teg1c3p)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(acting_on_a_started_leg, || t::handler::<H>(SET).registration(), task::STREAM, runtime::event::EventKind::new(task::TICK, FIRE));
}

#[test_id(teo4eiu)]
#[test]
fn a_store_that_would_not_answer_the_branch_read_fails_the_pass_naming_the_repo_and_the_stores_text() {
    let e = task::store_failure("spira", "reads every bead ever filed and is refused");
    assert!(e.starts_with("hel.sweep: spira:") && e.contains("refused"), "{e}");
}

/// A pass whose scheduler restarts before its next slot: the timer that held the slot is gone,
/// and the boot must run the sweep that was due rather than wait an hour for the next one.
#[test_id(tclkcl3)]
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

#[test_id(t07xjuz)]
#[test]
fn a_restart_that_lost_the_probe_of_a_pass_between_repositories_observes_that_repository_again() {
    let cadence = runtime::calendar::Calendar::parse(SPEC).map(|c| spira_runtime::tasks::b::kit::Cadence::Calendar(c, 0)).unwrap();
    let between = Carry { phase: Phase::Observing, pass: Some(Pass { started: AT_35, repos_left: vec![], cursor: 1, observing: "second".into() }), ..Carry::default() };
    assert_eq!(task::catch_up(&between, AT_35 + 60_000, &cadence), Some("second".to_string()));
    let idle_and_current = Carry { last_fired: Some(AT_35), ..Carry::default() };
    assert_eq!(task::catch_up(&idle_and_current, AT_35 + 60_000, &cadence), None, "the next slot has not passed");
    assert_eq!(task::catch_up(&Carry::default(), AT_35 + 7_200_000, &cadence), None, "a fresh journal only arms");
}

fn real_in(run: &std::path::Path) -> hel::real::Real {
    hel::real::Real::minimal(run.to_path_buf(), run.to_path_buf(), None)
}

#[test_id(tjm8uvi)]
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

#[test_id(t982fym)]
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
    fn exchange(&self, _: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        let at = std::time::Instant::now();
        let reply = spira_store::lc_call::Reply { code: 0, out: "[]".into(), err: String::new(), read_ended: false };
        self.0.lock().unwrap().push(at.elapsed());
        Ok(reply)
    }
}

#[test_id(tb4092p)]
#[test]
fn a_600_branch_prefetch_is_twelve_bounded_store_calls() {
    use hel::ports::World;
    let dir = testkit::TempDir::new("hel-sweep-600");
    let timed = std::sync::Arc::new(Timed(Default::default()));
    let real = real_in(dir.path()).with_lc(spira_store::lc_call::LcClient::over(timed.clone(), true));
    let ids: Vec<String> = (0..600).map(|i| format!("sp-{i:03}")).collect();
    real.prefetch(&ids).unwrap();
    let calls = timed.0.lock().unwrap();
    assert_eq!(calls.len(), 12, "6 content reads and 6 list reads of 100 ids, not one per branch");
}

#[test_id(t0mzstd)]
#[test]
fn a_cursor_skips_branches_up_to_it_even_when_the_branch_it_names_is_gone() {
    assert!(!task::past_cursor("spira/sp-b", Some("spira/sp-c")));
    assert!(!task::past_cursor("spira/sp-c", Some("spira/sp-c")));
    assert!(task::past_cursor("spira/sp-d", Some("spira/sp-c")));
    assert!(task::past_cursor("spira/sp-a", None));
}

#[test_id(tjusu04)]
#[test]
fn a_resumed_repo_with_nothing_to_act_on_clears_its_cursor_and_one_with_work_keeps_it() {
    use hel::ports::World;
    let dir = testkit::TempDir::new("hel-sweep-idle");
    let real = real_in(dir.path());
    task::mark_handled(&real, "spira", "spira/sp-a");
    task::clear_cursor_when_idle(&real, "submitted", &Snapshot { branches: vec![branch("sp-a", true)], ..keep() }).unwrap();
    assert_eq!(real.resume_after("spira").as_deref(), Some("spira/sp-a"), "a landed branch is still to be sent");
    task::clear_cursor_when_idle(&real, "submitted", &keep()).unwrap();
    assert_eq!(real.resume_after("spira"), None);
}

fn with_second(mut world: Snapshot) -> Snapshot {
    let b = Branch { id: "sp-b".into(), br: "spira/sp-b".into(), worktree: world.branches[0].worktree.as_ref().map(|_| "run/worktree/sp-b".into()), ..world.branches[0].clone() };
    world.branches.push(b);
    if let Some(row) = world.rows.get("sp-a").cloned() {
        world.rows.insert("sp-b".into(), row);
    }
    if let Some(bead) = world.beads.get("sp-a").cloned() {
        world.beads.insert("sp-b".into(), bead);
    }
    world
}

#[test_id(t79frd9)]
#[test]
fn a_runners_failure_is_recorded_against_the_branch_it_was_asked_for() {
    for (world, effect) in [(same(), task::SEND), (unbeaded(), task::ARCHIVE), (superseded_unsafe(), task::FREE_WORKTREE)] {
        let world = json(&with_second(world));
        // `sp-b` sorts after `sp-a`, so a failure charged by position would land on `sp-a`.
        let only_b_fails = move |input: &str| if input.contains("sp-b") { Err("the store did not answer".to_string()) } else { Ok(format!("{effect}:spira:sp-a")) };
        // A succeeded archive chains `sp-a`'s send; the later double of one name is the one kept.
        let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.clone())).double(task::SEND, |_| Ok("started".into())).double(effect, only_b_fails);
        let (_, stream, kind) = H::boot();
        r.tick(stream, kind).unwrap();
        r.tick(stream, runtime::event::EventKind::new(task::TICK, FIRE)).unwrap();
        let c = t::recorded::<H>(&r).unwrap();
        assert_eq!(c.refused.keys().collect::<Vec<_>>(), ["spira:spira/sp-b"], "{effect}: {c:?}");
    }
}

#[test_id(tzg1crb)]
#[test]
fn an_effect_that_times_out_is_recorded_against_each_branch_it_was_asked_for() {
    let world = json(&with_second(same()));
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.clone())).hanging(task::SEND);
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r.tick(stream, runtime::event::EventKind::new(task::TICK, FIRE)).unwrap();
    r.advance(121_000).unwrap();
    let c = t::recorded::<H>(&r).unwrap();
    assert_eq!(c.refused.keys().collect::<Vec<_>>(), ["spira:spira/sp-a", "spira:spira/sp-b"], "{c:?}");
}

#[test_id(t203xra)]
#[test]
fn a_failure_that_names_none_of_two_jobs_is_recorded_against_neither() {
    for (world, effect) in [(same(), task::SEND), (unbeaded(), task::ARCHIVE)] {
        let (c, _) = pass(&Carry::default(), with_second(world));
        let (c, _) = step(&c, answer(effect, false, ""));
        assert!(c.refused.is_empty(), "{effect}: {c:?}");
        assert_eq!((c.jobs.len(), c.tally.failed), (1, 1), "the pass still counts it and waits on one less");
    }
}

#[test_id(t2tlhk4)]
#[test]
fn a_failure_that_answers_past_the_budget_is_recorded_and_issues_nothing() {
    for late in [901_000, 1_801_000] {
        let (c, _) = pass(&Carry::default(), same());
        let (c, acts) = task::decide(&t::handler::<H>(SET).cfg, &c, &Input::Ran(leg_ran(1)), AT_35 + late).unwrap();
        assert!(acts.is_empty(), "{late}: {acts:?}");
        let (c, acts) = pass(&c, same());
        assert!(acts.is_empty(), "{late}: {acts:?}");
        assert_eq!(c.tally.refused, 1, "{late}");
    }
}

#[test_id(tj9dl8m)]
#[test]
fn a_leg_that_fails_after_the_budget_closed_the_pass_is_still_recorded() {
    let cfg = t::handler::<H>(SET).cfg;
    let (c, _) = pass(&Carry::default(), same());
    let (c, _) = task::decide(&cfg, &c, &answer(task::SEND, true, "started"), AT_35 + 901_000).unwrap();
    assert_eq!(c.phase, Phase::BudgetSpent);
    let (c, acts) = task::decide(&cfg, &c, &Input::Ran(leg_ran(1)), AT_35 + 950_000).unwrap();
    assert!(acts.is_empty(), "{acts:?}");
    assert!(c.refused.contains_key("spira:spira/sp-a"), "{c:?}");
}

#[test_id(tevkajf)]
#[test]
fn the_observed_base_carries_the_land_refs_commit_and_moves_with_it() {
    let dir = testkit::TempDir::new("hel-sweep-base");
    let git = |args: &[&str]| testkit::git(dir.path(), args);
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "one"]);
    let observed = || task::observed_base(dir.path(), hel::ports::Base { landref: "main".into(), landrefs: vec![], remote: None }).sha;
    let first = observed();
    assert_eq!(first, git(&["rev-parse", "main"]));
    git(&["commit", "-q", "--allow-empty", "-m", "two"]);
    assert_eq!(observed(), git(&["rev-parse", "main"]));
    assert_ne!(observed(), first);
}

#[test_id(twr6ep2)]
#[test]
fn an_observes_window_is_what_it_resumed_after_and_where_it_was_cut() {
    let mut s = Snapshot::default();
    s.set_window(Some("spira/sp-b"), true, Some("spira/sp-d"));
    assert_eq!((s.resumed_after.as_str(), s.cut_at.as_str()), ("spira/sp-b", "spira/sp-d"));
    s.set_window(None, false, Some("spira/sp-d"));
    assert_eq!((s.resumed_after.as_str(), s.cut_at.as_str()), ("", ""));
}

#[test_id(tfnm3rz)]
#[test]
fn a_leg_that_answers_after_the_tick_following_a_spent_budget_issues_nothing() {
    let cfg = t::handler::<H>(SET).cfg;
    for (code, recorded) in [(0, false), (1, true)] {
        let (c, _) = pass(&Carry::default(), same());
        let (c, _) = task::decide(&cfg, &c, &answer(task::SEND, true, "started"), AT_35 + 901_000).unwrap();
        let (c, _) = step(&c, Input::Fire);
        let (c, acts) = step(&c, Input::Ran(leg_ran(code)));
        assert!(acts.is_empty(), "{code}: {acts:?}");
        assert_eq!((c.phase, c.refused.contains_key("spira:spira/sp-a")), (Phase::Observing, recorded), "{code}: {c:?}");
    }
}

#[test_id(tal70ct)]
#[test]
fn an_input_that_could_not_be_read_has_not_moved() {
    let unreadable = Snapshot { rows: Default::default(), base: Base::default(), inputs_unread: true, ..same() };
    let (c, acts) = pass(&send_failed(), unreadable);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(c.tally.refused, 1);
    let (_, acts) = pass(&c, same());
    assert!(acts.is_empty(), "the record kept its inputs: {acts:?}");
}
