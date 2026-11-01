use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::outcome::Kind;
use crate::ports::Lc;
use crate::real::Unserved;
use crate::sim::bench::Bench;

const CONC: &str = "knobs/RoundController/lane/concurrency";
const HOLD: &str = "knobs/RoundController/lane/hold/sp-a/a1";

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

fn of(v: &Value) -> Intent {
    parse_intent(v).unwrap()
}

fn knob(b: &Bench, key: &str) -> Option<Value> {
    b.store.get(key).unwrap().map(|i| i.value.data)
}

fn ack(b: &Bench, key: &str) -> Value {
    b.store.get(key).unwrap().unwrap().value.data["ack"]["result"].clone()
}

#[test]
fn a_concurrency_step_is_one_level_on_this_repo_s_knob_within_the_slot_budget() {
    let b = Bench::new();
    let p = b.ports();
    let d = |from: Value, to: Value, target: &str| {
        decide(&p, "lane", &of(&intent("1", CONCURRENCY, target, from, to)))
    };
    assert_eq!(d(json!(2), json!(3), CONC), Decision::Apply);
    assert_eq!(d(json!(3), json!(2), CONC), Decision::Apply);
    let refused = |r: Decision| match r {
        Decision::Refuse(why) => why,
        Decision::Apply => panic!("applied"),
    };
    assert!(refused(d(
        json!(2),
        json!(3),
        "knobs/RoundController/other/concurrency"
    ))
    .contains("not this repo's concurrency knob"));
    assert!(refused(d(json!(2), json!("3"), CONC)).contains("not one whole stack depth"));
    assert!(refused(d(json!(2.5), json!(3), CONC)).contains("not one whole stack depth"));
    assert_eq!(
        refused(d(json!(2), json!(4), CONC)),
        "a concurrency step is ±1, not 2 → 4"
    );
    assert_eq!(
        refused(d(json!(1), json!(0), CONC)),
        "the stack depth is at least 1"
    );
    assert_eq!(
        refused(d(json!(4), json!(5), CONC)),
        "5 is beyond the slot budget of 4",
        "one Proxmox slot and three on EC2"
    );
}

#[test]
fn a_hold_is_made_from_null_on_one_member_at_one_tip_and_names_who_lifts_it() {
    let b = Bench::new();
    let p = b.ports();
    let to = json!({ "lifted_by": "reconciler" });
    let d = |from: Value, to: Value, target: &str| {
        decide(&p, "lane", &of(&intent("1", HOLD_MEMBER, target, from, to)))
    };
    assert_eq!(d(Value::Null, to.clone(), HOLD), Decision::Apply);
    for (from, to2, target, why) in [
        (
            Value::Null,
            to.clone(),
            "knobs/RoundController/lane/hold/sp-a",
            "not a hold on one member",
        ),
        (json!(false), to.clone(), HOLD, "created from null"),
        (
            Value::Null,
            json!({ "lifted_by": "" }),
            HOLD,
            "names who lifts it",
        ),
        (Value::Null, json!(true), HOLD, "names who lifts it"),
    ] {
        match d(from, to2, target) {
            Decision::Refuse(r) => assert!(r.contains(why), "{r}"),
            Decision::Apply => panic!("applied {target}"),
        }
    }
    assert_eq!(
        decide(
            &p,
            "lane",
            &of(&intent("1", "drain", CONC, json!(1), json!(2)))
        ),
        Decision::Refuse("\"drain\" is not an action RoundController declares".into())
    );
}

#[test]
fn each_intent_is_applied_or_refused_once_and_acked_in_the_same_txn() {
    let b = Bench::new();
    lane(&b);
    let p = b.ports();
    let up = put(&b, "1", intent("1", CONCURRENCY, CONC, json!(2), json!(3)));
    let wide = put(&b, "2", intent("2", CONCURRENCY, CONC, json!(3), json!(5)));
    let hold = put(
        &b,
        "3",
        intent(
            "3",
            HOLD_MEMBER,
            HOLD,
            Value::Null,
            json!({ "lifted_by": "reconciler" }),
        ),
    );
    let r = apply(&p, "lane").unwrap();
    assert_eq!(
        r,
        [
            (up.clone(), "applied"),
            (wide.clone(), "refused"),
            (hold.clone(), "applied")
        ]
    );
    assert_eq!(
        knob(&b, CONC),
        Some(json!(3)),
        "set at the lane's depth 2, then stepped once"
    );
    assert_eq!(knob(&b, HOLD), Some(json!({ "lifted_by": "reconciler" })));
    assert_eq!(
        ack(&b, &wide),
        json!({ "refused": { "reason": "a concurrency step is ±1, not 3 → 5" } })
    );
    assert_eq!(
        b.log
            .outcomes()
            .iter()
            .filter(|o| o.kind == Kind::Acked)
            .count(),
        3
    );

    let again = put(&b, "4", intent("4", CONCURRENCY, CONC, json!(2), json!(3)));
    let stale = put(&b, "5", intent("5", CONCURRENCY, CONC, json!(1), json!(2)));
    let r = apply(&p, "lane").unwrap();
    assert_eq!(
        r,
        [
            (again.clone(), "already-applied"),
            (stale.clone(), "refused")
        ],
        "acked intents are not taken again"
    );
    assert_eq!(
        ack(&b, &stale),
        json!({ "refused": { "reason": "stale", "stale": 3 } })
    );
    assert_eq!(knob(&b, CONC), Some(json!(3)));
    assert!(apply(&p, "lane").unwrap().is_empty());
}

#[test]
fn a_stopped_repo_takes_no_step_and_its_intents_wait() {
    let b = Bench::new();
    lane(&b);
    let key = put(&b, "1", intent("1", CONCURRENCY, CONC, json!(2), json!(3)));
    b.switches.stop("lane", "stopped by ops: drill");
    assert!(apply(&b.ports(), "lane").unwrap().is_empty());
    assert_eq!(ack(&b, &key), Value::Null);
    assert_eq!(knob(&b, CONC), None, "not even the knob is set");
}

#[test]
fn the_concurrency_knob_is_set_once_at_the_lane_s_depth_and_only_for_a_repo_with_a_lane() {
    let b = Bench::new();
    assert!(apply(&b.ports(), "lane").unwrap().is_empty());
    assert_eq!(knob(&b, CONC), None);
    b.seed(
        "lanes/lane",
        json!({ "landing_ref": "base-0", "image_digest": "d", "harness_version": "h", "concurrency": 3 }),
    );
    apply(&b.ports(), "lane").unwrap();
    assert_eq!(knob(&b, CONC), Some(json!(3)));
    let calls = crate::lock(&b.lc_calls).len();
    apply(&b.ports(), "lane").unwrap();
    assert_eq!(
        crate::lock(&b.lc_calls).len(),
        calls,
        "a set knob is read, not set again"
    );
}

#[test]
fn an_intent_that_does_not_decode_or_cannot_be_acked_is_the_step_s_refusal_and_the_rest_go_on() {
    let b = Bench::new();
    lane(&b);
    put(&b, "0", json!({ "id": "0" }));
    let good = put(&b, "1", intent("1", CONCURRENCY, CONC, json!(2), json!(3)));
    b.seed_ttl(
        &format!("{}/x", intent_key(NAME, "lane", "1")),
        json!("not an intent"),
        60,
    );
    let e = apply(&b.ports(), "lane").unwrap_err();
    assert_eq!(e.word(), "unreadable");
    assert!(
        e.to_string().contains("intents/RoundController/lane/0"),
        "{e}"
    );
    assert_eq!(
        ack(&b, &good),
        "applied",
        "the decodable one was still taken"
    );

    step(&b.ports(), "lane");
    step(&b.ports(), "lane");
    let declined = b
        .log
        .outcomes()
        .iter()
        .filter(|o| o.kind == Kind::Declined)
        .count();
    assert_eq!(declined, 1, "a standing refusal is told once");
}

#[test]
fn a_door_that_refuses_or_cannot_tell_is_answered_as_such() {
    let b = Bench::new();
    lane(&b);
    b.seed(CONC, json!(2));
    put(&b, "1", intent("1", CONCURRENCY, CONC, json!(2), json!(3)));
    let p = Ports {
        lc: &Unserved,
        ..b.ports()
    };
    assert_eq!(apply(&p, "lane").unwrap_err().word(), "unreadable");

    struct Refusing(Mutex<u32>);
    impl Lc for Refusing {
        fn call(&self, _: &[String]) -> (i32, Value) {
            *crate::lock(&self.0) += 1;
            (
                LC_REFUSED,
                json!({ "refused": { "field": "ack", "rule": "is already acknowledged", "code": "once" } }),
            )
        }
    }
    let refusing = Refusing(Mutex::new(0));
    let p = Ports {
        lc: &refusing,
        ..b.ports()
    };
    assert_eq!(
        apply(&p, "lane").unwrap_err(),
        Refusal::Admission {
            key: intent_key(NAME, "lane", "1"),
            field: "ack".into(),
            rule: "is already acknowledged".into(),
        }
    );

    let fresh = Bench::new();
    lane(&fresh);
    let p = Ports {
        lc: &refusing,
        ..fresh.ports()
    };
    let e = apply(&p, "lane").unwrap_err();
    assert_eq!(
        (e.word(), e.key()),
        ("admission-refused", format!("{CONC}#ack")),
        "the knob's first value was refused"
    );
    let u = Bench::unserved();
    assert_eq!(apply(&u.ports(), "lane").unwrap_err().word(), "unreadable");
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

/// The reconciler's own pass, through the same spira-lc verbs against this bench's store.
struct ReconcilerLc<'a>(&'a Bench);

impl reconciler_engine::goodput_pass::Lc for ReconcilerLc<'_> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        Lc::call(self.0, args)
    }
}

fn red_repeated(b: &Bench, hash: &str, member: &str, tip: &str) {
    use crate::sim::bench::EPOCH_MS;
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

#[test]
fn a_reconciler_pass_over_an_open_repeat_identity_writes_a_hold_this_applier_applies() {
    use reconciler_engine::goodput_pass::{run, Outcome as Pass, PassConfig};
    let b = Bench::new();
    lane(&b);
    red_repeated(&b, "h1", "sp-a", "a1");
    red_repeated(&b, "h2", "sp-b", "b1");
    let p = b.ports();
    crate::section::publish(&p, "lane").unwrap();

    let cfg = PassConfig {
        holder: "test".into(),
        timeout_secs: 60,
        shadow: Vec::new(),
        halted: false,
    };
    let Pass::Passed(pass) = run(&ReconcilerLc(&b), &b.clock, &cfg).unwrap() else {
        panic!("the reconciler's lock was free");
    };
    let steps: Vec<(&str, &str)> = pass
        .record
        .steps
        .iter()
        .map(|s| (s.action.as_str(), s.target.as_str()))
        .collect();
    assert_eq!(
        steps,
        [(HOLD_MEMBER, HOLD)],
        "refused: {:?}",
        pass.record.refused
    );
    let key = pass.record.steps[0].intent.clone().unwrap();

    assert_eq!(apply(&p, "lane").unwrap(), [(key.clone(), "applied")]);
    assert_eq!(ack(&b, &key), "applied");
    assert_eq!(knob(&b, HOLD), Some(json!({ "lifted_by": "reconciler" })));
    let held = crate::knobs::held(&p, "lane").unwrap();
    assert_eq!(
        held.into_iter().map(|m| format!("{}@{}", m.id, m.tip)).collect::<Vec<_>>(),
        ["sp-a@a1"],
        "the first open identity's proven member, at its tip, is out of every cut"
    );
}
