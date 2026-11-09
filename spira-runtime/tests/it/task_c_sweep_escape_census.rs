//! T1–T6 for `sweep.escape_census`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-escape-census.md`. Red until the task is ported.
// RETIRE-CANDIDATE: no reader of the census was found; these tests go with the task if retired.

use std::collections::BTreeMap;

use runtime::taskrig::DAY_MS;
use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_escape_census::{self as task, Carry, Phase, Snapshot, SweepEscapeCensus as H};

const SPEC: &str = "Mon *-*-* 06:15:00";
const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, SPEC)];
/// 1970-01-01 is a Thursday: the first Monday 06:15 is four days and 6 h 15 min on.
const FIRST: u64 = 4 * DAY_MS + (6 * 60 + 15) * 60_000;

fn counts(pairs: &[(&str, u64)]) -> Snapshot {
    Snapshot { counts: pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect::<BTreeMap<_, _>>(), hours: 168 }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_next_monday_0615() {
    t::t1_registered_and_boots::<H>(SET, FIRST);
}

#[test]
fn t2_six_days_without_a_tick_journal_nothing() {
    let mut rig = t::rig::<H>(1, SET, &json(&counts(&[])));
    rig.advance(4 * DAY_MS).unwrap();
    assert!(rig.trace().is_empty() && rig.requested().is_empty(), "nothing before Monday 06:15: {:?}", rig.requested());
    rig.advance(2 * DAY_MS).unwrap();
    t::assert_ported(&rig, task::STREAM);
}

#[test]
fn t3_idle_reports_its_week_and_a_second_report_that_week_is_refused() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(counts(&[("flake", 1)])), FIRST).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Reported, vec![task::REPORT]));
    assert_eq!(c.week.as_deref(), Some("1970-W02"));
    let refused = task::decide(&h.cfg, &c, &Input::Observed(counts(&[("flake", 1)])), FIRST + 3_600_000).unwrap_err();
    assert!(refused.contains("reported"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_neither_duckdb_nor_tsd_query() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_escape_census.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_the_line_matches_escape_classify_census_for_empty_flake_only_and_all_four() {
    let h = t::handler::<H>(SET);
    for (snap, line) in [
        (counts(&[]), "0 mapping_gap, 0 gate_gap, 0 environment_gap, 0 flake"),
        (counts(&[("flake", 2)]), "0 mapping_gap, 0 gate_gap, 0 environment_gap, 2 flake"),
        (counts(&[("flake", 4), ("gate_gap", 2), ("mapping_gap", 1), ("environment_gap", 3)]), "1 mapping_gap, 2 gate_gap, 3 environment_gap, 4 flake"),
    ] {
        let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), FIRST).unwrap();
        assert_eq!(names(&acts), [task::REPORT]);
        assert!(acts[0].input.contains(line), "{} lacks {line}", acts[0].input);
    }
}
