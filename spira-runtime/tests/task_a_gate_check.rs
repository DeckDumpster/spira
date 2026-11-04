//! Acceptance T1–T6 for `gate.check`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/gate-check.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` and `record_ingested` are `unimplemented!`, and the row waits in
//! `tasks::a::STAGED`. T6 runs two virtual days (the contract asks seven); its idle budget is
//! the contract's "0 per idle hour".

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: gate_check,
    registration: GateCheck::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
    budget: 0,
}

fn run(id: &str, conclusion: &str) -> Run {
    Run { id: id.into(), repo: "rgantt/spira".into(), branch: "main".into(), conclusion: conclusion.into(), head_sha: format!("sha-{id}") }
}

/// The contract's idle fixture: no gates, no ESCALATE line, no flaky or red suite, every run
/// already ingested (the first answer is the baseline that ingests them).
fn idle() -> Snapshot {
    Snapshot { lc_open: Some(vec![]), ci: Ci { recent_runs: vec![run("101", "success")], last_green_sha: Some("sha-101".into()), ..Ci::default() }, flaky_gh_repo: "rgantt/spira".into(), ..Snapshot::default() }
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

#[test]
fn t3_a_growing_ingested_set_is_recorded_and_a_stale_write_is_refused() {
    let first = record_ingested(&Carry::default(), &ingested(&["101"])).expect("a first write records");
    let second = record_ingested(&first, &ingested(&["101", "102"])).expect("a superset records");
    assert_eq!(second, ingested(&["101", "102"]), "the read equals the last write");
    assert!(record_ingested(&second, &ingested(&["101"])).is_err(), "an older set is refused");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
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

/// `main.rs:166`: a red-twice suite whose open bead is already P1 is untouched.
#[test]
fn t5_a_red_twice_suite_already_p1_is_untouched() {
    let title = "red twice: lint".to_string();
    let snap = Snapshot {
        ci: Ci { recent_runs: vec![run("101", "failure"), run("102", "failure")], annotations_by_job: [("j1".to_string(), vec!["lint failed".to_string()]), ("j2".to_string(), vec!["lint failed".to_string()])].into(), ..idle().ci },
        beads_by_title: [(title, Bead { id: "sp-r".into(), priority: 1, open: true })].into(),
        ..idle()
    };
    let acts = decide(&ingested(&["101", "102"]), &snap).1;
    assert!(!effects(&acts).contains(&FILE_BEAD) && !effects(&acts).contains(&RAISE_PRIORITY), "{acts:?}");
}

/// `main.rs:186-190` (changed by the contract): a new run is ingested once, an old one never again.
#[test]
fn t5_a_new_run_is_ingested_once() {
    let snap = Snapshot { ci: Ci { recent_runs: vec![run("101", "success"), run("103", "success")], ..idle().ci }, ..idle() };
    let (carry, acts) = decide(&ingested(&["101"]), &snap);
    assert_eq!(acts.iter().filter(|a| a.effect == TSD_INGEST).map(|a| a.key.as_str()).collect::<Vec<_>>(), ["rgantt/spira/103"]);
    assert!(decide(&carry, &snap).1.iter().all(|a| a.effect != TSD_INGEST));
}

/// `main.rs:119-127`: unreadable lc rows file nothing.
#[test]
fn t5_unreadable_rows_file_nothing() {
    let snap = Snapshot { lc_open: None, ..busy() };
    assert!(!effects(&decide(&ingested(&["101"]), &snap).1).contains(&FILE_BEAD));
}
