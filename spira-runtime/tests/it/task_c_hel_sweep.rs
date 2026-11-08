//! T1–T6 for `hel.sweep` (a P2 machine on a calendar tick, per the corrections), from the
//! Acceptance section of `wiki/projects/spira/designs/tasks/hel-sweep.md`. Red until ported.

use runtime::taskrig::DAY_MS;
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
    let spent = Carry { phase: Phase::Acting, pass: Some(Pass { started: AT_35, repos_left: vec!["spira".into()], cursor: 0 }), ..Carry::default() };
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

#[test]
fn t6_two_virtual_days_fire_at_every_35_deterministically_within_2_events_an_hour() {
    let rig = t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&keep())), 2, 2);
    assert_eq!(t::fired_at::<H>(&rig), t::elapses(SPEC, 0, 2 * DAY_MS));
}
