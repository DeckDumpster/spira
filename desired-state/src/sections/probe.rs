//! `Probe`: the test kind. It exercises every rule of the schema and is what the conformance
//! tests write; no component publishes it.

use serde::Deserialize;
use serde_json::Value;

use super::{typed, ConditionDecl, FieldShape, SchemaError, SectionKind};
use crate::section::{Numeric, Target};

pub const NAME: &str = "Probe";
pub const OWNER: &str = "probe";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Spec {
    pub depth_max: Option<Target<f64>>,
    pub wedged_open: Option<Target<f64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Status {
    pub depth: Option<Numeric>,
    pub wedged: Option<Numeric>,
    pub depth_24h: Option<Numeric>,
    pub stuck: Option<Vec<Value>>,
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
            type_: "Deep",
            page_on_raise: false,
        },
        ConditionDecl {
            type_: "Wedged",
            page_on_raise: true,
        },
    ],
    fields: &[
        ("depth", FieldShape::Numeric),
        ("wedged", FieldShape::Numeric),
        ("depth_24h", FieldShape::Numeric),
        ("stuck", FieldShape::List),
    ],
    check_spec,
    check_status,
};
