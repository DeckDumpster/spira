//! `goodput_keys` — the reconciler's pass record and each section's distance and age, from one
//! `spira-lc section snapshot`. A reading the snapshot does not carry is an absent key, never 0.

use super::{push, Kv};
use crate::quoting::parse_iso8601;
use serde_json::Value;
use spira_store::lc_call::LcClient;

pub fn goodput_keys() -> Kv {
    let Some(client) = crate::store::client() else { return Kv::new() };
    keys_on(&client)
}

pub fn keys_on(client: &LcClient) -> Kv {
    let Some(snap) = crate::store::answer_on(client, &["section", "snapshot"]).and_then(|raw| serde_json::from_str::<Value>(&raw).ok()) else { return Kv::new() };
    let mut out = Kv::new();
    let sections: Vec<&Value> = snap["sections"].as_array().map(|a| a.iter().filter_map(|e| e.get("section")).collect()).unwrap_or_default();
    let by_ref = |r: &str| sections.iter().find(|s| section_ref(s) == r).copied();

    let lifecycle = by_ref("Lifecycle/all");
    if let Some(n) = lifecycle.and_then(|s| number(&s["status"]["landed_24h"]["value"])) {
        push(&mut out, "SP_LC_LANDED_24H", n);
    }
    if let Some(n) = lifecycle.and_then(|s| number(&s["spec"]["goodput_rate_floor"]["value"])) {
        push(&mut out, "SP_GP_TARGET", n);
    }

    let pass = &snap["pass"];
    if pass.is_object() {
        pass_keys(&mut out, &snap, pass, &by_ref);
    }

    push(&mut out, "SP_GP_SEC_N", sections.len().to_string());
    for (i, s) in sections.iter().enumerate() {
        let r = section_ref(s);
        let age = &pass["ages"][r.as_str()];
        let fresh = age["freshness"]["state"].as_str().map(str::to_lowercase);
        let gap = pass["gaps"].as_array().and_then(|g| g.iter().find(|g| g["section"] == r.as_str())).and_then(|g| number(&g["distance"]));
        let fresh_and_closed = fresh.as_deref() == Some("fresh") && s.get("distance").is_some_and(|d| !d.is_null());
        push(&mut out, &format!("SP_GP_SEC{i}_REF"), plain(&r));
        if let Some(d) = gap.or_else(|| fresh_and_closed.then(|| "0".to_string())) {
            push(&mut out, &format!("SP_GP_SEC{i}_DIST"), d);
        }
        if let Some(a) = age["age_secs"].as_u64() {
            push(&mut out, &format!("SP_GP_SEC{i}_AGE"), a.to_string());
        }
        if let Some(f) = fresh {
            push(&mut out, &format!("SP_GP_SEC{i}_FRESH"), f);
        }
    }
    out
}

fn pass_keys<'a>(out: &mut Kv, snap: &Value, pass: &Value, by_ref: &impl Fn(&str) -> Option<&'a Value>) {
    if let Some(at) = pass["at"].as_str() {
        if let Some(t) = parse_iso8601(at) {
            push(out, "SP_GP_PASS_AT", t.to_string());
        }
    }
    if let Some(n) = number(&pass["goodput"]["rate_per_day"]) {
        push(out, "SP_GP_RATE", n);
    }
    let Some(binding) = (match &pass["binding"] {
        Value::Null => Some("-"),
        b => b.as_str(),
    }) else {
        return;
    };
    push(out, "SP_GP_BINDING", plain(binding));
    if binding == "-" {
        return;
    }
    let step = in_flight(snap, binding);
    if let Some(i) = step {
        push(out, "SP_GP_STEP", plain(&format!("{} {}", i["action"].as_str().unwrap_or("?"), i["target"].as_str().unwrap_or("?"))));
        if let Some(at) = i["ack"]["at"].as_str().and_then(|a| a.get(11..16)) {
            push(out, "SP_GP_STEP_ACK", at);
        }
        if let Some(h) = i["horizon"].as_str() {
            push(out, "SP_GP_STEP_HORIZON", plain(h));
        }
    }
    let section = by_ref(binding);
    let moves = step.and_then(|i| i["moves"].as_str()).or_else(|| {
        let s = section?;
        s["actions"].as_array()?.iter().filter_map(|a| a["moves"].as_str()).find(|m| number(&s["status"][*m]["value"]).is_some())
    });
    if let (Some(m), Some(v)) = (moves, section.zip(moves).and_then(|(s, m)| number(&s["status"][m]["value"]))) {
        push(out, "SP_GP_DISTANCE", plain(&format!("{v} {}", m.replace('_', " "))));
    }
}

/// The binding section's newest intent not yet judged.
fn in_flight<'a>(snap: &'a Value, binding: &str) -> Option<&'a Value> {
    let prefix = format!("intents/{binding}/");
    snap["intents"]
        .as_array()?
        .iter()
        .filter(|e| e["key"].as_str().is_some_and(|k| k.starts_with(&prefix)))
        .map(|e| &e["intent"])
        .filter(|i| i["judgement"].is_null())
        .max_by_key(|i| i["taken_at"].as_str().unwrap_or(""))
}

/// The supervisor's merge quotes every value, so only a line break could escape it; `@` in a step's
/// `member@tip` target must survive, which the allowlist in `quoting::sanitize` would not allow.
fn plain(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn section_ref(s: &Value) -> String {
    format!("{}/{}", s["kind"].as_str().unwrap_or("?"), s["metadata"]["name"].as_str().unwrap_or("?"))
}

/// A JSON number as text, whole values without a fraction; `None` for anything that is not one.
fn number(v: &Value) -> Option<String> {
    let f = v.as_f64()?;
    Some(if f.fract() == 0.0 { format!("{f:.0}") } else { format!("{f:.1}") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::scripted::client;
    use serde_json::json;

    fn section(kind: &str, name: &str, status: serde_json::Value, observed: Option<&str>, actions: serde_json::Value, distance: bool) -> serde_json::Value {
        let mut s = json!({ "apiVersion": "spira/v1", "kind": kind, "metadata": { "name": name }, "status": status, "actions": actions });
        if let Some(at) = observed {
            s["metadata"]["observedAt"] = json!(at);
        }
        if distance {
            s["distance"] = json!({ "fn": "x", "open": 1.0, "close": 0.0 });
        }
        json!({ "key": format!("telemetry/sections/{kind}/{name}"), "section": s })
    }

    fn snapshot() -> String {
        let hold = json!([{ "name": "hold-member", "class": "local", "step": "s", "moves": "repeat_identity_open", "addresses": [], "expected_effect": 1.0, "horizon": "10m" }]);
        json!({
            "list_rev": 9,
            "undecodable": [],
            "knobs": {},
            "sections": [
                section("RoundController", "pokebot", json!({ "repeat_identity_open": { "value": 1.0, "window": "1h" } }), Some("2026-11-12T14:09:48Z"), hold, true),
                section("Lifecycle", "all", json!({ "landed_24h": { "value": 80.0, "window": "24h" } }), Some("2026-11-12T14:09:20Z"), json!([]), false),
                section("Alarms", "all", json!({}), None, json!([]), true),
            ],
            "intents": [
                { "key": "intents/RoundController/pokebot/i1", "mod_rev": 3, "intent": { "action": "old", "target": "b-1@aaaa", "taken_at": "2026-11-12T13:00:00Z", "horizon": "5m", "moves": "repeat_identity_open", "judgement": { "result": "fulfilled" } } },
                { "key": "intents/RoundController/pokebot/i2", "mod_rev": 8, "intent": { "action": "hold-member", "target": "b-17@3f2a", "taken_at": "2026-11-12T14:01:00Z", "horizon": "10m", "moves": "repeat_identity_open", "ack": { "at": "2026-11-12T14:02:11Z", "result": "applied" } } }
            ],
            "pass": {
                "n": 41, "at": "2026-11-12T14:09:45Z", "binding": "RoundController/pokebot",
                "goodput": { "ratio": 0.6, "rate_per_day": 3.2 },
                "gaps": [{ "section": "RoundController/pokebot", "distance": 1.0, "score": 2.0 }],
                "ages": {
                    "RoundController/pokebot": { "age_secs": 12, "freshness": { "state": "Fresh" } },
                    "Lifecycle/all": { "age_secs": 40, "freshness": { "state": "Fresh" } },
                    "Alarms/all": { "age_secs": null, "freshness": { "state": "Stale", "reason": "no status has been published" } }
                }
            }
        })
        .to_string()
    }

    fn keys(answer: &str) -> std::collections::HashMap<String, String> {
        let (c, _) = client(&[("section snapshot", 0, answer)]);
        keys_on(&c).into_iter().collect()
    }

    #[test]
    fn the_pass_record_becomes_goodput_age_binding_and_the_step_in_flight() {
        let k = keys(&snapshot());
        let want = [
            ("SP_GP_RATE", "3.2"),
            ("SP_GP_PASS_AT", "1794492585"),
            ("SP_GP_BINDING", "RoundController/pokebot"),
            ("SP_GP_DISTANCE", "1 repeat identity open"),
            ("SP_GP_STEP", "hold-member b-17@3f2a"),
            ("SP_GP_STEP_ACK", "14:02"),
            ("SP_GP_STEP_HORIZON", "10m"),
        ];
        for (key, v) in want {
            assert_eq!(k.get(key).map(String::as_str), Some(v), "{key}: {k:?}");
        }
    }

    #[test]
    fn each_section_carries_its_distance_age_and_freshness() {
        let k = keys(&snapshot());
        assert_eq!(k["SP_GP_SEC_N"], "3");
        let row = |i: usize| ["REF", "DIST", "AGE", "FRESH"].map(|f| k.get(&format!("SP_GP_SEC{i}_{f}")).cloned());
        assert_eq!(row(0), [Some("RoundController/pokebot".into()), Some("1".into()), Some("12".into()), Some("fresh".into())]);
        assert_eq!(row(1), [Some("Lifecycle/all".into()), None, Some("40".into()), Some("fresh".into())], "a section with no distance function has none to show");
        assert_eq!(row(2), [Some("Alarms/all".into()), None, None, Some("stale".into())], "an unpublished section has no age and no distance");
    }

    #[test]
    fn landed_in_24h_is_the_lifecycle_sections_windowed_count() {
        assert_eq!(keys(&snapshot())["SP_LC_LANDED_24H"], "80");
        let warming = snapshot().replace(r#"{"value":80.0,"window":"24h"}"#, r#""WarmingUp""#);
        assert!(!keys(&warming).contains_key("SP_LC_LANDED_24H"), "a window still warming up is not a count");
    }

    #[test]
    fn the_floor_is_the_goodput_target() {
        let with_floor = snapshot().replace(
            r#""status":{"landed_24h""#,
            r#""spec":{"goodput_rate_floor":{"value":5.0,"statute":"s"}},"status":{"landed_24h""#,
        );
        assert_eq!(keys(&with_floor)["SP_GP_TARGET"], "5");
        assert!(!keys(&snapshot()).contains_key("SP_GP_TARGET"));
    }

    #[test]
    fn without_a_pass_record_no_pass_key_is_written_and_no_zero_stands_in() {
        let mut snap: serde_json::Value = serde_json::from_str(&snapshot()).unwrap();
        snap["pass"] = serde_json::Value::Null;
        let k = keys(&snap.to_string());
        for key in ["SP_GP_PASS_AT", "SP_GP_RATE", "SP_GP_BINDING", "SP_GP_DISTANCE", "SP_GP_STEP"] {
            assert!(!k.contains_key(key), "{key}: {k:?}");
        }
        assert_eq!(k["SP_GP_SEC_N"], "3", "the sections are still read");
        assert!(k.keys().all(|key| !key.ends_with("_DIST") && !key.ends_with("_AGE")), "no distance or age without a pass record to measure them: {k:?}");
    }

    #[test]
    fn a_snapshot_that_cannot_be_read_writes_nothing() {
        let (c, _) = client(&[("section snapshot", 1, "")]);
        assert!(keys_on(&c).is_empty());
        assert!(keys("not json").is_empty());
    }

    #[test]
    fn a_pass_with_no_open_gap_has_no_binding_section() {
        let idle = snapshot().replace(r#""binding":"RoundController/pokebot""#, r#""binding":null"#);
        let k = keys(&idle);
        assert_eq!(k["SP_GP_BINDING"], "-");
        assert!(!k.contains_key("SP_GP_STEP") && !k.contains_key("SP_GP_DISTANCE"), "{k:?}");
    }

    #[test]
    fn with_no_intent_in_flight_the_distance_comes_from_the_binding_sections_own_action() {
        let mut snap: serde_json::Value = serde_json::from_str(&snapshot()).unwrap();
        snap["intents"][1]["intent"]["judgement"] = json!({ "result": "failed" });
        let k = keys(&snap.to_string());
        assert_eq!(k["SP_GP_DISTANCE"], "1 repeat identity open");
        assert!(!k.contains_key("SP_GP_STEP"), "every intent is judged: {k:?}");
    }

    #[test]
    fn a_binding_the_snapshot_does_not_list_has_no_distance_and_an_unparseable_time_has_no_age() {
        let mut snap: serde_json::Value = serde_json::from_str(&snapshot()).unwrap();
        snap["pass"]["binding"] = json!("Ghost/none");
        snap["pass"]["at"] = json!("yesterday");
        snap["intents"][1]["intent"]["ack"] = serde_json::Value::Null;
        let k = keys(&snap.to_string());
        assert_eq!(k["SP_GP_BINDING"], "Ghost/none");
        assert!(!k.contains_key("SP_GP_DISTANCE") && !k.contains_key("SP_GP_PASS_AT"), "an unparseable time is no time: {k:?}");
        snap["pass"]["binding"] = json!("RoundController/pokebot");
        let k = keys(&snap.to_string());
        assert!(k.contains_key("SP_GP_STEP") && !k.contains_key("SP_GP_STEP_ACK"), "a step not yet acked names no ack time: {k:?}");
    }

    #[test]
    fn a_line_break_in_a_value_cannot_start_a_second_key() {
        assert_eq!(plain("a\nSP_X=1"), "a SP_X=1");
        assert_eq!(number(&json!("x")), None);
        assert_eq!(number(&json!(0.25)).as_deref(), Some("0.2"));
    }
}
