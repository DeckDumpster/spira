//! `Gate`: how much of the last hour's gating ended without a verdict, written by the
//! gate-worker. A term only: its concurrency is `host`'s shedding lever, not its own.

use serde::Deserialize;
use serde_json::Value;

use super::{typed, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target};

pub const NAME: &str = "Gate";
pub const OWNER: &str = "gate-worker";
/// The one Gate section.
pub const SECTION: &str = "gate";

pub const NO_VERDICT_SHARE: &str = "no_verdict_share_1h";
pub const NO_VERDICT_HOURS_SHARE: &str = "no_verdict_hours_share_1h";
pub const NO_VERDICT_BY_CAUSE: &str = "no_verdict_by_cause";
pub const GATES: &str = "gates_1h";

pub const NO_VERDICT_SHARE_SPEC: &str = "no_verdict_share";

/// The distance's literal follows the spec's target (a test holds them together).
pub const DISTANCE: &str = "max(0, no_verdict_share_1h - 0.05)";

/// The section is a gap once its distance passes this.
pub const OPEN: f64 = 0.05;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub no_verdict_share: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub no_verdict_share_1h: Option<Numeric>,
    pub no_verdict_hours_share_1h: Option<Numeric>,
    pub no_verdict_by_cause: Option<Vec<CauseCount>>,
    pub gates_1h: Option<Numeric>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct CauseCount {
    pub cause: String,
    pub count: u64,
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
        (NO_VERDICT_SHARE, FieldShape::Numeric),
        (NO_VERDICT_HOURS_SHARE, FieldShape::Numeric),
        (NO_VERDICT_BY_CAUSE, FieldShape::List),
        (GATES, FieldShape::Numeric),
    ],
    check_spec,
    check_status,
};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::section::{admit_publish, parse_section};

    fn section(share: f64) -> Value {
        json!({
            "apiVersion": "spira/v1",
            "kind": NAME,
            "metadata": { "name": SECTION, "owner": OWNER, "observedAt": "2026-10-31T04:10:00Z", "target_latency": "10m", "max_age": "1h" },
            "status": {
                "observedGeneration": 0,
                NO_VERDICT_SHARE: { "value": share, "window": "1h" },
                NO_VERDICT_HOURS_SHARE: { "value": 0.5, "window": "1h" },
                GATES: { "value": 20.0, "window": "1h" },
                NO_VERDICT_BY_CAUSE: [{ "cause": "died", "count": 3 }],
            },
            "flow": { "consumes": ["host/scratch", "host/io"], "produces": ["gate/verdicts"] },
            "distance": { "fn": DISTANCE, "open": OPEN, "close": 0.0 },
            "actions": [],
        })
    }

    #[test]
    fn the_distance_is_the_share_over_its_target_and_zero_under_it() {
        for (share, want) in [(0.20, 0.15), (0.03, 0.0)] {
            let s = parse_section(&section(share)).unwrap();
            let kind = admit_publish(&s).unwrap();
            assert_eq!(kind.name, NAME);
            assert!((s.distance_value(kind).unwrap().unwrap() - want).abs() < 1e-9);
        }
    }

    #[test]
    fn a_field_the_kind_does_not_declare_is_refused_by_name() {
        let mut v = section(0.1);
        v["status"]["queue_depth"] = json!({ "value": 1.0, "window": "level" });
        let r = admit_publish(&parse_section(&v).unwrap()).unwrap_err();
        assert!(r.rule.contains("queue_depth"), "{r:?}");
    }

    #[test]
    fn the_shipped_spec_seeds_the_target_and_the_distance_follows_it() {
        let fragment = crate::producer::parse_fragment(include_str!("../../examples/gate.toml")).unwrap();
        let composite = crate::compose::compose(&[fragment]).unwrap();
        let specs = crate::compose::section_specs(&composite);
        let s = specs.iter().find(|s| s["kind"] == NAME).expect("a Gate spec is seeded");
        assert_eq!(s["metadata"]["name"], SECTION);
        let target = s["spec"][NO_VERDICT_SHARE_SPEC]["value"].as_f64().unwrap();
        assert_eq!(target, 0.05);
        assert!(DISTANCE.contains(&target.to_string()), "the distance literal {DISTANCE:?} follows the spec's {target}");
        assert!(check_spec(&json!({ "share": { "value": 0.05, "statute": "x" } })).is_err());
    }
}
