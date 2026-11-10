//! T1–T6 for `asks.verify`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/asks-verify.md`. Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::asks_verify::{self as task, Ask, AsksVerify as H, Carry, CheckOutcome, Phase, Snapshot};
use spira_runtime::tasks::b::kit::{names, testing as t, Input};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "900"), ("SPIRA_VERIFY_TIMEOUT", "120")];
const TICK: u64 = 900_000;

fn ask(id: &str, cmd: &str, works: &[&str]) -> Ask {
    Ask { id: id.into(), works: works.iter().map(|s| s.to_string()).collect(), cmd: cmd.into() }
}

fn checked(id: &str, rc: i32) -> Input<Snapshot> {
    Input::Outcome { effect: task::CHECK.into(), ok: true, body: serde_json::to_string(&CheckOutcome { id: id.into(), rc, output: "ok".into() }).unwrap() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_15min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_two_open_asks_without_verify_are_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&Snapshot::default()));
}

#[test]
fn t3_an_ask_is_checked_satisfied_closed_withdrawn_and_a_close_without_satisfied_is_refused() {
    let h = t::handler::<H>(SET);
    let snap = Snapshot { asks: vec![ask("sp-a", "test -f /done", &["sp-w"])], work_states: BTreeMap::from([("sp-w".into(), "SUBMITTED".into())]), ..Snapshot::default() };
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), TICK).unwrap();
    assert_eq!((c.asks.get("sp-a").copied(), names(&acts)), (Some(Phase::Checking), vec![task::CHECK]));
    let (c, acts) = task::decide(&h.cfg, &c, &checked("sp-a", 0), TICK).unwrap();
    assert_eq!((c.asks.get("sp-a").copied(), names(&acts)), (Some(Phase::Satisfied), vec![task::CLOSE]));
    let (c, acts) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::CLOSE.into(), ok: true, body: "sp-a".into() }, TICK).unwrap();
    assert_eq!((c.asks.get("sp-a").copied(), names(&acts)), (Some(Phase::Closed), vec![task::WITHDRAW]));
    let checking = Carry { asks: BTreeMap::from([("sp-a".to_string(), Phase::Checking)]), ..Carry::default() };
    let refused = task::decide(&h.cfg, &checking, &Input::Outcome { effect: task::CLOSE.into(), ok: true, body: "sp-a".into() }, TICK).unwrap_err();
    assert!(refused.contains("checking"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_handler_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/asks_verify.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_satisfied_ask_closes_with_the_old_reason_and_a_mislabelled_epic_is_said_once() {
    let h = t::handler::<H>(SET);
    let c = Carry { asks: BTreeMap::from([("sp-a".to_string(), Phase::Checking)]), ..Carry::default() };
    let (_, acts) = task::decide(&h.cfg, &c, &checked("sp-a", 0), TICK).unwrap();
    assert!(acts[0].input.contains("Verified already done") && acts[0].input.contains("exit 0"), "{acts:?}");
    let snap = Snapshot { asks: vec![], mislabelled: vec![("sp-e".into(), "epic".into(), "an epic with the ask label".into())], ..Snapshot::default() };
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap.clone()), TICK).unwrap();
    assert_eq!(names(&acts), [task::NOTE]);
    let (_, acts) = task::decide(&h.cfg, &c, &Input::Observed(snap), 2 * TICK).unwrap();
    assert!(acts.is_empty(), "an unchanged mislabelled set is silent: {acts:?}");
}

fn mislabelled_epic() -> Snapshot {
    Snapshot { asks: vec![ask("sp-a", "test -f /done", &["sp-w"])], mislabelled: vec![("sp-e".into(), "epic".into(), "an epic with the ask label".into())], ..Snapshot::default() }
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rig = t::rig::<H>(1, SET, &json(&mislabelled_epic()));
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig, task::NAME);
    for want in ["mislabelled_seen", "asks/sp-a", "checks/sp-a"] {
        assert!(rows.iter().any(|k| k == want), "{want} in {rows:?}");
    }
}

#[test]
fn a_boot_repeats_no_mislabelled_note_and_misses_none() {
    let both = Snapshot { mislabelled: vec![("sp-e".into(), "epic".into(), "an epic with the ask label".into()), ("sp-f".into(), "epic".into(), "another".into())], ..mislabelled_epic() };
    t::assert_boot_matches_an_unstopped_run::<H>(SET, vec![json(&mislabelled_epic()), json(&both)], |_| {});
}
