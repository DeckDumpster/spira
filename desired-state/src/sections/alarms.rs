//! `Alarms`: whether every alarm reached someone, written by `incident alarms-pass`. Its one
//! lever re-sends one failed alarm; its own dead-man condition pages on raise.

use serde::Deserialize;
use serde_json::Value;

use super::{typed, ConditionDecl, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target};

pub const NAME: &str = "Alarms";
pub const OWNER: &str = "incident";
/// The one Alarms section.
pub const SECTION: &str = "all";

pub const UNDELIVERED: &str = "Undelivered";
pub const RECONCILER_PASS_STALE: &str = "ReconcilerPassStale";
pub const RETRY_DELIVERY: &str = "retry-delivery";
pub const UNDELIVERED_OPEN: &str = "undelivered_open";
/// `deadman` while the latest pass record is inside the dead-man.
pub const DEADMAN_FRESH: &str = "fresh";
/// `heartbeat_last` when no off-box receiver is configured.
pub const NO_HEARTBEAT_URL: &str = "no heartbeat url configured";
/// `heartbeat_last` begins with this when the last ping did not reach the receiver.
pub const HEARTBEAT_FAILED: &str = "failed: ";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub undelivered_open: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub undelivered_open: Option<Numeric>,
    pub attempts_24h: Option<Numeric>,
    pub delivered_24h: Option<Numeric>,
    pub failed_24h: Option<Numeric>,
    pub pages_sent_24h: Option<Numeric>,
    pub pass_age_secs: Option<Numeric>,
    pub heartbeat_last: Option<String>,
    pub deadman: Option<String>,
    pub undelivered: Option<Vec<Value>>,
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
            type_: UNDELIVERED,
            page_on_raise: false,
        },
        ConditionDecl {
            type_: RECONCILER_PASS_STALE,
            page_on_raise: true,
        },
    ],
    fields: &[
        (UNDELIVERED_OPEN, FieldShape::Numeric),
        ("attempts_24h", FieldShape::Numeric),
        ("delivered_24h", FieldShape::Numeric),
        ("failed_24h", FieldShape::Numeric),
        ("pages_sent_24h", FieldShape::Numeric),
        ("pass_age_secs", FieldShape::Numeric),
        ("heartbeat_last", FieldShape::List),
        ("deadman", FieldShape::List),
        ("undelivered", FieldShape::List),
    ],
    check_spec,
    check_status,
};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_shipped_alarms_spec_seeds_zero_undelivered_under_its_statute() {
        let fragment = crate::producer::parse_fragment(include_str!("../../examples/alarms.toml")).unwrap();
        let composite = crate::compose::compose(&[fragment]).unwrap();
        let specs = crate::compose::section_specs(&composite);
        let a = specs.iter().find(|s| s["kind"] == NAME).expect("an Alarms spec is seeded");
        assert_eq!(a["metadata"]["name"], SECTION);
        assert_eq!(a["spec"][UNDELIVERED_OPEN], json!({ "value": 0, "statute": "law-answers-need-a-delivery-path" }));
        assert!(check_spec(&json!({ "undelivered": { "value": 0.0, "statute": "x" } })).is_err());
        assert!(KIND.condition(RECONCILER_PASS_STALE).unwrap().page_on_raise);
        assert!(!KIND.condition(UNDELIVERED).unwrap().page_on_raise);
    }
}
