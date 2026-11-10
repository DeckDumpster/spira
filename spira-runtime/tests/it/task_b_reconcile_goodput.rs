//! T1–T6 for `reconcile.goodput`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/reconcile-goodput.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::b::reconcile_goodput::{self as task, Carry, LockAnswer, PassDigest, Phase, ReconcileGoodput as H, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "60"), (task::PASS_TIMEOUT_KEY, "50"), (task::SHADOW_KINDS_KEY, "RoundController Lifecycle Alarms"), (task::DEADMAN_KEY, "180")];
const MIN: u64 = 60_000;

/// All sections fresh, distances below `close`, no intents due, the lock free.
fn quiet() -> Snapshot {
    Snapshot { halted: false, section_snapshot: serde_json::json!({"sections": [{"name": "lifecycle", "fresh": true, "distance": 0.0}], "intents": [], "pass": {"n": 7, "list_rev": 3}}), lock: LockAnswer::default() }
}

fn carried_equal() -> Carry {
    Carry { last: PassDigest { n: 7, list_rev: 3, ..PassDigest::default() }, recorded_at: Some(0), ..Carry::default() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_60s_tick() {
    t::t1_registered_and_boots::<H>(SET, MIN);
}

#[test]
fn t2_fresh_sections_and_nothing_due_is_the_probe_alone() {
    let h = t::handler::<H>(SET);
    let (next, acts) = task::decide(&h.cfg, &carried_equal(), &Input::Observed(quiet()), MIN).unwrap();
    assert!(acts.is_empty() && next == carried_equal(), "idle: {acts:?}");
    t::t2_silent::<H>(SET, &json(&quiet()));
}

#[test]
fn t3_the_pass_walks_lock_judge_step_record_release_and_refuses_a_step_past_its_deadline() {
    let h = t::handler::<H>(SET);
    let at = |phase| Carry { phase, token: Some("tok".into()), deadline_at: Some(50_000), ..Carry::default() };
    let ok = |effect: &str, body: &str| Input::Outcome { effect: effect.into(), ok: true, body: body.into() };
    let (c, acts) = task::decide(&h.cfg, &at(Phase::Locking), &ok(task::LOCK, r#"{"token":"tok"}"#), 1_000).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Judging, vec![task::JUDGE]));
    let (c, acts) = task::decide(&h.cfg, &at(Phase::Locking), &Input::Outcome { effect: task::LOCK.into(), ok: false, body: "locked".into() }, 1_000).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Standby, vec![task::STANDBY]));
    let (c, _) = task::decide(&h.cfg, &at(Phase::Recording), &ok(task::RECORD, "{}"), 2_000).unwrap();
    assert_eq!(c.phase, Phase::Releasing);
    let refused = task::decide(&h.cfg, &at(Phase::Stepping), &ok(task::PUT, "{}"), 60_000).unwrap_err();
    assert!(refused.contains("deadline"), "a step after the pass deadline is refused naming it: {refused}");
}

#[test]
fn t4_its_seven_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/reconcile_goodput.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_due_intent_takes_the_lock_and_a_halted_world_records_without_judging() {
    let h = t::handler::<H>(SET);
    let mut due = quiet();
    due.section_snapshot = serde_json::json!({"sections": [{"name": "lifecycle", "fresh": true, "distance": 0.9}], "intents": [{"key": "lifecycle/floor", "due": true}], "pass": {"n": 7, "list_rev": 3}});
    let (c, acts) = task::decide(&h.cfg, &carried_equal(), &Input::Observed(due), MIN).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Locking, vec![task::LOCK]));
    let halted = Snapshot { halted: true, ..quiet() };
    let (c, acts) = task::decide(&h.cfg, &carried_equal(), &Input::Observed(halted), MIN).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Locking, vec![task::LOCK]), "a halt is an input, not a skip: Mode::Halted still records");
}

/// A halted world, a lock granted, and a judge leg whose answer does not decode: the pass stays judging.
fn judging() -> runtime::taskrig::TaskRig {
    let halted = json(&Snapshot { halted: true, ..quiet() });
    let mut rig = runtime::taskrig::TaskRig::new(1, t::handler::<H>(SET).registration())
        .unwrap()
        .double(task::OBSERVE, move |_| Ok(halted.clone()))
        .double(task::LOCK, |_| Ok(r#"{"token":"tok"}"#.into()))
        .double(task::JUDGE, |_| Ok("not a judgement".into()));
    let (_, stream, kind) = H::boot();
    rig.tick(stream, kind).unwrap();
    rig
}

fn registration() -> runtime::event::Registration {
    t::handler::<H>(SET).registration()
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&judging(), task::NAME);
    for want in ["phase", "token", "holder", "deadline_at"] {
        assert!(rows.iter().any(|k| k == want), "{want} in {rows:?}");
    }
}

#[test]
fn a_boot_does_not_take_the_lock_again_for_a_pass_in_flight() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(judging, registration, task::STREAM, runtime::event::EventKind::new(task::TICK, ""));
}
