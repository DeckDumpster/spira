//! T1–T6 for `mail.reconcile`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/mail-reconcile.md`. Red until the task is ported.

use std::collections::{BTreeMap, BTreeSet};

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::b::mail_reconcile::{self as task, Carry, MailReconcile as H, Orig, Reply, Snapshot};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "60")];
const MIN: u64 = 60_000;

fn reply(key: &str, kind: &str, done: &[&str]) -> Reply {
    Reply {
        key: key.into(),
        in_reply_to: format!("<orig-{key}@spira>"),
        actor: "ryan".into(),
        para: "yes, go".into(),
        orig: Some(Orig { bead: "sp-q1".into(), kind: kind.into(), work_bead: String::new(), subject: "decide X".into() }),
        done: done.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        fails: BTreeMap::new(),
    }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_60s_tick() {
    t::t1_registered_and_boots::<H>(SET, MIN);
}

#[test]
fn t2_no_pending_reply_is_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&Snapshot::default()));
}

#[test]
fn t3_a_reply_steps_to_complete_once_and_a_second_complete_is_refused() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "notice", &[])] }), MIN).unwrap();
    assert_eq!(names(&acts), [task::NOTE]);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Outcome { effect: task::NOTE.into(), ok: true, body: "m1/note-orig".into() }, MIN).unwrap();
    assert_eq!(names(&acts), [task::MARK], "a step that succeeded is marked in the ledger");
    let refused = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "notice", &["note-orig", "complete"])] }), MIN).unwrap_err();
    assert!(refused.contains("complete"), "{refused}");
    let mut abandoned = reply("m2", "notice", &[]);
    abandoned.fails.insert("note-orig".into(), task::MAX_ATTEMPTS);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![abandoned] }), MIN).unwrap();
    assert_eq!(names(&acts), [task::INBOX], "an abandoned step is said once");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_neither_bdq_nor_spira_lc() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../src/tasks/b/mail_reconcile.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_question_closes_and_answers_and_a_second_pass_is_silent() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "question", &[])] }), MIN).unwrap();
    assert_eq!(names(&acts), [task::CLOSE, task::ANSWER], "close the tracking bead, then close-ask --exit answered");
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "question", &["close", "answer"])] }), 2 * MIN).unwrap();
    assert!(acts.is_empty(), "the ledger makes the second pass silent: {acts:?}");
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_60_events_an_hour() {
    let answers = vec![json(&Snapshot::default()), json(&Snapshot { replies: vec![reply("m1", "notice", &["note-orig", "complete"])] })];
    t::t6_deterministic::<H>(|seed| t::rig_cycle::<H>(seed, SET, answers.clone()), 2, 60);
}
