
use serde_json::json;

use super::*;
use crate::sim::bench::Bench;
use spira_desired_state::intent::JudgementResult;

const CONC: &str = "knobs/RoundController/lane/concurrency";
fn lane(b: &Bench) {
    b.seed(
        "lanes/lane",
        json!({ "landing_ref": "base-0", "image_digest": "sha256:img-0", "harness_version": "h0" }),
    );
}

fn intent(id: &str, action: &str, target: &str, from: Value, to: Value) -> Value {
    json!({
        "id": id, "action": action, "target": target, "from": from, "to": to,
        "moves": "waiting_with_no_slot", "baseline": 1.0, "expected_effect": -1.0, "horizon": "1h",
        "addresses": ["SlotsSaturated"], "taken_at": "2027-01-15T08:00:00Z", "by": { "pass": "p-1", "lock_token": "5.0" },
    })
}

fn put(b: &Bench, id: &str, v: Value) -> String {
    let key = intent_key(NAME, "lane", id);
    b.seed_ttl(&key, v, spira_desired_state::intent::INTENT_TTL_SECS);
    key
}

fn knob(b: &Bench, key: &str) -> Option<Value> {
    b.store.get(key).unwrap().map(|i| i.value.data)
}

fn ack(b: &Bench, key: &str) -> Value {
    b.store.get(key).unwrap().unwrap().value.data["ack"]["result"].clone()
}

#[test]
fn a_stopped_repo_takes_no_step_and_its_intents_wait() {
    let b = Bench::new();
    lane(&b);
    let key = put(&b, "1", intent("1", CONCURRENCY, CONC, json!(2), json!(3)));
    b.stops.stop("lane", "ops", "drill", "manual");
    assert!(apply(&b.ports(), "lane").unwrap().is_empty());
    assert_eq!(ack(&b, &key), Value::Null);
    assert_eq!(knob(&b, CONC), None, "not even the knob is set");
}

#[test]
fn an_ack_reads_as_its_result_s_word() {
    assert_eq!(
        ack_word(&json!({ "ack": { "result": "applied" } })),
        "applied"
    );
    assert_eq!(
        ack_word(&json!({ "ack": { "result": "already-applied" } })),
        "already-applied"
    );
    assert_eq!(
        ack_word(&json!({ "ack": { "result": { "refused": { "reason": "stale" } } } })),
        "refused"
    );
}

/// The reconciler's own goodput pass, through the same spira-lc verbs against this bench's store.
struct ReconcilerLc<'a>(&'a Bench);

impl reconciler_engine::goodput_pass::Lc for ReconcilerLc<'_> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        crate::ports::Lc::call(self.0, args)
    }
}

fn goodput_pass(b: &Bench) -> reconciler_engine::pass_record::PassRecord {
    use reconciler_engine::goodput_pass::{run, Outcome as Pass, PassConfig};
    let cfg = PassConfig {
        holder: "test".into(),
        timeout_secs: 60,
        shadow: Vec::new(),
        halted: false,
    };
    match run(&ReconcilerLc(b), &b.clock, &cfg).unwrap() {
        Pass::Passed(p) => p.record,
        Pass::Standby { held_by } => panic!("the reconciler's lock was held by {held_by}"),
    }
}

#[test]
fn a_hold_the_reconciler_steps_on_the_section_is_acked_by_the_controller_s_pass_and_judged_fulfilled() {
    use crate::sim::bench::EPOCH_MS;
    let b = Bench::new();
    lane(&b);
    for (hash, member, tip) in [("h1", "sp-a", "a1"), ("h2", "sp-b", "b1")] {
        b.seed(
            &format!("verdicts/lane/{hash}"),
            json!({ "class": "red", "step": "unit", "attempts": 2, "last_attempt": EPOCH_MS }),
        );
        for n in 1..=2u64 {
            b.seed(
                &format!("verdicts/lane/{hash}/attempt/{n}"),
                json!({ "class": "red", "step": "unit", "initiated_by": "planner", "at": EPOCH_MS - (3 - n) * 600_000 }),
            );
        }
        b.seed(
            &format!("verdicts/lane/by-member/{member}/{tip}"),
            json!({ "hash": hash }),
        );
    }
    let p = b.ports();
    let only = [crate::intents::STEP, crate::section::STEP];
    let steps: Vec<crate::pass::Step> = crate::pass::STEPS
        .iter()
        .filter(|s| only.contains(&s.name))
        .copied()
        .collect();
    let controller = || crate::pass::pass_with(&p, &["lane".into()], &steps);

    controller();
    let first = goodput_pass(&b);
    let hold = "knobs/RoundController/lane/hold/sp-a/a1";
    let stepped: Vec<(&str, &str)> = first
        .steps
        .iter()
        .map(|s| (s.action.as_str(), s.target.as_str()))
        .collect();
    assert_eq!(stepped, [(HOLD_MEMBER, hold)], "refused: {:?}", first.refused);
    let key = first.steps[0].intent.clone().unwrap();

    controller();
    assert_eq!(ack(&b, &key), "applied");

    b.clock.advance(11 * 60_000);
    controller();
    let second = goodput_pass(&b);
    let judged: Vec<(&str, JudgementResult, f64, f64)> = second
        .judged
        .iter()
        .map(|j| (j.intent.as_str(), j.result, j.baseline, j.observed))
        .collect();
    assert_eq!(
        judged,
        [(key.as_str(), JudgementResult::Fulfilled, 2.0, 1.0)],
        "the held identity left repeat_identity_open"
    );
    let stored = b.store.get(&key).unwrap().unwrap().value.data;
    assert_eq!(stored["judgement"]["result"], json!("fulfilled"));
}
