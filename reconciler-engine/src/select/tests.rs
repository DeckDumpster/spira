use serde_json::json;

use super::*;

#[test]
fn a_closed_form_step_moves_from_to_and_keeps_an_integer_knob_an_integer() {
    assert_eq!(step_to("+1 slot", &json!(3)), Some(json!(4)));
    assert_eq!(step_to("-2", &json!(3)), Some(json!(1)));
    assert_eq!(step_to("−1 slot", &json!(3)), Some(json!(2)));
    assert_eq!(step_to("one item", &json!(0)), Some(json!(1)));
    assert_eq!(step_to("one fewer", &json!(2)), Some(json!(1)));
    assert_eq!(step_to("-1", &json!(0)), Some(json!(0)), "an integer knob is never stepped below zero");
    assert_eq!(step_to("10% fewer", &json!(50)), Some(json!(45)));
    assert_eq!(step_to("10% fewer", &json!(4)), Some(json!(3)), "an asymptotic step still moves an integer by one");
    assert_eq!(step_to("10% more", &json!(2.0)), Some(json!(2.2)));
    assert_eq!(step_to("+1", &json!("x")), None, "a knob that is not a number has no to");
    assert_eq!(step_to("set to 4", &json!(1)), None);
}

#[test]
fn back_off_doubles_per_consecutive_failure_and_stops_at_a_day() {
    assert_eq!(backoff_secs(600, 0), 600);
    assert_eq!(backoff_secs(600, 3), 4800);
    assert_eq!(backoff_secs(3600, 5), BACKOFF_CAP_SECS);
    assert_eq!(backoff_secs(u64::MAX, 1), BACKOFF_CAP_SECS);
    let failed = |taken, at| (taken, JudgementResult::Failed, at);
    let h = Attempts { last_taken: 0, judged: vec![failed(100, 700), (0, JudgementResult::Fulfilled, 600), failed(800, 1400)], horizon: 600 };
    assert_eq!(h.backoff_until(1400), Some(1400 + 2400), "two failures after the last fulfilment");
    assert_eq!(h.backoff_until(3800), None);
    let ok = Attempts { last_taken: 0, judged: vec![failed(0, 600), (700, JudgementResult::Fulfilled, 1300)], horizon: 600 };
    assert_eq!(ok.backoff_until(1300), None, "a fulfilment ends the streak");
}

#[test]
fn every_refusal_has_a_stable_code_and_a_reason_naming_what() {
    let all = [
        Refusal::StaleInput("Probe/a".into()),
        Refusal::NotCaughtUp,
        Refusal::BackOff { until: 0 },
        Refusal::InFlight("intents/Probe/a/1".into()),
        Refusal::UpstreamAbsent("host/scratch".into()),
        Refusal::UpstreamStale("Probe/p".into()),
        Refusal::UpstreamGapped("Probe/p".into()),
        Refusal::NoAndon,
        Refusal::AndonPulled("Probe/a".into()),
        Refusal::Stopping,
        Refusal::KnobAbsent("knobs/x".into()),
        Refusal::KnobNotNumeric("knobs/x".into()),
        Refusal::MovesUnreadable("depth".into()),
        Refusal::NoHoldTarget("RoundController/lane".into()),
    ];
    let mut codes: Vec<_> = all.iter().map(Refusal::code).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), all.len());
    assert!(all.iter().all(|r| !r.reason().is_empty()));
    assert!(Refusal::UpstreamAbsent("host/scratch".into()).reason().contains("host/scratch"));
}

/// 2026-10-31T19:40:00Z.
const T0: u64 = 1_793_475_600_000;
const NOW: &str = "2026-10-31T19:40:00Z";

fn probe(name: &str, actions: serde_json::Value) -> serde_json::Value {
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": "Probe",
        "metadata": { "name": name, "owner": "probe", "observedAt": NOW, "target_latency": "5m", "max_age": "15m" },
        "status": {
            "observedGeneration": 0,
            "depth": { "value": 3.0, "window": "level" },
            "wedged": { "value": 1.0, "window": "1h" },
            "conditions": [{ "type": "Deep", "status": "True", "reason": "t", "lastTransitionTime": NOW }],
        },
        "distance": { "fn": "max(0, depth) / 5 + wedged", "open": 1.0, "close": 0.5 },
        "actions": actions,
    }})
}

fn andon(reason: &str) -> serde_json::Value {
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": "Andon",
        "metadata": { "name": "cord", "owner": "andon", "observedAt": NOW, "target_latency": "5m", "max_age": "15m" },
        "status": { "conditions": [{ "type": "Pulled", "status": "True", "reason": reason, "lastTransitionTime": NOW }] },
    }})
}

fn plan(sections: Vec<serde_json::Value>, knobs: serde_json::Value) -> Plan {
    let snap = crate::assemble::Snapshot::parse(&json!({
        "list_rev": 4, "sections": sections, "intents": [], "pass": null, "knobs": knobs, "undecodable": [],
    }))
    .unwrap();
    let a = crate::assemble::assemble(&snap, T0);
    let gaps = crate::rank::rank(&a, &crate::causal_map::CausalMap::seed().unwrap()).gaps;
    select(&Inputs { assembled: &a, gaps: &gaps, shadow: &[], halted: false, now_ms: T0, token: "1.0" })
}

fn act(name: &str, class: &str, stopping: Option<bool>) -> serde_json::Value {
    let mut a = json!({ "name": name, "class": class, "step": "one item", "moves": "depth", "addresses": ["Deep"], "expected_effect": -1.0, "horizon": "10m" });
    if let Some(s) = stopping {
        a["stopping"] = json!(s);
    }
    a
}

fn refused(p: &Plan) -> Vec<&str> {
    p.refused.iter().map(|r| r.code.as_str()).collect()
}

#[test]
fn a_pulled_cord_naming_the_target_refuses_its_step_and_a_present_andon_admits_a_destructive_one() {
    let knobs = json!({ "knobs/Probe/a/reap": 1, "knobs/Probe/b/reap": 1 });
    let reap = || json!([act("reap", "destructive", None)]);
    let p = plan(vec![probe("a", reap()), probe("b", reap()), andon("Probe/a")], knobs.clone());
    assert_eq!(refused(&p), ["andon-pulled"]);
    assert_eq!(p.steps.iter().map(|s| s.section.as_str()).collect::<Vec<_>>(), ["Probe/b"], "a cord names what it stops, nothing else");
    let p = plan(vec![probe("a", reap())], knobs);
    assert_eq!(refused(&p), ["no-andon"]);
}

#[test]
fn a_stopping_action_is_never_stepped_even_past_admission() {
    let p = plan(vec![probe("a", json!([act("halt", "local", Some(true))]))], json!({ "knobs/Probe/a/halt": 0 }));
    assert_eq!(refused(&p), ["stopping"]);
    assert!(p.steps.is_empty());
}

#[test]
fn a_baseline_needs_a_value_and_a_knob_needs_a_number() {
    let mut s = probe("a", json!([act("drain", "local", None)]));
    s["section"]["actions"][0]["moves"] = json!("wedgedness");
    let p = plan(vec![s], json!({ "knobs/Probe/a/drain": 0 }));
    assert_eq!(refused(&p), ["moves-unreadable"]);
    let p = plan(vec![probe("a", json!([act("drain", "local", None)]))], json!({ "knobs/Probe/a/drain": "on" }));
    assert_eq!(refused(&p), ["knob-not-numeric"]);
}

#[test]
fn among_candidates_the_one_attempted_longest_ago_goes_first_never_declaration_order() {
    let snap = crate::assemble::Snapshot::parse(&json!({
        "list_rev": 4,
        "sections": [probe("a", json!([act("a-second", "local", None), act("b-first", "local", None)]))],
        "intents": [{ "key": "intents/Probe/a/1", "mod_rev": 2, "intent": {
            "id": "1", "action": "a-second", "target": "knobs/Probe/a/a-second", "from": 0, "to": 1, "moves": "depth",
            "baseline": 3.0, "expected_effect": -1.0, "horizon": "10m", "addresses": ["Deep"],
            "taken_at": "2026-10-30T19:40:00Z", "by": { "pass": "1", "lock_token": "1.0" },
            "ack": { "at": "2026-10-30T19:41:00Z", "result": "applied", "observed_rev": 3 },
            "judgement": { "at": "2026-10-30T19:51:00Z", "result": "fulfilled", "observed": 2.0 },
        }}],
        "pass": null, "knobs": { "knobs/Probe/a/b-first": 0, "knobs/Probe/a/a-second": 0 }, "undecodable": [],
    }))
    .unwrap();
    let a = crate::assemble::assemble(&snap, T0);
    let gaps = crate::rank::rank(&a, &crate::causal_map::CausalMap::seed().unwrap()).gaps;
    let p = select(&Inputs { assembled: &a, gaps: &gaps, shadow: &[], halted: false, now_ms: T0, token: "1.0" });
    assert_eq!(p.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["b-first"], "never attempted beats attempted a day ago");
}

fn round_controller(holds: serde_json::Value) -> serde_json::Value {
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": "RoundController",
        "metadata": { "name": "lane", "owner": "round-controller", "observedAt": NOW, "target_latency": "5m", "max_age": "15m" },
        "status": {
            "observedGeneration": 0,
            "repeat_identity_open": { "value": 2.0, "window": "level" },
            "repeat_identity_holds": holds,
            "conditions": [{ "type": "RepeatIdentity", "status": "True", "reason": "identity h1 run 2 times by the planner, still red", "lastTransitionTime": NOW }],
        },
        "distance": { "fn": "repeat_identity_open", "open": 1.0, "close": 0.5 },
        "actions": [{ "name": "hold-member", "class": "local", "step": "one member at one tip", "moves": "repeat_identity_open",
                      "addresses": ["RepeatIdentity"], "expected_effect": -1.0, "horizon": "10m" }],
    }})
}

#[test]
fn a_hold_member_step_creates_the_hold_on_the_first_named_member_not_already_held() {
    let holds = json!([
        { "identity": "h1", "id": "sp-a", "tip": "a1" },
        { "identity": "h1", "id": "sp-b", "tip": "b1" },
    ]);
    let p = plan(vec![round_controller(holds.clone())], json!({}));
    assert_eq!(refused(&p), Vec::<&str>::new());
    let i = &p.steps[0].intent;
    assert_eq!(
        (i.action.as_str(), i.target.as_str(), &i.from, &i.to),
        ("hold-member", "knobs/RoundController/lane/hold/sp-a/a1", &serde_json::Value::Null, &json!({ "lifted_by": "reconciler" })),
        "an absent hold reads as null, so no knob need exist first"
    );
    let p = plan(vec![round_controller(holds)], json!({ "knobs/RoundController/lane/hold/sp-a/a1": { "lifted_by": "reconciler" } }));
    assert_eq!(p.steps[0].intent.target, "knobs/RoundController/lane/hold/sp-b/b1");
}

#[test]
fn a_hold_member_step_with_no_member_left_to_hold_is_refused_by_name() {
    let held = json!({ "knobs/RoundController/lane/hold/sp-a/a1": { "lifted_by": "reconciler" } });
    for (holds, knobs) in [
        (json!([{ "identity": "h1", "id": "sp-a", "tip": "a1" }]), held),
        (json!([]), json!({})),
    ] {
        let p = plan(vec![round_controller(holds)], knobs);
        assert!(p.steps.is_empty());
        assert_eq!(refused(&p), ["no-hold-target"]);
        assert!(p.refused[0].reason.contains("RoundController/lane"), "{}", p.refused[0].reason);
    }
}
