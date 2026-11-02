//! `RoundController`: one repo's rounds, written by the round controller. The reconciler's
//! design owns this schema; the controller owns the publisher, its actions and its applier.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use super::{typed, ConditionDecl, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target, Windowed};

pub const NAME: &str = "RoundController";
pub const OWNER: &str = "round-controller";

pub const STACKED_ON_UNDRIVEN_PARENT: &str = "StackedOnUndrivenParent";
pub const REPEAT_IDENTITY: &str = "RepeatIdentity";
pub const SLOTS_SATURATED: &str = "SlotsSaturated";
pub const LANE_BLOCKED: &str = "LaneBlocked";
pub const HALTED: &str = "Halted";
pub const STOPPED: &str = "Stopped";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub green_lands_within: Option<Target<String>>,
    pub repeat_identity_open: Option<Target<f64>>,
    pub waiting_with_no_slot: Option<Target<f64>>,
    pub lanes_blocked: Option<Target<f64>>,
    /// The stack depth the repo is run at.
    pub concurrency: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub green_unlanded: Option<Vec<Value>>,
    pub green_unlanded_minutes: Option<Numeric>,
    pub repeat_identity_open: Option<Numeric>,
    pub waiting_with_no_slot: Option<Numeric>,
    pub rounds_since_landing: Option<Numeric>,
    pub lanes_blocked: Option<Numeric>,
    /// Non-terminal rounds by lifecycle state.
    pub rounds: Option<BTreeMap<String, u64>>,
    pub leased: Option<Vec<Value>>,
    pub orphans: Option<Vec<Value>>,
    pub orphans_not_redriven: Option<Numeric>,
    pub lane_faults: Option<Vec<Value>>,
    pub slots: Option<Vec<Value>>,
    pub stack_depth: Option<Numeric>,
    pub repeat_identity_passes_24h: Option<Windowed>,
    pub judge_passes_24h: Option<Windowed>,
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
    conditions: &[
        ConditionDecl {
            type_: STACKED_ON_UNDRIVEN_PARENT,
            page_on_raise: false,
        },
        ConditionDecl {
            type_: REPEAT_IDENTITY,
            page_on_raise: false,
        },
        ConditionDecl {
            type_: SLOTS_SATURATED,
            page_on_raise: false,
        },
        ConditionDecl {
            type_: LANE_BLOCKED,
            page_on_raise: true,
        },
        ConditionDecl {
            type_: HALTED,
            page_on_raise: false,
        },
        ConditionDecl {
            type_: STOPPED,
            page_on_raise: false,
        },
    ],
    fields: &[
        ("green_unlanded", FieldShape::List),
        ("green_unlanded_minutes", FieldShape::Numeric),
        ("repeat_identity_open", FieldShape::Numeric),
        ("waiting_with_no_slot", FieldShape::Numeric),
        ("rounds_since_landing", FieldShape::Numeric),
        ("lanes_blocked", FieldShape::Numeric),
        ("rounds", FieldShape::List),
        ("leased", FieldShape::List),
        ("orphans", FieldShape::List),
        ("orphans_not_redriven", FieldShape::Numeric),
        ("lane_faults", FieldShape::List),
        ("slots", FieldShape::List),
        ("stack_depth", FieldShape::Numeric),
        ("repeat_identity_passes_24h", FieldShape::Windowed),
        ("judge_passes_24h", FieldShape::Windowed),
    ],
    check_spec,
    check_status,
};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::section::{admit_publish, parse_section};

    fn section(status: serde_json::Value) -> serde_json::Value {
        json!({
            "apiVersion": "spira/v1",
            "kind": "RoundController",
            "metadata": {
                "name": "repo-b", "owner": "round-controller",
                "observedAt": "2026-10-31T04:10:00Z", "target_latency": "5m", "max_age": "15m",
            },
            "status": status,
            "flow": { "consumes": ["vm-pool/slots", "gate/verdicts", "lifecycle/submitted"], "produces": ["lifecycle/landed"] },
            "distance": { "fn": "max(0, green_unlanded_minutes) / 5 + repeat_identity_open + lanes_blocked", "open": 1.0, "close": 0.5 },
            "actions": [
                { "name": "hold-member", "class": "local", "step": "one member at one tip", "moves": "repeat_identity_open",
                  "addresses": ["RepeatIdentity"], "expected_effect": -1.0, "horizon": "10m" },
                { "name": "concurrency", "class": "capacity-raise", "step": "+1 slot", "moves": "waiting_with_no_slot",
                  "addresses": ["SlotsSaturated"], "expected_effect": -1.0, "horizon": "1h" },
                { "name": "concurrency", "class": "capacity-lower", "step": "-1 slot", "moves": "waiting_with_no_slot",
                  "addresses": ["SlotsSaturated"], "expected_effect": 1.0, "horizon": "1h" },
            ],
        })
    }

    #[test]
    fn a_stacked_repos_section_is_admitted_and_its_distance_reads_its_levels() {
        let level = |v: f64| json!({ "value": v, "window": "level" });
        let v = section(json!({
            "observedGeneration": 0,
            "green_unlanded": [{ "bead": "b-1245", "batch": "repo-b-20261031T040200Z", "age_minutes": 10 }],
            "green_unlanded_minutes": level(10.0),
            "repeat_identity_open": level(1.0),
            "waiting_with_no_slot": level(0.0),
            "rounds_since_landing": level(3.0),
            "lanes_blocked": level(0.0),
            "conditions": [{ "type": "StackedOnUndrivenParent", "status": "True",
                             "reason": "parent repo-b-20261031T031222Z ATTRIBUTING, unleased", "lastTransitionTime": "2026-10-31T04:10:00Z" }],
        }));
        let s = parse_section(&v).unwrap();
        let kind = admit_publish(&s).unwrap();
        assert_eq!(kind.name, super::NAME);
        assert!((s.distance_value(kind).unwrap().unwrap() - 3.0).abs() < 1e-9);
        assert!(kind.condition(super::LANE_BLOCKED).unwrap().page_on_raise);
    }

    #[test]
    fn a_field_the_kind_does_not_declare_is_refused_by_name() {
        let v = section(json!({ "observedGeneration": 0, "wedgedness": { "value": 2.0, "window": "level" } }));
        let r = admit_publish(&parse_section(&v).unwrap()).unwrap_err();
        assert!(r.field.starts_with("status"), "{r:?}");
        assert!(r.rule.contains("wedgedness"), "{r:?}");
    }
}
