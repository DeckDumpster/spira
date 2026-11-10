//! T1–T6 for `reconcile.content`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/reconcile-content.md`. Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::b::reconcile_content::{self as task, Carry, Close, Phase, ReconcileContent as H, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "600"), (task::FULL_PASS_KEY, "21600")];
const TICK: u64 = 600_000;

fn close(id: &str, why: &str) -> Close {
    Close { id: id.into(), why: why.into(), reason: format!("lifecycle {why}") }
}

/// Every record non-terminal, no closed asks, no complete epics.
fn idle() -> Snapshot {
    Snapshot { through_seq: Some(40), ..Snapshot::default() }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_10min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_nothing_to_close_journals_nothing() {
    t::t2_silent::<H>(SET, &json(&idle()));
}

#[test]
fn t3_a_bead_goes_open_close_requested_closed_and_a_second_close_is_refused_by_name() {
    let h = t::handler::<H>(SET);
    let pending = Carry { through_seq: Some(40), pending: vec![close("sp-a", "LANDED")], ..Carry::default() };
    let (c, acts) = task::decide(&h.cfg, &pending, &Input::Outcome { effect: task::SKEW_CHECK.into(), ok: true, body: "ok".into() }, TICK).unwrap();
    assert_eq!((c.beads.get("sp-a").copied(), names(&acts)), (Some(Phase::CloseRequested), vec![task::CLOSE]));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::CLOSE.into(), ok: true, body: "sp-a".into() }, TICK).unwrap();
    assert_eq!(c.beads.get("sp-a"), Some(&Phase::Closed));
    let closed = Carry { beads: BTreeMap::from([("sp-a".to_string(), Phase::Closed)]), pending: vec![close("sp-a", "LANDED")], ..Carry::default() };
    let refused = task::decide(&h.cfg, &closed, &Input::Outcome { effect: task::SKEW_CHECK.into(), ok: true, body: "ok".into() }, TICK).unwrap_err();
    assert!(refused.contains("closed") && refused.contains("sp-a"), "{refused}");
    let (later, _) = task::decide(&h.cfg, &Carry { through_seq: Some(40), ..Carry::default() }, &Input::Observed(Snapshot { through_seq: Some(52), ..Snapshot::default() }), TICK).unwrap();
    assert!(later.through_seq >= Some(40), "the carried seq is monotone");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/reconcile_content.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_terminal_records_and_complete_epics_close_behind_the_skew_gate() {
    let h = t::handler::<H>(SET);
    let snap = Snapshot { through_seq: Some(41), work: vec![close("sp-a", "LANDED"), close("sp-b", "DROPPED")], epics: vec![close("sp-e", "children terminal")], ..Snapshot::default() };
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), TICK).unwrap();
    assert_eq!(names(&acts), [task::SKEW_CHECK], "the batch waits on the skew check");
    assert_eq!(c.pending.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["sp-a", "sp-b", "sp-e"]);
    let (_, acts) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::SKEW_CHECK.into(), ok: false, body: "exit 3".into() }, TICK).unwrap();
    assert!(acts.is_empty(), "a red skew check drops the batch: {acts:?}");
}

/// A store that keeps listing `sp-a` as closable, with the skew check answering `skew`.
fn listed_with(skew: Result<&'static str, &'static str>) -> runtime::taskrig::TaskRig {
    let listing = json(&Snapshot { through_seq: Some(41), work: vec![close("sp-a", "LANDED")], ..Snapshot::default() });
    let mut rig = runtime::taskrig::TaskRig::new(1, t::handler::<H>(SET).registration())
        .unwrap()
        .double(task::OBSERVE, move |_| Ok(listing.clone()))
        .double(task::SKEW_CHECK, move |_| skew.map(String::from).map_err(String::from))
        .double(task::CLOSE, |input| Ok(serde_json::from_str::<Close>(input).unwrap().id));
    let (_, stream, kind) = H::boot();
    rig.tick(stream, kind).unwrap();
    rig
}

fn closed() -> runtime::taskrig::TaskRig {
    listed_with(Ok("ok"))
}

fn skew_red() -> runtime::taskrig::TaskRig {
    listed_with(Err("exit 3"))
}

fn registration() -> runtime::event::Registration {
    t::handler::<H>(SET).registration()
}

fn tick() -> runtime::event::EventKind {
    runtime::event::EventKind::new(task::TICK, "")
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&closed(), task::NAME);
    assert!(rows.iter().any(|k| k == "beads/sp-a") && rows.iter().any(|k| k == "through_seq"), "{rows:?}");
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&skew_red(), task::NAME);
    assert!(rows.iter().any(|k| k == "skew_red_at"), "{rows:?}");
}

#[test]
fn a_boot_closes_no_listed_bead_a_second_time() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(closed, registration, task::STREAM, tick());
}

#[test]
fn a_boot_does_not_ask_the_skew_check_again_inside_its_red_window() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(skew_red, registration, task::STREAM, tick());
}
