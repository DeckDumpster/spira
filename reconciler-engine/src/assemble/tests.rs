use serde_json::{json, Value};

use super::*;
use crate::pass_record::{history_key, Gap, Goodput, Judged, Mode, Refused, Step, PASSES_PREFIX};
use spira_desired_state::intent::JudgementResult;

/// 2026-10-31T19:40:00Z.
const T0: u64 = 1_793_475_600_000;
const NOW: &str = "2026-10-31T19:40:00Z";

fn section(name: &str, meta: Value) -> Value {
    let mut m = json!({ "name": name, "owner": "probe", "resourceVersion": "3" });
    for (k, v) in meta.as_object().unwrap() {
        m[k] = v.clone();
    }
    json!({ "section": { "apiVersion": "spira/v1", "kind": "Probe", "metadata": m }, "freshness": { "state": "Fresh" }, "caught_up": true })
}

fn published(name: &str) -> Value {
    section(name, json!({ "observedAt": NOW, "target_latency": "5m", "max_age": "15m" }))
}

fn intent(key: &str, taken_at: &str, horizon: &str, ack: Value) -> Value {
    json!({ "key": key, "mod_rev": 2, "intent": {
        "id": "1", "action": "drain", "target": "knobs/x", "from": 1, "to": 2, "moves": "depth",
        "baseline": 3.0, "expected_effect": -1.0, "horizon": horizon, "addresses": ["Deep"],
        "taken_at": taken_at, "by": { "pass": "p-1", "lock_token": "1.0" }, "ack": ack,
    }})
}

fn record(n: u64) -> Value {
    json!({ "n": n, "list_rev": 1, "at": NOW, "holder": "goodput-pass", "mode": "shadow" })
}

fn snap(sections: Vec<Value>, intents: Vec<Value>, pass: Value) -> Snapshot {
    Snapshot::parse(&json!({ "list_rev": 9, "sections": sections, "intents": intents, "pass": pass, "undecodable": [] })).unwrap()
}

#[test]
fn a_snapshot_keeps_what_does_not_decode_and_refuses_what_it_cannot_read() {
    let v = json!({
        "list_rev": 9,
        "sections": [published("a")],
        "undecodable": ["telemetry/sections/Probe/bad"],
        "intents": [intent("intents/Probe/a/1", NOW, "10m", Value::Null), { "key": "intents/Probe/a/2", "mod_rev": 3, "intent": {} }],
        "pass": null,
    });
    let s = Snapshot::parse(&v).unwrap();
    assert_eq!(s.list_rev, 9);
    assert_eq!(s.sections.len(), 1);
    assert_eq!(s.intents.len(), 1);
    assert_eq!(s.pass, None, "a null pass record reads as none");
    assert_eq!(s.undecodable, ["telemetry/sections/Probe/bad", "intents/Probe/a/2"]);

    let full = || json!({ "list_rev": 1, "sections": [], "undecodable": [], "intents": [], "pass": null });
    let mut bad_sections = full();
    bad_sections["sections"] = json!([{ "section": { "kind": "Probe", "metadata": { "name": "x" }, "bogus": 1 } }, { "section": { "kind": 1 } }]);
    assert_eq!(Snapshot::parse(&bad_sections).unwrap().undecodable, ["telemetry/sections/Probe/x", "sections[1]"], "a bad section is kept by key and blinds no other");

    let without = |field: &str| {
        let mut v = full();
        v.as_object_mut().unwrap().remove(field);
        v
    };
    for field in ["list_rev", "sections", "undecodable", "intents", "pass"] {
        assert!(Snapshot::parse(&without(field)).is_err(), "a snapshot with no {field} was read");
    }
    for (what, ptr, x) in [
        ("an undecodable entry that is not a key", "/undecodable", json!([1])),
        ("an intent with no key", "/intents", json!([{ "mod_rev": 1, "intent": {} }])),
        ("an intent with no mod_rev", "/intents", json!([{ "key": "intents/Probe/a/1", "intent": {} }])),
        ("a pass record that does not decode", "/pass", json!({ "n": "x" })),
    ] {
        let mut v = full();
        *v.pointer_mut(ptr).unwrap() = x;
        assert!(Snapshot::parse(&v).is_err(), "{what} was read");
    }
}

#[test]
fn the_pass_numbers_from_one_and_follows_the_previous_record() {
    let none = assemble(&snap(vec![], vec![], Value::Null), T0);
    assert_eq!((none.n, none.prev.as_ref()), (1, None));
    let after = assemble(&snap(vec![], vec![], record(41)), T0);
    assert_eq!(after.n, 42);
    assert_eq!(after.prev.unwrap().mode, Mode::Shadow);
    assert_eq!(history_key(42), format!("{PASSES_PREFIX}42"));
}

#[test]
fn only_unacked_intents_past_their_horizon_count_and_unreadable_times_count() {
    let acked = json!({ "at": NOW, "result": "applied", "observed_rev": 4 });
    let s = snap(
        vec![published("a"), published("b"), published("c")],
        vec![
            intent("intents/Probe/a/1", NOW, "10m", Value::Null),
            intent("intents/Probe/a/2", NOW, "10m", acked),
            intent("intents/Probe/b/1", "yesterday", "10m", Value::Null),
            intent("intents/Probe/c/1", NOW, "forever", Value::Null),
            intent("intents/stray", NOW, "10m", Value::Null),
            intent("intents/Probe/gone/1", NOW, "10m", Value::Null),
        ],
        Value::Null,
    );
    let derived = |a: &Assembled, r: &str| a.current.sections[r].derived.clone();
    let inside = assemble(&s, T0 + 10 * 60_000);
    assert_eq!(derived(&inside, "Probe/a"), [], "at its horizon an intent is not yet past it");
    assert_eq!(derived(&inside, "Probe/b"), [Derived::IntentsUnacknowledged { n: 1 }]);
    assert_eq!(derived(&inside, "Probe/c"), [Derived::IntentsUnacknowledged { n: 1 }]);
    let past = assemble(&s, T0 + 10 * 60_000 + 1_000);
    assert_eq!(derived(&past, "Probe/a"), [Derived::IntentsUnacknowledged { n: 1 }], "the acked intent does not count");
    assert_eq!(past.intents.len(), 6, "every intent is carried");
    let unattributed: Vec<_> = past.unattributed.iter().map(|(k, n)| (k.as_str(), *n)).collect();
    assert_eq!(unattributed, [("Probe/gone", 1), ("intents/stray", 1)], "an intent with no section to mark is still counted");
}

#[test]
fn publish_latency_is_age_against_target_latency_and_unmeasured_without_a_status() {
    let s = snap(
        vec![published("a"), section("never", json!({})), section("loose", json!({ "observedAt": NOW, "max_age": "15m" }))],
        vec![],
        Value::Null,
    );
    let at = |ms: u64, r: &str| assemble(&s, ms).ages[r].clone();
    assert_eq!(at(T0 + 300_000, "Probe/a").publish_latency, PublishLatency::Within { target_secs: 300 });
    let late = at(T0 + 301_000, "Probe/a");
    assert_eq!(late.publish_latency, PublishLatency::Late { target_secs: 300, over_secs: 1 });
    assert_eq!((late.age_secs, late.freshness), (Some(301), Freshness::Fresh));
    let never = at(T0, "Probe/never");
    assert_eq!((never.age_secs, never.publish_latency), (None, PublishLatency::Unmeasured));
    assert!(matches!(never.freshness, Freshness::Stale { age_secs: None, .. }), "{:?}", never.freshness);
    assert_eq!(at(T0, "Probe/loose").publish_latency, PublishLatency::Undeclared);
}

#[test]
fn the_verb_source_reads_the_programs_answer_and_refuses_a_failed_or_garbled_one() {
    let sh = |script: &str| Verb { argv: vec!["sh".into(), "-c".into(), script.into(), "spira-lc".into()] };
    let got = sh(r#"[ "$1 $2" = "section snapshot" ] && printf '%s' '{"list_rev": 7, "sections": [], "undecodable": [], "intents": [], "pass": null}'"#).snapshot().unwrap();
    assert_eq!(got.list_rev, 7);
    let refused = sh(r#"printf '{"cannot_tell": "no store"}'; echo held >&2; exit 2"#).snapshot().unwrap_err();
    assert!(refused.contains("exited Some(2)") && refused.contains("no store") && refused.contains("held"), "{refused}");
    assert!(sh("printf 'not json'").snapshot().unwrap_err().contains("non-JSON"));
    assert!(Verb { argv: vec![] }.snapshot().is_err());
    assert_eq!(Verb::new("spira-lc").argv, ["spira-lc"]);
}

#[test]
fn a_pass_record_round_trips_with_every_age_shape() {
    let fresh = |f: Freshness, l: PublishLatency| SectionAge { age_secs: Some(1), freshness: f, caught_up: false, publish_latency: l };
    let rec = PassRecord {
        n: 3,
        list_rev: 88,
        at: NOW.into(),
        holder: "goodput-pass".into(),
        mode: Mode::Halted,
        goodput: Some(Goodput { ratio: Some(0.9), rate_per_day: None }),
        binding: Some("Probe/a".into()),
        gaps: vec![Gap { section: "Probe/a".into(), distance: 1.5, score: 3.0 }],
        steps: vec![Step { section: "Probe/a".into(), action: "drain".into(), intent: None }],
        judged: vec![Judged { intent: "intents/Probe/a/1".into(), result: JudgementResult::Failed }],
        refused: vec![Refused { section: "Probe/b".into(), action: "drain".into(), reason: "stale".into() }],
        exhausted: vec!["Probe/c".into()],
        no_lever: vec!["Probe/d".into()],
        ages: [
            ("Probe/a".to_string(), fresh(Freshness::Fresh, PublishLatency::Within { target_secs: 300 })),
            ("Probe/b".to_string(), fresh(Freshness::Stale { age_secs: Some(901), reason: "old".into() }, PublishLatency::Late { target_secs: 300, over_secs: 601 })),
            ("Probe/c".to_string(), fresh(Freshness::Stale { age_secs: None, reason: "never".into() }, PublishLatency::Unmeasured)),
            ("Probe/d".to_string(), fresh(Freshness::Fresh, PublishLatency::Undeclared)),
        ]
        .into(),
        shadow_kinds: vec!["Probe".into()],
    };
    let back: PassRecord = serde_json::from_value(serde_json::to_value(&rec).unwrap()).unwrap();
    assert_eq!(back, rec);
}
