//! T1–T6 for `sweep.nix_gc`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-nix-gc.md`. Red until the task is ported.

use runtime::taskrig::DAY_MS;
use runtime::units::{Exit, UnitHandle, UnitRan};
use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_nix_gc::{self as task, Carry, Phase, Prev, Snapshot, SweepNixGc as H};

const SPEC: &str = "weekly";
const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, SPEC), (task::JITTER_KEY, "3600")];
/// 1970-01-01 is a Thursday: the first Monday 00:00 is four days on.
const FIRST_MONDAY: u64 = 4 * DAY_MS;

fn ok() -> Snapshot {
    Snapshot { env_ok: true, prev: Prev::Ok, gc_bin: Some("/nix/var/nix/profiles/default/bin/nix-collect-garbage".into()) }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

fn ran(exit: i32) -> Input<Snapshot> {
    Input::Ran(UnitRan { handle: UnitHandle::of(task::NAME, "1970-W02").unwrap(), exit: Exit::Code(exit), duration_ms: 60_000 })
}

#[test]
fn t1_registered_boots_and_arms_next_monday() {
    t::t1_registered_and_boots::<H>(SET, FIRST_MONDAY);
}

#[test]
fn t2_six_days_without_a_tick_journal_nothing() {
    let mut rig = t::rig::<H>(1, SET, &json(&ok()));
    rig.advance(3 * DAY_MS).unwrap();
    assert!(rig.trace().is_empty() && rig.requested().is_empty(), "nothing before Monday: {:?}", rig.requested());
    rig.advance(3 * DAY_MS).unwrap();
    t::assert_ported(&rig, task::STREAM);
}

#[test]
fn t3_idle_collects_until_unit_ran_a_refusal_is_recorded_and_unit_ran_while_idle_is_refused() {
    let h = t::handler::<H>(SET);
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(ok()), FIRST_MONDAY).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Collecting, vec![task::COLLECT]));
    let (c, _) = task::decide(&h.cfg, &c, &ran(0), FIRST_MONDAY + 60_000).unwrap();
    assert_eq!(c.phase, Phase::Idle);
    let refused = task::decide(&h.cfg, &Carry::default(), &ran(0), FIRST_MONDAY).unwrap_err();
    assert!(refused.contains("idle"), "{refused}");
    let (c, _) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { env_ok: false, ..ok() }), FIRST_MONDAY).unwrap();
    assert_eq!(c.phase, Phase::Refused);
    assert!(c.refused.is_some());
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_nix_gc.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_three_refusals_and_the_happy_tree_match_the_scripts_exit_codes() {
    let h = t::handler::<H>(SET);
    for (snap, why) in [(Snapshot { env_ok: false, ..ok() }, "env"), (Snapshot { prev: Prev::Dangling, ..ok() }, "prev"), (Snapshot { gc_bin: None, ..ok() }, "binary")] {
        let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), FIRST_MONDAY).unwrap();
        assert_eq!(names(&acts), [task::INBOX_APPEND], "exit 1 ({why}): one inbox line, no collect");
    }
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(ok()), FIRST_MONDAY).unwrap();
    assert_eq!(names(&acts), [task::COLLECT], "exit 0");
}
