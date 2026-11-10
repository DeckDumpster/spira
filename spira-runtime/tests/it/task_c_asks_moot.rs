//! T1–T6 for `asks.moot`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/asks-moot.md`. Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::asks_moot::{self as task, Ask, AsksMoot as H, Carry, CheckOutcome, Phase, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "900"), ("SPIRA_MOOT_TIMEOUT", "30")];
const TICK: u64 = 900_000;

fn ask(id: &str, pred: &str, works: &[&str]) -> Ask {
    Ask { id: id.into(), pred: pred.into(), works: works.iter().map(|s| s.to_string()).collect() }
}

fn checked(id: &str, rc: i32) -> Input<Snapshot> {
    Input::Outcome { effect: task::CHECK.into(), ok: true, body: serde_json::to_string(&CheckOutcome { id: id.into(), rc, output: "evidence".into() }).unwrap() }
}

fn at(id: &str, phase: Phase) -> Carry {
    Carry { asks: BTreeMap::from([(id.to_string(), phase)]), ..Carry::default() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_15min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_a_live_predicate_resolves_nothing_and_journals_no_emit() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &at("sp-a", Phase::Checking), &checked("sp-a", 1), TICK).unwrap();
    assert!(acts.is_empty() && c.asks.get("sp-a") == Some(&Phase::Live), "{acts:?}");
    t::t2_silent::<H>(SET, &json(&Snapshot::default()));
}

#[test]
fn t3_an_ask_is_checked_cleared_resolved_withdrawn_and_a_resolve_from_timed_out_is_refused() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { asks: vec![ask("sp-a", "test -f /gone", &["sp-w"])] }), TICK).unwrap();
    assert_eq!((c.asks.get("sp-a").copied(), names(&acts)), (Some(Phase::Checking), vec![task::CHECK]));
    let (c, acts) = task::decide(&h.cfg, &c, &checked("sp-a", 0), TICK).unwrap();
    assert_eq!((c.asks.get("sp-a").copied(), names(&acts)), (Some(Phase::Cleared), vec![task::RESOLVE]));
    let (c, acts) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::RESOLVE.into(), ok: true, body: "sp-a".into() }, TICK).unwrap();
    assert_eq!((c.asks.get("sp-a").copied(), names(&acts)), (Some(Phase::Resolved), vec![task::WITHDRAW]));
    let (c, _) = task::decide(&h.cfg, &at("sp-b", Phase::Checking), &checked("sp-b", task::TIMED_OUT_RC), TICK).unwrap();
    assert_eq!(c.asks.get("sp-b"), Some(&Phase::TimedOut));
    let refused = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::RESOLVE.into(), ok: true, body: "sp-b".into() }, TICK).unwrap_err();
    assert!(refused.contains("timed_out"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_handler_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/asks_moot.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_cleared_predicate_resolves_with_the_old_reason_and_rc_124_never_reads_as_cleared() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &at("sp-a", Phase::Checking), &checked("sp-a", 0), TICK).unwrap();
    assert_eq!(names(&acts), [task::RESOLVE]);
    assert!(acts[0].input.contains("auto-resolved: MOOT-WHEN predicate cleared") && acts[0].input.contains("exit 0"), "{}", acts[0].input);
    let (_, acts) = task::decide(&h.cfg, &at("sp-a", Phase::Checking), &checked("sp-a", task::TIMED_OUT_RC), TICK).unwrap();
    assert!(acts.is_empty(), "a timeout leaves the ask open: {acts:?}");
}

use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::tasks::b::kit::FIRE;

/// One ask whose predicate cleared and whose resolve the store refused: it stays cleared, held.
fn cleared_and_unresolved() -> TaskRig {
    let listed = serde_json::to_string(&Snapshot { asks: vec![ask("sp-a", "test -f /gone", &["sp-w"])] }).unwrap();
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration())
        .unwrap()
        .double(task::OBSERVE, move |_| Ok(listed.clone()))
        .double(task::CHECK, |input| {
            let c: task::Check = serde_json::from_str(input).unwrap();
            Ok(serde_json::to_string(&CheckOutcome { id: c.id, rc: 0, output: "gone".into() }).unwrap())
        })
        .double(task::RESOLVE, |_| Err("store unreachable".into()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&cleared_and_unresolved(), task::NAME);
    for key in ["asks/sp-a", "held/sp-a"] {
        assert!(rows.iter().any(|k| k == key), "{key} in {rows:?}");
    }
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(cleared_and_unresolved, || t::handler::<H>(SET).registration(), task::STREAM, runtime::event::EventKind::new(task::TICK, FIRE));
}
