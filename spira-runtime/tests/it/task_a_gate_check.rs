//! Acceptance T1–T6 for `gate.check`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/gate-check.md`, Acceptance) on `TaskRig`. T6 runs two virtual days (the contract asks seven); its idle budget is
//! the contract's "0 per idle hour".

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: gate_check,
    registration: GateCheck::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
}

/// The contract's idle fixture: no gates, no ESCALATE line.
fn idle() -> Snapshot {
    Snapshot::default()
}

/// A gh:run gate escalated, blocking a bead.
fn busy() -> Snapshot {
    Snapshot { gate_list: vec!["g1".into()], gate_check_out: "ESCALATE g1 run 102 failed\n".into(), gate_blocked: [("g1".to_string(), Some("sp-x".to_string()))].into(), ..idle() }
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

fn ingested(ids: &[&str]) -> Carry {
    Carry { ingested: ids.iter().map(|s| s.to_string()).collect() }
}

/// `main.rs:100-127`: an escalated gate blocking a bead is resolved and its bead gets `ci.failed`.
#[test]
fn t5_an_escalated_gate_is_resolved_and_its_bead_hears_ci_failed() {
    let (_, acts) = decide(&ingested(&["101"]), &busy());
    assert!(acts.iter().any(|a| a.effect == RESOLVE && a.key == "g1"), "{acts:?}");
    assert!(acts.iter().any(|a| a.effect == CI_FAILED && a.key == "ci.failed/sp-x/g1"), "{acts:?}");
}

/// `main.rs:112`: a gate with no blocked bead is not resolved.
#[test]
fn t5_a_gate_blocking_nothing_is_left() {
    let snap = Snapshot { gate_blocked: [("g1".to_string(), None)].into(), ..busy() };
    assert!(!effects(&decide(&ingested(&["101"]), &snap).1).contains(&RESOLVE));
}

fn after_baseline() -> TaskRig {
    let mut r = rig_with(1, kit::cycling(vec![busy(), busy()], 1));
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(after_baseline, registration, STREAM, EventKind::new(TICK, ""));
}
