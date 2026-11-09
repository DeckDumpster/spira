//! `DwarfFleet`: how busy the dwarves are while work waits, written by the sentinel's call
//! pass. Its two levers step the live-dwarf ceiling by one; the applier compares the ceiling
//! against the intent's `from` before it sets it.

use serde::Deserialize;
use serde_json::Value;

use super::{typed, ConditionDecl, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target, Windowed};

pub const NAME: &str = "DwarfFleet";
pub const OWNER: &str = "sentinel";
/// The one DwarfFleet section.
pub const SECTION: &str = "fleet";

pub const SATURATED: &str = "Saturated";
pub const HOST_SHEDDING: &str = "HostShedding";
pub const RAISE: &str = "max-live-dwarves-raise";
pub const LOWER: &str = "max-live-dwarves-lower";

pub const UTILISATION: &str = "utilisation_1h";
pub const UTILISATION_SPEC: &str = "utilisation_while_ready";
pub const MINUTES_PER_LANDED: &str = "minutes_per_landed_24h";
pub const NEVER_SUBMITTED: &str = "never_submitted_share_24h";

/// The distance's expression over [`UTILISATION`] alone: the other terms are detail.
pub const DISTANCE: &str = "max(0, utilisation_1h - 0.85)";

/// The config key the actions step, as `spira-config` names it.
pub const CEILING_KEY: &str = "spira.max_live_dwarves";
pub const CEILING_MAX_KEY: &str = "spira.max_dwarves";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub utilisation_while_ready: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub utilisation_1h: Option<Numeric>,
    pub minutes_per_landed_24h: Option<Windowed>,
    pub never_submitted_share_24h: Option<Windowed>,
    pub live: Option<Numeric>,
    pub ceiling: Option<Numeric>,
    pub claimable_ready: Option<Numeric>,
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
    conditions: &[
        ConditionDecl {
            type_: SATURATED,
            page_on_raise: false,
        },
        ConditionDecl {
            type_: HOST_SHEDDING,
            page_on_raise: false,
        },
    ],
    fields: &[
        (UTILISATION, FieldShape::Numeric),
        (MINUTES_PER_LANDED, FieldShape::Windowed),
        (NEVER_SUBMITTED, FieldShape::Windowed),
        ("live", FieldShape::Numeric),
        ("ceiling", FieldShape::Numeric),
        ("claimable_ready", FieldShape::Numeric),
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
            "metadata": { "name": SECTION, "owner": OWNER, "observedAt": "2026-10-31T04:10:00Z", "target_latency": "5m", "max_age": "15m" },
            "status": status,
            "flow": { "consumes": ["supply/landable", "host/scratch"], "produces": ["lifecycle/submitted"] },
            "distance": { "fn": DISTANCE, "open": 0.05, "close": 0.0 },
            "actions": [
                { "name": RAISE, "class": "capacity-raise", "step": "+1", "moves": UTILISATION,
                  "addresses": [SATURATED], "expected_effect": -0.1, "horizon": "65m" },
                { "name": LOWER, "class": "capacity-lower", "step": "-1", "moves": UTILISATION,
                  "addresses": [HOST_SHEDDING], "expected_effect": 0.1, "horizon": "65m" },
            ],
        })
    }

    #[test]
    fn the_kind_admits_its_section_and_its_distance_reads_utilisation_alone() {
        let v = section(json!({
            "observedGeneration": 0,
            "utilisation_1h": { "value": 0.95, "window": "1h" },
            "minutes_per_landed_24h": { "value": 59.0, "window": "24h" },
            "never_submitted_share_24h": { "unobservable": "no run ended in the window", "window": "24h" },
            "live": { "value": 4.0, "window": "level" },
            "ceiling": { "value": 4.0, "window": "level" },
            "claimable_ready": { "value": 7.0, "window": "level" },
        }));
        let s = parse_section(&v).unwrap();
        let kind = admit_publish(&s).unwrap();
        assert_eq!(kind.name, NAME);
        assert!((s.distance_value(kind).unwrap().unwrap() - 0.10).abs() < 1e-9);
    }

    #[test]
    fn a_field_the_kind_does_not_declare_is_refused_by_name() {
        let v = section(json!({ "observedGeneration": 0, "idle_dwarves": { "value": 2.0, "window": "level" } }));
        let r = admit_publish(&parse_section(&v).unwrap()).unwrap_err();
        assert!(r.rule.contains("idle_dwarves"), "{r:?}");
    }

    #[test]
    fn the_shipped_spec_seeds_the_utilisation_target_under_its_statute() {
        let fragment = crate::producer::parse_fragment(include_str!("../../examples/dwarf-fleet.toml")).unwrap();
        let composite = crate::compose::compose(&[fragment]).unwrap();
        let specs = crate::compose::section_specs(&composite);
        let s = specs.iter().find(|s| s["kind"] == NAME).expect("a DwarfFleet spec is seeded");
        assert_eq!(s["metadata"]["name"], SECTION);
        assert_eq!(
            s["spec"][UTILISATION_SPEC],
            json!({ "value": 0.85, "statute": "law-a-concurrency-ceiling-names-its-measurement" })
        );
        assert!(check_spec(&json!({ "utilisation": { "value": 0.85, "statute": "x" } })).is_err());
        let target = s["spec"][UTILISATION_SPEC]["value"].as_f64().unwrap();
        assert!(DISTANCE.contains(&target.to_string()), "the distance literal {DISTANCE:?} follows the spec's {target}");
    }
}
