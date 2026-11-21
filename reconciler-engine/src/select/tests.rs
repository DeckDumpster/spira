use serde_json::json;

use super::*;

#[test_id(t32suqv)]
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

#[test_id(t63nn0u)]
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

#[test_id(tbsuq1h)]
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

/// The ranking under a map that weighs `Probe` at 2, a weight the seed does not give it.
fn ranked(a: &crate::assemble::Assembled) -> Vec<crate::rank::RankedGap> {
    let map = crate::causal_map::CausalMap::load("[[weight]]\nkind = \"Probe\"\nweight = 2.0\nunit = \"u\"\nsource = \"s\"\n", &[]).unwrap();
    crate::rank::rank(a, &map).gaps
}

fn plan(sections: Vec<serde_json::Value>, knobs: serde_json::Value) -> Plan {
    let snap = crate::assemble::Snapshot::parse(&json!({
        "list_rev": 4, "sections": sections, "intents": [], "pass": null, "knobs": knobs, "undecodable": [],
    }))
    .unwrap();
    let a = crate::assemble::assemble(&snap, T0);
    let gaps = ranked(&a);
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

#[test_id(t6grxm4)]
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

#[test_id(th7snab)]
#[test]
fn a_stopping_action_is_never_stepped_even_past_admission() {
    let p = plan(vec![probe("a", json!([act("halt", "local", Some(true))]))], json!({ "knobs/Probe/a/halt": 0 }));
    assert_eq!(refused(&p), ["stopping"]);
    assert!(p.steps.is_empty());
}

#[test_id(tfv0yow)]
#[test]
fn a_baseline_needs_a_value_and_a_knob_needs_a_number() {
    let mut s = probe("a", json!([act("drain", "local", None)]));
    s["section"]["actions"][0]["moves"] = json!("wedgedness");
    let p = plan(vec![s], json!({ "knobs/Probe/a/drain": 0 }));
    assert_eq!(refused(&p), ["moves-unreadable"]);
    let p = plan(vec![probe("a", json!([act("drain", "local", None)]))], json!({ "knobs/Probe/a/drain": "on" }));
    assert_eq!(refused(&p), ["knob-not-numeric"]);
}

#[test_id(tdj1ilw)]
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
    let gaps = ranked(&a);
    let p = select(&Inputs { assembled: &a, gaps: &gaps, shadow: &[], halted: false, now_ms: T0, token: "1.0" });
    assert_eq!(p.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["b-first"], "never attempted beats attempted a day ago");
}

fn host(conditions: &[&str], io: f64) -> serde_json::Value {
    use spira_desired_state::sections::host as h;
    let conds: Vec<_> = [h::SCRATCH_SHORT, h::DISK_SHORT, h::IO_PRESSURE, h::HOST_SHEDDING, h::SHED]
        .iter()
        .map(|c| json!({ "type": c, "status": if conditions.contains(c) { "True" } else { "False" }, "reason": "t", "lastTransitionTime": NOW }))
        .collect();
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": h::NAME,
        "metadata": { "name": h::SECTION, "owner": h::OWNER, "observedAt": NOW, "target_latency": "5m", "max_age": "15m" },
        "status": {
            "observedGeneration": 0, "conditions": conds,
            h::SCRATCH: { "value": 0.5, "window": "level" }, h::WORKTREES: { "value": 0.5, "window": "level" },
            h::ROOT: { "value": 0.5, "window": "level" }, h::IO: { "value": io, "window": "5m" },
            h::CERTIFY_PAR: { "value": 1.0, "window": "level" },
        },
        "flow": { "consumes": [], "produces": ["host/scratch", "host/io"] },
        "distance": { "fn": h::DISTANCE, "open": h::OPEN, "close": h::CLOSE },
        "actions": h::actions(),
    }})
}

fn host_knobs() -> serde_json::Value {
    json!({
        "knobs/Host/host/shed-gate-slot": 3, "knobs/Host/host/restore-gate-slot": 3,
        "knobs/Host/host/reap-stale-scratch": 20, "knobs/Host/host/reap-stale-targets": 20,
    })
}

#[test_id(tg3nhav)]
#[test]
fn the_host_reaps_are_refused_while_no_andon_section_exists() {
    let p = plan(vec![host(&["DiskShort"], 80.0)], host_knobs());
    assert!(p.steps.is_empty() && p.would_step.is_empty(), "{p:?}");
    let mut got: Vec<_> = p.refused.iter().map(|r| (r.action.as_str(), r.code.as_str())).collect();
    got.sort_unstable();
    assert_eq!(got, [("reap-stale-scratch", "no-andon"), ("reap-stale-targets", "no-andon")]);
}

#[test_id(tq4ogeq)]
#[test]
fn restore_gate_slot_is_a_candidate_only_under_shed() {
    let shedding = plan(vec![host(&["HostShedding"], 80.0)], host_knobs());
    assert!(shedding.steps.is_empty(), "a shed in force with the host still over its spec is not yet Shed: {shedding:?}");
    let shed = plan(vec![host(&["HostShedding", "Shed"], 80.0)], host_knobs());
    assert_eq!(shed.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["restore-gate-slot"]);
    assert_eq!((shed.steps[0].intent.from.clone(), shed.steps[0].intent.to.clone()), (json!(3), json!(4)));
}

#[test_id(t2q1nvp)]
#[test]
fn io_pressure_picks_the_shed_not_a_reap_and_steps_certify_par_down_by_one() {
    let p = plan(vec![host(&["IoPressure"], 80.0)], host_knobs());
    assert_eq!(p.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["shed-gate-slot"]);
    assert_eq!((p.steps[0].intent.from.clone(), p.steps[0].intent.to.clone()), (json!(3), json!(2)));
}

#[test_id(tkhgjxe)]
#[test]
fn a_calm_host_under_shed_restores_a_gate_slot_though_its_gap_is_closed() {
    let calm = plan(vec![host(&["HostShedding", "Shed"], 0.0)], host_knobs());
    assert_eq!(calm.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["restore-gate-slot"], "{calm:?}");
    assert_eq!((calm.steps[0].intent.from.clone(), calm.steps[0].intent.to.clone()), (json!(3), json!(4)));
}

#[test_id(tpzmv63)]
#[test]
fn a_closed_gap_takes_only_capacity_raise_steps_and_keeps_every_refusal() {
    let calm = plan(vec![host(&["HostShedding", "IoPressure"], 0.0)], host_knobs());
    assert!(calm.steps.is_empty() && calm.refused.is_empty() && calm.no_lever.is_empty(), "a lower is never taken on a closed gap: {calm:?}");
    let mut s = host(&["Shed"], 0.0);
    s["section"]["metadata"]["observedAt"] = json!("2026-10-31T10:00:00Z");
    let stale = plan(vec![s], host_knobs());
    assert!(stale.steps.is_empty());
    assert_eq!(refused(&stale), ["stale-input"]);
    let no_knob = plan(vec![host(&["Shed"], 0.0)], json!({}));
    assert_eq!(refused(&no_knob), ["knob-absent"]);
}

fn fleet(conditions: &[&str], utilisation: f64, actions: serde_json::Value) -> serde_json::Value {
    let conds: Vec<_> = ["Saturated", "HostShedding"]
        .iter()
        .map(|c| json!({ "type": c, "status": if conditions.contains(c) { "True" } else { "False" }, "reason": "t", "lastTransitionTime": NOW }))
        .collect();
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": "DwarfFleet",
        "metadata": { "name": "fleet", "owner": "sentinel", "observedAt": NOW, "target_latency": "5m", "max_age": "15m" },
        "status": { "observedGeneration": 0, "conditions": conds, "utilisation_1h": { "value": utilisation, "window": "1h" } },
        "distance": { "fn": "max(0, utilisation_1h - 0.85)", "open": 0.05, "close": 0.0 },
        "actions": actions,
    }})
}

fn fleet_actions() -> serde_json::Value {
    json!([
        { "name": "max-live-dwarves-raise", "class": "capacity-raise", "step": "+1", "moves": "utilisation_1h",
          "addresses": ["Saturated"], "expected_effect": -0.1, "horizon": "65m" },
        { "name": "max-live-dwarves-lower", "class": "capacity-lower", "step": "-1", "moves": "utilisation_1h",
          "addresses": ["HostShedding"], "expected_effect": 0.1, "horizon": "65m" },
    ])
}

fn fleet_knobs() -> serde_json::Value {
    json!({ "knobs/DwarfFleet/fleet/max-live-dwarves-raise": 4, "knobs/DwarfFleet/fleet/max-live-dwarves-lower": 4 })
}

#[test_id(tq4p3kr)]
#[test]
fn the_fleet_lowers_on_host_shedding_only_while_its_gap_is_open() {
    let open = plan(vec![fleet(&["HostShedding"], 0.99, fleet_actions())], fleet_knobs());
    assert_eq!(open.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["max-live-dwarves-lower"], "{open:?}");
    let closed = plan(vec![fleet(&["HostShedding"], 0.5, fleet_actions())], fleet_knobs());
    assert!(closed.steps.is_empty() && closed.refused.is_empty() && closed.no_lever.is_empty(), "{closed:?}");
}

#[test_id(t8can4y)]
#[test]
fn a_closed_fleet_with_nothing_addressed_reports_neither_a_missing_lever_nor_a_term() {
    let p = plan(vec![fleet(&["HostShedding"], 0.5, json!([]))], json!({}));
    assert_eq!(p, Plan::default(), "{p:?}");
}

#[test_id(t164qwm)]
#[test]
fn a_closed_fleet_still_raises_for_a_saturated_condition_still_open_on_it() {
    let p = plan(vec![fleet(&["Saturated"], 0.5, fleet_actions())], fleet_knobs());
    assert_eq!(p.steps.iter().map(|s| s.action.as_str()).collect::<Vec<_>>(), ["max-live-dwarves-raise"], "{p:?}");
}
