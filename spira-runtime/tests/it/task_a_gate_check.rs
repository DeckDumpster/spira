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
    test_ids: [ #[test_id(ti186gt)] #[test_id(tbp7wir)] #[test_id(t5d44dm)] #[test_id(t04mv26)] #[test_id(tj7p1hl)] ],
}

/// The contract's idle fixture: no gates, no ESCALATE line.
fn idle() -> Snapshot {
    Snapshot::default()
}

/// A gh:run gate escalated, blocking a bead.
fn busy() -> Snapshot {
    Snapshot { gate_check_out: "ESCALATE g1 run 102 failed\n".into(), gate_blocked: [("g1".to_string(), Some("sp-x".to_string()))].into(), ..idle() }
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

fn ingested(ids: &[&str]) -> Carry {
    Carry { ingested: ids.iter().map(|s| s.to_string()).collect() }
}

/// `main.rs:100-127`: an escalated gate blocking a bead is resolved and its bead gets `ci.failed`.
#[test_id(tuvvt5a)]
#[test]
fn t5_an_escalated_gate_is_resolved_and_its_bead_hears_ci_failed() {
    let (_, acts) = decide(&ingested(&["101"]), &busy());
    assert!(acts.iter().any(|a| a.effect == RESOLVE && a.key == "g1"), "{acts:?}");
    assert!(acts.iter().any(|a| a.effect == CI_FAILED && a.key == "ci.failed/sp-x/g1"), "{acts:?}");
}

/// `main.rs:112`: a gate with no blocked bead is not resolved.
#[test_id(t0k3d3b)]
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

#[test_id(tp9ehiy)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(after_baseline, registration, STREAM, EventKind::new(TICK, ""));
}

/// A discover a previous release asked for and never heard back from.
#[test_id(t3wcjqd)]
#[test]
fn a_pre_upgrade_discover_row_settles_and_the_task_goes_on() {
    use runtime::journal::{EffectRow, Journal, Turn};
    use runtime::event::{EffectSpec, StreamKey};

    let mut r = rig(1, idle());
    let spec = EffectSpec { name: "gate.check.discover".into(), input: r#"{"repo":"spira","branch":"b"}"#.into(), deadline_ms: 10_000, key: "spira/b".into() };
    let row = EffectRow { task: NAME.into(), ekey: spec.key.clone(), request_id: "gate.check/pre-upgrade".into(), stream: StreamKey(STREAM.into()), spec, requested_at: 0, deadline_at: 10_000 };
    r.journal.commit(&Turn { effects: vec![row], ..Turn::default() }).unwrap();
    let mark = r.requested().len();
    r.reopen(registration()).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    assert!(r.journal.boot().unwrap().effects.is_empty(), "the old row is still open: {:?}", r.journal.boot().unwrap().effects);
    let asked: Vec<&str> = r.requested()[mark..].iter().map(|q| q.name.as_str()).collect();
    assert_eq!(asked, [OBSERVE], "the old discover is not run again and the next tick still probes");
    assert!(!EFFECTS.contains(&"gate.check.discover") && !kit::real_names().contains(&"gate.check.discover"), "discover is retired");
}
