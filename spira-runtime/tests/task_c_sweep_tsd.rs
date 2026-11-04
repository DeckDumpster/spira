//! T1–T6 for `sweep.tsd`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-tsd.md`. Red until the task is ported.

use std::collections::BTreeMap;

use runtime::taskrig::DAY_MS;
use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_tsd::{self as task, Carry, Family, Phase, Snapshot, SweepTsd as H};

const SPEC: &str = "*-*-* 04:20:00";
const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, SPEC), (task::KEEP_DAYS_KEY, "30"), (task::BY_FAMILY_KEY, "")];
const AT_0420: u64 = (4 * 60 + 20) * 60_000;

fn fam(name: &str, oldest_days_ago: u64, now: u64) -> Family {
    Family { family: name.into(), bytes: 1_000, oldest_ts: Some(now - oldest_days_ago * DAY_MS) }
}

/// Three families, all inside their bounds.
fn within(now: u64) -> Snapshot {
    Snapshot { families: vec![fam("cert-pass", 3, now), fam("escape", 10, now), fam("round-attribution", 20, now)], default_days: 30, overrides: BTreeMap::new() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_the_next_0420() {
    t::t1_registered_and_boots::<H>(SET, AT_0420);
}

#[test]
fn t2_three_families_inside_their_bounds_journal_nothing() {
    t::t2_silent::<H>(SET, &json(&within(40 * DAY_MS)));
}

#[test]
fn t3_idle_prunes_and_returns_and_a_prune_outcome_while_idle_is_refused() {
    let h = t::handler::<H>(SET);
    let now = 40 * DAY_MS;
    let old = Snapshot { families: vec![fam("cert-pass", 35, now)], ..within(now) };
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(old), now).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Pruning, vec![task::PRUNE]));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::PRUNE.into(), ok: true, body: "kept 10, dropped 4".into() }, now).unwrap();
    assert_eq!(c.phase, Phase::Idle);
    let refused = task::decide(&h.cfg, &Carry::default(), &Input::Outcome { effect: task::PRUNE.into(), ok: true, body: "kept 1, dropped 0".into() }, now).unwrap_err();
    assert!(refused.contains("idle"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../src/tasks/c/sweep_tsd.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_only_families_past_their_keep_days_are_pruned_as_dry_run_lists_them() {
    let h = t::handler::<H>(SET);
    let now = 400 * DAY_MS;
    let snap = Snapshot { families: vec![fam("cert-pass", 31, now), fam("escape", 29, now), fam("custom", 8, now)], default_days: 30, overrides: BTreeMap::from([("custom".to_string(), 7)]) };
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), now).unwrap();
    assert_eq!(names(&acts), [task::PRUNE, task::PRUNE]);
    assert!(acts[0].input.contains("cert-pass") && acts[1].input.contains("custom"), "{acts:?}");
}

#[test]
fn t6_two_virtual_days_fire_at_0420_each_day_deterministically() {
    let rig = t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&within(40 * DAY_MS))), 2, 1);
    assert_eq!(t::fired_at::<H>(&rig), t::elapses(SPEC, 0, 2 * DAY_MS));
}
