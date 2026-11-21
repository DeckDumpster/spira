use serde_json::{json, Value};

use super::*;

/// A Probe section every rule admits; each test breaks one thing in it.
fn good() -> Value {
    json!({
        "apiVersion": "spira/v1",
        "kind": "Probe",
        "metadata": {
            "name": "a", "owner": "probe",
            "observedAt": "2026-10-31T19:39:58Z", "target_latency": "5m", "max_age": "15m",
        },
        "status": {
            "observedGeneration": 1,
            "depth": { "value": 3.0, "window": "level" },
            "wedged": { "value": 1.0, "window": "1h" },
            "depth_24h": { "value": 40.0, "window": "24h" },
            "stuck": [{ "item": "x" }],
            "conditions": [{ "type": "Deep", "status": "True", "reason": "3 deep", "lastTransitionTime": "2026-10-31T19:00:00Z" }],
        },
        "flow": { "consumes": ["vm-pool/slots"], "produces": ["lifecycle/landed"] },
        "distance": { "fn": "max(0, depth) / 5 + wedged", "open": 1.0, "close": 0.5 },
        "actions": [{
            "name": "drain", "class": "local", "step": "one item",
            "moves": "depth", "addresses": ["Deep"], "expected_effect": -1.0, "horizon": "10m",
        }],
    })
}

fn refused(v: Value) -> Refusal {
    match parse_section(&v) {
        Err(r) => r,
        Ok(s) => admit(&s).expect_err("admitted"),
    }
}

/// `v` with the value at `ptr` set; an object's missing last segment is added.
fn with(mut v: Value, ptr: &str, x: Value) -> Value {
    let (parent, last) = ptr.rsplit_once('/').unwrap_or_else(|| panic!("{ptr}"));
    match v.pointer_mut(parent).unwrap_or_else(|| panic!("{ptr}")) {
        Value::Object(o) => {
            o.insert(last.to_string(), x);
        }
        Value::Array(a) => a[last.parse::<usize>().unwrap_or_else(|_| panic!("{ptr}"))] = x,
        _ => panic!("{ptr}"),
    }
    v
}

#[test_id(tml0tvh)]
#[test]
fn the_good_section_is_admitted_and_its_distance_evaluates() {
    let s = parse_section(&good()).unwrap();
    let kind = admit_publish(&s).unwrap();
    assert!((s.distance_value(kind).unwrap().unwrap() - 1.6).abs() < 1e-9);
}

#[test_id(t9hl9k8)]
#[test]
fn unknown_fields_kinds_and_api_versions_are_refused_naming_the_field() {
    let mut v = good();
    v["metadata"]["bogus"] = json!(1);
    let r = refused(v);
    assert_eq!(r.field, "metadata.bogus", "{r}");
    assert!(r.rule.contains("bogus"), "{r}");

    let mut v = good();
    v["status"]["bogus"] = json!({ "value": 1.0, "window": "level" });
    let r = refused(v);
    assert!(r.field.starts_with("status"), "{r}");
    assert!(r.rule.contains("bogus"), "{r}");

    let mut v = good();
    v["spec"] = json!({ "depth_max": { "value": 4.0, "statute": "law-x", "x": 1 } });
    assert!(refused(v).field.starts_with("spec."));

    assert_eq!(refused(with(good(), "/kind", json!("Warp"))).field, "kind"); // not a path: JSON pointer
    assert_eq!(refused(with(good(), "/apiVersion", json!("spira/v2"))).field, "apiVersion"); // not a path: JSON pointer
}

#[test_id(tymvmms)]
#[test]
fn andon_is_refused_by_name_until_its_design_registers_it() {
    let r = refused(with(good(), "/kind", json!("Andon"))); // not a path: JSON pointer
    assert_eq!(r.field, "kind");
    assert!(r.rule.contains("Andon"), "{r}");
}

#[test_id(tp2ag36)]
#[test]
fn a_horizon_shorter_than_the_moved_fields_window_plus_target_latency_is_refused() {
    let v = with(good(), "/actions/0/moves", json!("wedged")); // not a path: JSON pointer
    let r = refused(with(v.clone(), "/actions/0/horizon", json!("5m"))); // not a path: JSON pointer
    assert_eq!((r.field.as_str(), r.code), ("actions[0].horizon", "horizon"), "{r}");
    let ok = with(v, "/actions/0/horizon", json!("65m")); // not a path: JSON pointer
    admit(&parse_section(&ok).unwrap()).expect("1h window + 5m latency = 65m");
}

#[test_id(t0to3md)]
#[test]
fn only_bounded_steps_are_admitted() {
    for bad in ["set to 4", "rebuild", "+30", "+N", "26% fewer", "0% more", "one", "+1 SLOT", "−0", "one rebuild"] {
        let r = refused(with(good(), "/actions/0/step", json!(bad))); // not a path: JSON pointer
        assert_eq!(r.code, "step", "{bad}: {r}");
    }
    for ok in ["+1", "-2", "−25", "+1 slot", "10% fewer", "25% more", "one member-at-tip"] {
        admit(&parse_section(&with(good(), "/actions/0/step", json!(ok))).unwrap()) // not a path: JSON pointer
            .unwrap_or_else(|r| panic!("{ok}: {r}"));
    }
}

#[test_id(ty78f98)]
#[test]
fn stopping_an_undeclared_condition_and_a_non_numeric_moves_are_refused() {
    let r = refused(with(good(), "/actions/0/stopping", json!(true))); // not a path: JSON pointer
    assert_eq!(r.code, "stopping");
    admit(&parse_section(&with(good(), "/actions/0/stopping", json!(false))).unwrap()).unwrap(); // not a path: JSON pointer

    assert_eq!(refused(with(good(), "/status/conditions/0/type", json!("Lost"))).code, "condition"); // not a path: JSON pointer
    assert_eq!(refused(with(good(), "/actions/0/addresses/0", json!("Lost"))).code, "addresses"); // not a path: JSON pointer
    assert_eq!(refused(with(good(), "/actions/0/moves", json!("stuck"))).code, "moves"); // not a path: JSON pointer

    let mut v = with(good(), "/status/conditions/0/status", json!("Unknown")); // not a path: JSON pointer
    v = with(v, "/status/conditions/0/reason", json!(" ")); // not a path: JSON pointer
    assert_eq!(refused(v).field, "status.conditions[0].reason");
}

#[test_id(tp8a3yx)]
#[test]
fn ownership_revision_name_flow_and_time_fields_are_checked() {
    assert_eq!(refused(with(good(), "/metadata/owner", json!("intruder"))).code, "owner"); // not a path: JSON pointer
    let v = with(good(), "/metadata/resourceVersion", json!("7")); // not a path: JSON pointer
    assert_eq!(refused(v).code, "resource-version");
    assert_eq!(refused(with(good(), "/metadata/name", json!("a/b"))).code, "name"); // not a path: JSON pointer
    assert_eq!(refused(with(good(), "/flow/consumes/0", json!("slots"))).code, "flow"); // not a path: JSON pointer
    assert_eq!(refused(with(good(), "/metadata/observedAt", json!("yesterday"))).code, "time"); // not a path: JSON pointer
    assert_eq!(refused(with(good(), "/metadata/max_age", json!("15 min"))).code, "duration"); // not a path: JSON pointer
    assert_eq!(refused(with(good(), "/status/depth_24h/window", json!("banana"))).code, "window", "an unreferenced field's window is checked too"); // not a path: JSON pointer
    let mut v = good();
    let first = v["actions"][0].clone();
    v["actions"].as_array_mut().unwrap().push(first);
    assert_eq!(refused(v).code, "action");
    let mut v = good();
    let first = v["actions"][0].clone();
    v["actions"].as_array_mut().unwrap().push(first);
    v["actions"][1]["class"] = json!("capacity-raise");
    admit(&parse_section(&v).unwrap()).expect("a ±1 knob is one declaration per class");
}

#[test_id(tcnekpq)]
#[test]
fn a_publish_must_declare_when_it_observed_and_for_how_long() {
    let mut v = good();
    v["metadata"].as_object_mut().unwrap().remove("max_age");
    let s = parse_section(&v).unwrap();
    admit(&s).unwrap();
    assert_eq!(admit_publish(&s).unwrap_err().field, "metadata.max_age");
}

#[test_id(tg90c91)]
#[test]
fn freshness_is_age_against_max_age_and_generation_lag_is_its_own_fact() {
    let mut s = parse_section(&good()).unwrap();
    let observed = parse_rfc3339("2026-10-31T19:39:58Z").unwrap() * 1000;
    assert_eq!(freshness(&s, observed + 15 * 60 * 1000), Freshness::Fresh);
    match freshness(&s, observed + 16 * 60 * 1000) {
        Freshness::Stale { age_secs, reason } => {
            assert_eq!(age_secs, Some(960));
            assert!(reason.contains("max_age"), "{reason}");
        }
        f => panic!("{f:?}"),
    }
    s.metadata.generation = Some(2);
    assert!(!caught_up(&s));
    assert_eq!(freshness(&s, observed), Freshness::Fresh);
    s.metadata.observed_at = None;
    assert!(matches!(freshness(&s, observed), Freshness::Stale { age_secs: None, .. }));
}

#[test_id(tzwaho4)]
#[test]
fn times_and_durations_parse_in_the_one_form_this_harness_writes() {
    assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    let t = 1_790_294_400;
    assert_eq!(parse_rfc3339(&crate::store::format_rfc3339(t)), Some(t));
    for bad in ["2026-13-01T00:00:00Z", "2026-10-31 19:39:58Z", "2026-10-31T19:39:58+00:00", "2026-1x-31T19:39:58Z"] {
        assert_eq!(parse_rfc3339(bad), None, "{bad}");
    }
    assert_eq!(parse_duration("90s"), Some(90));
    assert_eq!(parse_duration("2d"), Some(172_800));
    for bad in ["0m", "m", "5", "-5m", "5w"] {
        assert_eq!(parse_duration(bad), None, "{bad}");
    }
    assert_eq!(Window::parse("level"), Some(Window::Level));
    assert_eq!(Window::parse("1h").map(Window::secs), Some(3600));
}

#[test_id(t1grnn4)]
#[test]
fn keys_and_refs_name_one_section() {
    assert_eq!(section_key("Probe", "a"), "telemetry/sections/Probe/a");
    assert_eq!(parse_ref("Probe/a"), Some(("Probe", "a")));
    for bad in ["Probe", "Probe/", "/a", "Probe/a/b"] { // not a path: JSON pointer
        assert_eq!(parse_ref(bad), None, "{bad}");
    }
}
