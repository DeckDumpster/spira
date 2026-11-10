//! T1–T6 for `mail.tidy`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/mail-tidy.md`. Red until the task is ported.

use std::collections::BTreeSet;

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::mail_tidy::{self as task, Carry, MailTidy as H, Msg, Phase, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "900"), (task::MAILBOX_KEY, "operator"), (task::FRESH_KEY, "86400"), ("SPIRA_ID_PREFIX", "sp")];
const TICK: u64 = 900_000;
const NOW: u64 = 1_793_664_000;

fn msg(path: &str, subject: &str, bead: Option<&str>, unread: bool, age: u64) -> Msg {
    Msg { path: path.into(), mtime: NOW - age, subject: subject.into(), bead: bead.map(str::to_string), unread, urgent: false, bead_closed: None }
}

/// One unread fresh message and one naming an open ask.
fn idle() -> Snapshot {
    Snapshot { store_ok: true, open_asks: BTreeSet::from(["sp-ask".to_string()]), messages: vec![msg("new/1", "fresh", None, true, 60), msg("cur/2", "about the ask", Some("sp-ask"), false, 30 * 86_400)], now: NOW, error: None }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_for_operator_boots_and_arms_a_15min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
    let rig = t::rig::<H>(1, SET, "{}");
    assert!(rig.requested().iter().all(|q| q.input == "operator"), "the probe names its mailbox");
}

#[test]
fn t2_a_fresh_unread_and_an_open_ask_message_are_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&idle()));
}

#[test]
fn t3_a_pass_decides_and_archives_and_a_refused_snapshot_cannot_archive() {
    let h = t::handler::<H>(SET);
    let old = Snapshot { messages: vec![msg("cur/3", "stale", None, false, 9 * 86_400)], ..idle() };
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(old), TICK).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Decided, vec![task::ARCHIVE]));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::ARCHIVE.into(), ok: true, body: "1".into() }, TICK).unwrap();
    assert_eq!(c.phase, Phase::Archived);
    let (refused, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { error: Some("ask query failed".into()), ..idle() }), TICK).unwrap();
    assert_eq!((refused.phase, acts.len()), (Phase::Refused, 0));
    let err = task::decide(&h.cfg, &refused, &Input::Outcome { effect: task::ARCHIVE.into(), ok: true, body: "1".into() }, TICK).unwrap_err();
    assert!(err.contains("refused"), "{err}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_no_bdq() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/mail_tidy.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_the_keep_rule_archives_duplicates_and_stale_mail_and_a_failed_control_archives_nothing() {
    let h = t::handler::<H>(SET);
    let snap = Snapshot { messages: vec![msg("new/1", "dup", None, true, 60), msg("cur/2", "dup", None, true, 120), msg("cur/3", "closed bead", Some("sp-done"), true, 60), msg("cur/4", "old read", None, false, 2 * 86_400)], ..idle() };
    let mut closed = snap.clone();
    closed.messages[2].bead_closed = Some(true);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(closed), TICK).unwrap();
    assert_eq!(names(&acts), [task::ARCHIVE], "one batched archive");
    for p in ["cur/2", "cur/3", "cur/4"] {
        assert!(acts[0].input.contains(p), "{p} is archived: {}", acts[0].input);
    }
    assert!(!acts[0].input.contains("new/1"), "the first copy of a fresh subject stays");
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { store_ok: false, open_asks: BTreeSet::new(), ..snap }), TICK).unwrap();
    assert!(acts.is_empty(), "empty asks and a failing positive control archive nothing: {acts:?}");
}

use runtime::taskrig::TaskRig;
use spira_runtime::tasks::a::kit;
use spira_runtime::tasks::b::kit::FIRE;

/// A stale read message whose archive the mailbox refused: the pass stays decided.
fn decided_and_unarchived() -> TaskRig {
    let world = json(&Snapshot { messages: vec![msg("cur/3", "stale", None, false, 9 * 86_400)], ..idle() });
    let mut r = TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().double(task::OBSERVE, move |_| Ok(world.clone())).double(task::ARCHIVE, |_| Err("mailbox locked".into()));
    let (_, stream, kind) = H::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&decided_and_unarchived(), task::NAME);
    assert!(rows.iter().any(|k| k == "phase"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(decided_and_unarchived, || t::handler::<H>(SET).registration(), task::STREAM, runtime::event::EventKind::new(task::TICK, FIRE));
}
