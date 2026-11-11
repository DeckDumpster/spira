//! `VmPool`: how much of the last hour's certification passes the VM pool wasted, written by
//! round-vm. A term only: capacity has one knob, the round controller's `concurrency`.

use serde::Deserialize;
use serde_json::Value;

use super::{typed, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target};

pub const NAME: &str = "VmPool";
pub const OWNER: &str = "round-vm";
/// The one VmPool section.
pub const SECTION: &str = "pool";

pub const INFRA_INCOMPLETE: &str = "infra_incomplete_share_1h";
pub const OVER_CAP: &str = "over_cap_share_1h";
pub const PASSES: &str = "passes_1h";
pub const SLOTS_IN_USE: &str = "slots_in_use";

pub const INFRA_INCOMPLETE_SPEC: &str = "infra_incomplete_share";
pub const OVER_CAP_SPEC: &str = "over_cap_share";

/// The distance's literals follow the spec's targets (a test holds them together).
pub const DISTANCE: &str = "max(0, infra_incomplete_share_1h - 0.05) + max(0, over_cap_share_1h - 0.05)";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub infra_incomplete_share: Option<Target<f64>>,
    pub over_cap_share: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub infra_incomplete_share_1h: Option<Numeric>,
    pub over_cap_share_1h: Option<Numeric>,
    pub passes_1h: Option<Numeric>,
    pub slots_in_use: Option<Vec<PoolSlots>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct PoolSlots {
    pub pool: String,
    pub in_use: u64,
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
        (INFRA_INCOMPLETE, FieldShape::Numeric),
        (OVER_CAP, FieldShape::Numeric),
        (PASSES, FieldShape::Numeric),
        (SLOTS_IN_USE, FieldShape::List),
    ],
    check_spec,
    check_status,
};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::section::{admit_publish, parse_section};

    fn section(status: Value, actions: Value) -> Value {
        json!({
            "apiVersion": "spira/v1",
            "kind": NAME,
            "metadata": { "name": SECTION, "owner": OWNER, "observedAt": "2026-10-31T04:10:00Z", "target_latency": "5m", "max_age": "30m" },
            "status": status,
            "flow": { "consumes": ["host/io"], "produces": ["vm-pool/slots"] },
            "distance": { "fn": DISTANCE, "open": 0.05, "close": 0.0 },
            "actions": actions,
        })
    }

    fn status() -> Value {
        json!({
            "observedGeneration": 0,
            INFRA_INCOMPLETE: { "value": 0.30, "window": "1h" },
            OVER_CAP: { "value": 0.08, "window": "1h" },
            PASSES: { "value": 40.0, "window": "1h" },
            SLOTS_IN_USE: [{ "pool": "local", "in_use": 2 }, { "pool": "ec2", "in_use": 0 }],
        })
    }

    #[test]
    fn the_kind_admits_a_term_whose_distance_sums_both_shares_over_their_targets() {
        let s = parse_section(&section(status(), json!([]))).unwrap();
        let kind = admit_publish(&s).unwrap();
        assert_eq!(kind.name, NAME);
        assert!((s.distance_value(kind).unwrap().unwrap() - 0.28).abs() < 1e-9);
    }

    #[test]
    fn a_share_under_its_target_adds_no_distance() {
        let mut st = status();
        st[INFRA_INCOMPLETE]["value"] = json!(0.02);
        st[OVER_CAP]["value"] = json!(0.0);
        let s = parse_section(&section(st, json!([]))).unwrap();
        assert_eq!(s.distance_value(admit_publish(&s).unwrap()).unwrap().unwrap(), 0.0);
    }

    #[test]
    fn a_field_the_kind_does_not_declare_is_refused_by_name() {
        let mut st = status();
        st["free_slots"] = json!({ "value": 1.0, "window": "level" });
        let r = admit_publish(&parse_section(&section(st, json!([]))).unwrap()).unwrap_err();
        assert!(r.rule.contains("free_slots"), "{r:?}");
    }

    #[test]
    fn the_shipped_spec_seeds_both_targets_and_the_distance_follows_them() {
        let fragment = crate::producer::parse_fragment(include_str!("../../examples/vm-pool.toml")).unwrap();
        let composite = crate::compose::compose(&[fragment]).unwrap();
        let specs = crate::compose::section_specs(&composite);
        let s = specs.iter().find(|s| s["kind"] == NAME).expect("a VmPool spec is seeded");
        assert_eq!(s["metadata"]["name"], SECTION);
        for key in [INFRA_INCOMPLETE_SPEC, OVER_CAP_SPEC] {
            let target = s["spec"][key]["value"].as_f64().unwrap();
            assert_eq!(target, 0.05);
            assert!(DISTANCE.contains(&target.to_string()), "the distance literal {DISTANCE:?} follows the spec's {target}");
        }
        assert!(check_spec(&json!({ "infra_share": { "value": 0.05, "statute": "x" } })).is_err());
    }
}
