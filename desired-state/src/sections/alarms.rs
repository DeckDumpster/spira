//! `Alarms`: whether every alarm reached someone, written by `incident alarms-pass`. Its one
//! lever re-sends one failed alarm; its own dead-man condition pages on raise.

use serde::{Deserialize, Serialize};
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
/// The dead-man check's state, typed beside its prose `deadman`.
pub const DEADMAN_STATE: &str = "deadman_state";
/// The off-box heartbeat's last outcome, typed beside its prose `heartbeat_last`.
pub const HEARTBEAT_STATE: &str = "heartbeat_state";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeadManState {
    /// The latest pass record is inside the dead-man.
    Fresh,
    /// The operator turned the pass off, or stopped the observability plane: nothing is judged.
    Waiting,
    /// The latest pass record is past the dead-man, or there is none.
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HeartbeatState {
    /// No off-box receiver is configured.
    Unconfigured,
    Delivered,
    Failed,
}

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
    pub deadman_state: Option<DeadManState>,
    pub heartbeat_state: Option<HeartbeatState>,
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
