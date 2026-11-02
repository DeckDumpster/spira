
use serde_json::json;

use super::*;
use crate::sim::bench::Bench;

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
