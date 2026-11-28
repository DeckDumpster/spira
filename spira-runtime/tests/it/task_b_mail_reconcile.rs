//! T1–T6 for `mail.reconcile`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/mail-reconcile.md`. Red until the task is ported.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::taskrig::TaskRig;

use serde_json::{json, Value};
use spira_runtime::tasks::b::kit::{names, testing as t, Input, Stub};
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

#[test_id(tasziqz)]
#[test]
fn t1_registered_boots_and_arms_a_60s_tick() {
    t::t1_registered_and_boots::<H>(SET, MIN);
}

#[test_id(tu2u3e9)]
#[test]
fn t2_no_pending_reply_is_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&Snapshot::default()));
}

#[test_id(tf3f86c)]
#[test]
fn t3_a_reply_steps_to_complete_once_and_a_second_complete_is_refused() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "notice", &[])], ..Default::default() }), MIN).unwrap();
    assert_eq!(names(&acts), [task::NOTE]);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Outcome { effect: task::NOTE.into(), ok: true, body: "m1/note-orig".into() }, MIN).unwrap();
    assert_eq!(names(&acts), [task::MARK], "a step that succeeded is marked in the ledger");
    let refused = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "notice", &["note-orig", "complete"])], ..Default::default() }), MIN).unwrap_err();
    assert!(refused.contains("complete"), "{refused}");
    let mut abandoned = reply("m2", "notice", &[]);
    abandoned.fails.insert("note-orig".into(), task::MAX_ATTEMPTS);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![abandoned], ..Default::default() }), MIN).unwrap();
    assert_eq!(names(&acts), [task::INBOX], "an abandoned step is said once");
}

#[test_id(tted4yl)]
#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_neither_bdq_nor_spira_lc() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/b/mail_reconcile.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test_id(txsp87s)]
#[test]
fn t5_a_question_closes_and_answers_and_a_second_pass_is_silent() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "question", &[])], ..Default::default() }), MIN).unwrap();
    assert_eq!(names(&acts), [task::CLOSE, task::ANSWER], "close the tracking bead, then close-ask --exit answered");
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { replies: vec![reply("m1", "question", &["close", "answer"])], ..Default::default() }), 2 * MIN).unwrap();
    assert!(acts.is_empty(), "the ledger makes the second pass silent: {acts:?}");
}

fn marked(cfg: &spira_runtime::tasks::b::kit::Cfg, effect: &str, ok: bool, body: &str) -> String {
    let (_, acts) = task::decide(cfg, &Carry::default(), &Input::Outcome { effect: effect.into(), ok, body: body.into() }, MIN).unwrap();
    assert_eq!(names(&acts), [task::MARK]);
    serde_json::from_str::<task::Job>(&acts[0].input).unwrap().step
}

#[test_id(t3mc0o8)]
#[test]
fn a_close_refusal_is_the_runners_typed_answer_not_text_in_the_failure() {
    let h = t::handler::<H>(SET);
    assert_eq!(marked(&h.cfg, task::CLOSE, true, "m1/close:refused"), "close:refused");
    assert_eq!(marked(&h.cfg, task::CLOSE, false, "m1/close: spira-lc exited 3: a bead title"), "fail:close", "the words 'exited 3' decide nothing");
}

#[test_id(tqq8sxu)]
#[test]
fn it_keeps_no_carry_row_however_many_replies_it_has_acted_on() {
    let rig = t::rig::<H>(1, SET, &json(&Snapshot { replies: vec![reply("m1", "question", &[])], ..Default::default() }));
    t::assert_ported(&rig, task::STREAM);
    let kept: Vec<_> = rig.journal.state().into_iter().filter(|((task_name, _), v)| task_name == task::NAME && !v.is_empty()).collect();
    assert!(kept.is_empty(), "{kept:?}");
}

#[test_id(tob86vu)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    t::assert_boot_matches_an_unstopped_run::<H>(SET, vec![json(&Snapshot { replies: vec![reply("m1", "question", &[])], ..Default::default() })], |_| {});
}

#[test_id(t0uhhvw)]
#[test]
fn unread_mail_is_set_once_and_cleared_when_read_and_a_failed_publish_is_said_again() {
    let h = t::handler::<H>(SET);
    let one = json!({"from": "a", "subject": "s", "kind": "note", "at": 1});
    let unread = |ids: &[&str]| Input::Observed(Snapshot { unread: BTreeMap::from([("concierge".to_string(), ids.iter().map(|id| (id.to_string(), one.clone())).collect())]), ..Default::default() });
    let change = |acts: &[spira_runtime::tasks::b::kit::Act]| {
        assert_eq!(names(acts), [task::PUBLISH]);
        serde_json::from_str::<Value>(&acts[0].input).unwrap()
    };

    let (carry, acts) = task::decide(&h.cfg, &Carry::default(), &unread(&["m1"]), MIN).unwrap();
    assert_eq!(change(&acts), json!({"prefix": "mail/concierge", "set": {"m1": one}, "clear": [], "seq": MIN}));
    let (same, quiet) = task::decide(&h.cfg, &carry, &unread(&["m1"]), 2 * MIN).unwrap();
    assert!(quiet.is_empty() && same == carry, "{quiet:?}");

    let (read, acts) = task::decide(&h.cfg, &carry, &unread(&[]), 3 * MIN).unwrap();
    assert_eq!(change(&acts), json!({"prefix": "mail/concierge", "set": {}, "clear": ["m1"], "seq": 3 * MIN}));
    assert!(read.published.is_empty(), "{read:?}");

    let failed = Input::Outcome { effect: task::PUBLISH.into(), ok: false, body: acts[0].input.clone() };
    let (owed, quiet) = task::decide(&h.cfg, &read, &failed, 4 * MIN).unwrap();
    assert!(quiet.is_empty(), "{quiet:?}");
    let (_, acts) = task::decide(&h.cfg, &owed, &unread(&[]), 5 * MIN).unwrap();
    assert_eq!(change(&acts)["clear"], json!(["m1"]), "a clear that did not land is sent again");
}

#[test_id(t772ugl)]
#[test]
fn a_publish_the_engine_saw_fail_is_asked_for_again_by_the_next_pass() {
    let m1 = BTreeMap::from([("concierge".to_string(), BTreeMap::from([("m1".to_string(), json!({"from": "a"}))]))]);
    let answers = [json(&Snapshot { unread: m1, ..Default::default() }), json(&Snapshot::default()), json(&Snapshot::default())];
    let asked = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = asked.clone();
    let passes = AtomicUsize::new(0);
    // The second publish, the clear that empties the mailbox, fails.
    let mut rig = TaskRig::new(1, t::handler::<H>(SET).registration())
        .unwrap()
        .double(task::OBSERVE, move |_| Ok(answers[passes.fetch_add(1, Ordering::SeqCst).min(answers.len() - 1)].clone()))
        .double(task::PUBLISH, move |input| {
            let mut seen = seen.lock().unwrap();
            seen.push(serde_json::from_str(input).unwrap());
            if seen.len() == 2 { Err("lost".into()) } else { Ok(String::new()) }
        });
    let (_, stream, boot) = H::boot();
    rig.tick(stream, boot).unwrap();
    for _ in 0..2 {
        rig.advance(MIN).unwrap();
    }
    t::assert_ported(&rig, task::STREAM);
    let clears: Vec<Value> = asked.lock().unwrap().iter().skip(1).map(|c| c["clear"].clone()).collect();
    assert_eq!(clears, [json!(["m1"]), json!(["m1"])]);
}

#[test_id(t29cm94)]
#[test]
fn a_backlog_is_published_and_carried_in_pieces_the_store_takes() {
    const STORE_COLUMN: usize = 65_535;
    let h = t::handler::<H>(SET);
    let ids: BTreeSet<String> = (0..1000).map(|n| format!("100.{n:04}.1")).collect();
    let message = json!({"from": "a", "subject": "s".repeat(120), "kind": "note", "at": 1});
    let unread = BTreeMap::from([("concierge".to_string(), ids.iter().map(|id| (id.clone(), message.clone())).collect())]);
    let (carry, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { unread, ..Default::default() }), MIN).unwrap();

    assert!(acts.iter().all(|a| a.input.len() < STORE_COLUMN), "{:?}", acts.iter().map(|a| a.input.len()).collect::<Vec<_>>());
    assert_eq!(acts.iter().map(|a| &a.key).collect::<BTreeSet<_>>().len(), acts.len());
    let set: BTreeSet<String> = acts.iter().flat_map(|a| serde_json::from_str::<Value>(&a.input).unwrap()["set"].as_object().unwrap().keys().cloned().collect::<Vec<_>>()).collect();
    assert_eq!(set, ids);

    let rows = runtime::rows::split(&serde_json::to_string(&carry).unwrap(), <H as Stub>::ROWS);
    assert!(rows.values().all(|row| row.len() < STORE_COLUMN), "{:?}", rows.values().map(String::len).max());
    let joined: Carry = serde_json::from_str(&runtime::rows::join(&rows, <H as Stub>::ROWS)).unwrap();
    assert_eq!(joined, carry);
}

#[test_id(ttsnoih)]
#[test]
fn a_step_lost_at_a_restart_is_not_marked_failed_and_the_next_observe_asks_it_again() {
    let observed = json(&Snapshot { replies: vec![reply("m1", "notice", &[])] });
    let mut rig = runtime::taskrig::TaskRig::new(1, t::handler::<H>(SET).registration()).unwrap().hanging(task::NOTE).double(task::OBSERVE, move |_| Ok(observed.clone()));
    let (_, stream, kind) = H::boot();
    rig.tick(stream, kind).unwrap();
    rig.reopen(t::handler::<H>(SET).registration()).unwrap();
    rig.run_until_idle().unwrap();
    assert!(!rig.requested().iter().any(|q| q.name == task::MARK), "{:?}", rig.requested());
    rig.tick(task::STREAM, runtime::event::EventKind::new(task::TICK, "")).unwrap();
    assert_eq!(rig.requested().iter().filter(|q| q.name == task::NOTE).count(), 2, "{:?}", rig.requested());
    assert!(!rig.wide_set("mail.reconcile.refused"));
}

#[test_id(t7u586q)]
#[test]
fn decide_marks_nothing_for_a_lost_step_and_still_marks_a_failed_one() {
    let h = t::handler::<H>(SET);
    for effect in [task::NOTE, task::CLOSE, task::LIFT, task::ANSWER, task::INBOX, task::MARK] {
        let step = Input::Lost(spira_runtime::tasks::b::kit::Act { name: effect.into(), key: "m1/step".into(), input: "{}".into(), deadline_ms: 1_000, probe: false });
        assert_eq!(task::decide(&h.cfg, &Carry::default(), &step, MIN).unwrap(), (Carry::default(), vec![]), "{effect}");
    }
    assert_eq!(marked(&h.cfg, task::NOTE, false, "m1/note-orig: why"), "fail:note-orig");
}

#[test_id(tfv0a2r)]
#[test]
fn decide_marks_a_lost_publish_unknown_as_it_does_a_failed_one() {
    let h = t::handler::<H>(SET);
    let input = serde_json::json!({"prefix": "mail/operator", "set": {"m1": {"subject": "s"}}, "clear": ["m0"], "seq": 1}).to_string();
    let lost = Input::Lost(spira_runtime::tasks::b::kit::Act { name: task::PUBLISH.into(), key: "k".into(), input: input.clone(), deadline_ms: 1_000, probe: false });
    let (next, acts) = task::decide(&h.cfg, &Carry::default(), &lost, MIN).unwrap();
    assert_eq!(next.published, BTreeMap::from([("operator/m0".to_string(), serde_json::Value::Null), ("operator/m1".to_string(), serde_json::Value::Null)]));
    assert!(acts.is_empty(), "{acts:?}");
    let failed = Input::Outcome { effect: task::PUBLISH.into(), ok: false, body: input };
    assert_eq!(task::decide(&h.cfg, &Carry::default(), &failed, MIN).unwrap(), (next, acts));
}
