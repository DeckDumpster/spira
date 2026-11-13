//! `goodput_keys` — the reconciler's pass record and each section's distance and age, from one
//! `spira-lc section snapshot`. A reading the snapshot does not carry is an absent key, never 0.

use super::Kv;
use spira_store::lc_call::LcClient;

pub fn goodput_keys() -> Kv {
    let Some(client) = crate::store::client() else { return Kv::new() };
    keys_on(&client, crate::io::now())
}

pub fn keys_on(_client: &LcClient, _now: i64) -> Kv {
    Kv::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::scripted::client;
    use serde_json::json;

    const NOW: i64 = 1_794_492_600; // 2026-11-12T14:10:00Z

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
        keys_on(&c, NOW).into_iter().collect()
    }

    #[test]
    fn the_pass_record_becomes_goodput_age_binding_and_the_step_in_flight() {
        let k = keys(&snapshot());
        let want = [
            ("SP_GP_RATE", "3.2"),
            ("SP_GP_PASS_AT", "2026-11-12T14:09:45Z"),
            ("SP_GP_PASS_AGE_S", "15"),
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
        let none = snapshot().replace(&snapshot()[snapshot().find(r#""pass":"#).unwrap()..snapshot().len() - 1], r#""pass":null"#);
        let k = keys(&none);
        assert!(k.get("SP_GP_PASS_AT").is_none() && k.get("SP_GP_PASS_AGE_S").is_none() && k.get("SP_GP_RATE").is_none() && k.get("SP_GP_BINDING").is_none(), "{k:?}");
        assert_eq!(k["SP_GP_SEC_N"], "3", "the sections are still read");
        assert!(k.keys().filter(|key| key.starts_with("SP_GP_SEC") && key.ends_with("_DIST")).count() == 0 || k.values().all(|v| v != "0"), "{k:?}");
    }

    #[test]
    fn a_snapshot_that_cannot_be_read_writes_nothing() {
        let (c, _) = client(&[("section snapshot", 1, "")]);
        assert!(keys_on(&c, NOW).is_empty());
        assert!(keys("not json").is_empty());
    }

    #[test]
    fn a_pass_with_no_open_gap_has_no_binding_section() {
        let idle = snapshot().replace(r#""binding":"RoundController/pokebot""#, r#""binding":null"#);
        let k = keys(&idle);
        assert_eq!(k["SP_GP_BINDING"], "-");
        assert!(!k.contains_key("SP_GP_STEP") && !k.contains_key("SP_GP_DISTANCE"), "{k:?}");
    }
}
