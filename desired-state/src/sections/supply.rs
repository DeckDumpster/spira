//! `Supply`: how much landable work was filed in the last day against the goodput target,
//! written by the groomer. A term only: a filing quota would invite filler beads, and drafting
//! capacity is the floor's standing ask, not an action.

use serde::Deserialize;
use serde_json::Value;

use super::{typed, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target};

pub const NAME: &str = "Supply";
pub const OWNER: &str = "groomer";
/// The one Supply section.
pub const SECTION: &str = "supply";

pub const FILED: &str = "filed_24h";
pub const LANDABLE: &str = "landable_24h";
pub const DROPPED_SHARE: &str = "dropped_share_24h";
pub const CLAIMABLE_READY: &str = "claimable_ready";

pub const LANDABLE_SPEC: &str = "landable_filed_per_day";
/// The spec's seeded target; the distance's literal follows it (a test holds them together).
pub const TARGET_PER_DAY: f64 = 150.0;

pub const DISTANCE: &str = "max(0, 1 - landable_24h / 150)";

/// The section is a gap once its distance passes this.
pub const OPEN: f64 = 0.05;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub landable_filed_per_day: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub filed_24h: Option<Numeric>,
    pub landable_24h: Option<Numeric>,
    pub dropped_share_24h: Option<Numeric>,
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
    conditions: &[],
    fields: &[
        (FILED, FieldShape::Numeric),
        (LANDABLE, FieldShape::Numeric),
        (DROPPED_SHARE, FieldShape::Numeric),
        (CLAIMABLE_READY, FieldShape::Numeric),
    ],
    check_spec,
    check_status,
};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::section::{admit_publish, parse_section};

    fn section(landable: f64) -> Value {
        json!({
            "apiVersion": "spira/v1",
            "kind": NAME,
            "metadata": { "name": SECTION, "owner": OWNER, "observedAt": "2026-10-31T04:10:00Z", "target_latency": "6h", "max_age": "24h" },
            "status": {
                "observedGeneration": 0,
                FILED: { "value": 201.0, "window": "24h" },
                LANDABLE: { "value": landable, "window": "24h" },
                DROPPED_SHARE: { "value": 0.8, "window": "24h" },
                CLAIMABLE_READY: { "value": 3.0, "window": "level" },
            },
            "flow": { "consumes": [], "produces": ["supply/landable"] },
            "distance": { "fn": DISTANCE, "open": OPEN, "close": 0.0 },
            "actions": [],
        })
    }

    #[test]
    fn the_distance_is_the_shortfall_against_the_target_and_zero_above_it() {
        for (landable, want) in [(75.0, 0.5), (0.0, 1.0), (150.0, 0.0), (300.0, 0.0)] {
            let s = parse_section(&section(landable)).unwrap();
            let d = s.distance_value(admit_publish(&s).unwrap()).unwrap().unwrap();
            assert!((d - want).abs() < 1e-9, "{landable}: {d}");
        }
    }

    #[test]
    fn a_field_the_kind_does_not_declare_is_refused_by_name() {
        let mut v = section(10.0);
        v["status"]["quota"] = json!({ "value": 1.0, "window": "24h" });
        let r = admit_publish(&parse_section(&v).unwrap()).unwrap_err();
        assert!(r.rule.contains("quota"), "{r:?}");
    }

    #[test]
    fn the_shipped_spec_seeds_the_target_and_the_distance_follows_it() {
        let fragment = crate::producer::parse_fragment(include_str!("../../examples/supply.toml")).unwrap();
        let composite = crate::compose::compose(&[fragment]).unwrap();
        let specs = crate::compose::section_specs(&composite);
        let s = specs.iter().find(|s| s["kind"] == NAME).expect("a Supply spec is seeded");
        assert_eq!(s["metadata"]["name"], SECTION);
        let target = s["spec"][LANDABLE_SPEC]["value"].as_f64().unwrap();
        assert_eq!(target, TARGET_PER_DAY);
        assert!(DISTANCE.contains(&target.to_string()[..3]), "the distance literal {DISTANCE:?} follows the spec's {target}");
        assert!(check_spec(&json!({ "landable_per_day": { "value": 1.0, "statute": "x" } })).is_err());
    }
}
