//! `Lifecycle`: what the loop produces. Goodput ratio and rate per hour and per trailing day
//! of unhalted time, in total and per repo, and the CERTIFIED dwell. A term: it declares no
//! actions, so it ranks and is shown and never steps.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{typed, ConditionDecl, FieldShape, SchemaError, SectionKind};
use crate::section::{Target, Windowed};

pub const NAME: &str = "Lifecycle";
pub const OWNER: &str = "spira-lc";
/// The one Lifecycle section: the whole loop's.
pub const SECTION: &str = "all";
pub const BELOW_FLOOR: &str = "BelowFloor";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub goodput_rate_floor: Option<Target<f64>>,
    pub certified_dwell_p95_secs: Option<Target<f64>>,
}

/// The windowed goodput fields, as the total and as each repo carries them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goodput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_ratio_1h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_rate_1h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_ratio_24h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_rate_24h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_1h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_24h: Option<Windowed>,
}

/// The typed signal a harm was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Signal {
    LandingStop,
    Revert,
    RedWithInnocents,
    RepeatEject,
    OverCap,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub signal: Signal,
    pub at: String,
    pub minutes: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<String>,
}

/// One bead that harmed the loop in the trailing 24 h of unhalted time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Harm {
    pub bead: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub cost_minutes: f64,
    pub evidence: Vec<Evidence>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_ratio_1h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_rate_1h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_ratio_24h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodput_rate_24h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_1h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_24h: Option<Windowed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certified_dwell_p95_secs: Option<Windowed>,
    /// Each repo's windowed goodput fields.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub repos: BTreeMap<String, Goodput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harmful: Vec<Harm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warming_up_until: Option<String>,
}

impl Status {
    /// The total's windowed fields, as one repo carries them.
    pub fn set_total(&mut self, g: Goodput) {
        self.goodput_ratio_1h = g.goodput_ratio_1h;
        self.goodput_rate_1h = g.goodput_rate_1h;
        self.goodput_ratio_24h = g.goodput_ratio_24h;
        self.goodput_rate_24h = g.goodput_rate_24h;
        self.landed_1h = g.landed_1h;
        self.landed_24h = g.landed_24h;
    }
}

fn check_spec(v: &Value) -> Result<(), SchemaError> {
    typed::<Spec>(v)
}

fn check_status(v: &Value) -> Result<(), SchemaError> {
    typed::<Status>(v)
}

pub const KIND: SectionKind = SectionKind {
    name: NAME,
    owner: OWNER,
    escalation: None,
    conditions: &[ConditionDecl {
        type_: BELOW_FLOOR,
        page_on_raise: false,
    }],
    fields: &[
        ("goodput_ratio_1h", FieldShape::Windowed),
        ("goodput_rate_1h", FieldShape::Windowed),
        ("goodput_ratio_24h", FieldShape::Windowed),
        ("goodput_rate_24h", FieldShape::Windowed),
        ("landed_1h", FieldShape::Windowed),
        ("landed_24h", FieldShape::Windowed),
        ("certified_dwell_p95_secs", FieldShape::Windowed),
        ("repos", FieldShape::List),
        ("harmful", FieldShape::List),
        ("warming_up_until", FieldShape::List),
    ],
    check_spec,
    check_status,
};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::section::{admit_publish, parse_section};

    fn section(status: Value) -> Value {
        json!({
            "apiVersion": "spira/v1",
            "kind": NAME,
            "metadata": {
                "name": SECTION, "owner": OWNER,
                "observedAt": "2026-10-31T19:39:58Z", "target_latency": "5m", "max_age": "15m",
            },
            "status": status,
            "flow": { "consumes": ["round-controller/landed"], "produces": [] },
            "distance": { "fn": "max(0, 1 - goodput_rate_24h / 150)", "open": 0.05, "close": 0.02 },
        })
    }

    fn admitted(status: Value) -> Result<(), String> {
        let s = parse_section(&section(status)).map_err(|r| r.to_string())?;
        admit_publish(&s).map(|_| ()).map_err(|r| r.to_string())
    }

    fn warm() -> Value {
        json!({
            "observedGeneration": 1,
            "goodput_ratio_24h": { "value": 0.5, "window": "24h" },
            "goodput_rate_24h": { "value": 40.0, "window": "24h" },
            "landed_24h": { "value": 80.0, "window": "24h" },
            "goodput_ratio_1h": { "unobservable": "no landings in window", "window": "1h" },
            "goodput_rate_1h": { "value": 0.0, "window": "1h" },
            "certified_dwell_p95_secs": { "value": 900.0, "window": "24h" },
            "repos": { "harness": { "landed_24h": { "value": 80.0, "window": "24h" } } },
            "harmful": [{ "bead": "b-1", "repo": "harness", "cost_minutes": 647.0,
                          "evidence": [{ "signal": "landing-stop", "at": "2026-10-31T11:43:00Z", "minutes": 647.0 }] }],
            "conditions": [{ "type": "BelowFloor", "status": "True", "reason": "goodput rate 40.0/day", "lastTransitionTime": "2026-10-31T19:00:00Z" }],
        })
    }

    #[test]
    fn a_lifecycle_section_with_a_24h_distance_is_a_term_and_is_admitted() {
        admitted(warm()).unwrap();
        let s = parse_section(&section(warm())).unwrap();
        let v = s.distance_value(&KIND).unwrap().unwrap();
        assert!((v - (1.0 - 40.0 / 150.0)).abs() < 1e-9, "{v}");
    }

    #[test]
    fn a_warming_field_is_admitted_but_is_never_a_term() {
        let mut st = warm();
        st["goodput_rate_1h"] = json!("WarmingUp");
        st["landed_1h"] = json!("WarmingUp");
        st["warming_up_until"] = json!("2026-11-01T19:00:00Z");
        admitted(st.clone()).unwrap();

        st["goodput_rate_24h"] = json!("WarmingUp");
        let err = admitted(st).unwrap_err();
        assert!(err.contains("distance.fn"), "a distance cannot read a warming field: {err}");
    }

    #[test]
    fn a_malformed_windowed_field_or_an_unknown_field_is_refused() {
        for bad in [json!("warming"), json!({ "unobservable": " ", "window": "24h" }), json!({ "unobservable": "x", "window": "soon" }), json!(3)] {
            let mut st = warm();
            st["landed_1h"] = bad.clone();
            let err = admitted(st).unwrap_err();
            assert!(err.starts_with("status.landed_1h"), "{bad}: {err}");
        }
        let mut st = warm();
        st["landed_7d"] = json!({ "value": 1.0, "window": "7d" });
        assert!(admitted(st).unwrap_err().contains("landed_7d"));
        let mut st = warm();
        st["repos"]["harness"]["bogus"] = json!(1);
        assert!(admitted(st).unwrap_err().contains("bogus"));
        let mut st = warm();
        st["conditions"][0]["type"] = json!("Wedged");
        assert!(admitted(st).unwrap_err().contains("conditions"));
    }

    #[test]
    fn the_shipped_lifecycle_spec_seeds_the_floor_and_the_dwell_target() {
        let fragment = crate::producer::parse_fragment(include_str!("../../examples/lifecycle.toml")).unwrap();
        let composite = crate::compose::compose(&[fragment]).unwrap();
        let specs = crate::compose::section_specs(&composite);
        let lc = specs.iter().find(|s| s["kind"] == NAME).expect("a Lifecycle spec is seeded");
        assert_eq!(lc["metadata"]["name"], SECTION);
        assert_eq!(lc["spec"]["goodput_rate_floor"], json!({ "value": 150, "statute": "law-throughput-floor" }));
        assert_eq!(lc["spec"]["certified_dwell_p95_secs"]["value"], 1800);
    }

    #[test]
    fn the_spec_names_the_floor_and_the_dwell_target_and_nothing_else() {
        let ok = json!({ "goodput_rate_floor": { "value": 150.0, "statute": "law-throughput-floor" },
                         "certified_dwell_p95_secs": { "value": 1800.0, "statute": "law-throughput-floor" } });
        check_spec(&ok).unwrap();
        assert!(check_spec(&json!({ "floor": { "value": 1.0, "statute": "x" } })).is_err());
    }
}
