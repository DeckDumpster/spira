//! `RoundController`: one repo's rounds, written by the round controller. The reconciler's
//! design owns this schema; the controller owns the publisher, its actions and its applier.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
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
pub const LAND_REFUSED: &str = "LandRefused";
pub const EJECT_REFUSED: &str = "EjectRefused";
pub const TWO_ANSWERS_OPEN_ROUND: &str = "TwoAnswersOpenRound";
pub const CUT_OF_JUDGED_RED: &str = "CutOfJudgedRed";
pub const STUCK_RUNNER: &str = "StuckRunner";
pub const PRIOR_RUNNER_ALIVE: &str = "PriorRunnerAlive";
pub const REDRIVE_EXHAUSTED: &str = "RedriveExhausted";

/// The contradictions: states the controller cannot resolve itself. Each is paged when it
/// is raised, and none pulls a cord.
pub const CONTRADICTIONS: [&str; 8] = [
    LAND_REFUSED,
    EJECT_REFUSED,
    TWO_ANSWERS_OPEN_ROUND,
    CUT_OF_JUDGED_RED,
    LANE_BLOCKED,
    STUCK_RUNNER,
    PRIOR_RUNNER_ALIVE,
    REDRIVE_EXHAUSTED,
];

/// The runbook entry for a condition type.
pub fn runbook(type_: &str) -> String {
    format!("round-controller/runbook-controller#{}", type_.to_ascii_lowercase())
}

/// The two actions the section declares, by the names its intents carry.
pub const HOLD_MEMBER: &str = "hold-member";
pub const CONCURRENCY: &str = "concurrency";

/// The status field naming, for each open `RepeatIdentity`, the members a hold would keep out
/// of the cut: one [`RepeatHold`] per member proven red under that identity.
pub const REPEAT_IDENTITY_HOLDS: &str = "repeat_identity_holds";

/// Who lifts a hold the reconciler places (law-every-hold-names-who-lifts-it).
pub const HOLD_LIFTED_BY: &str = "reconciler";

/// One member at one tip that a hold keeps out of the cut, and the identity it stops repeating.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepeatHold {
    pub identity: String,
    pub id: String,
    pub tip: String,
}

/// A hold's value: who lifts it. The applier takes no hold without one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub lifted_by: String,
}

impl Hold {
    pub fn of(v: &Value) -> Option<Hold> {
        serde_json::from_value::<Hold>(v.clone()).ok().filter(|h| !h.lifted_by.trim().is_empty())
    }
}

/// The section's knobs, under the controller's own `knobs/RoundController/<repo>/`. The
/// reconciler writes intents on these keys and the controller applies only these, so both
/// sides build them here.
pub fn knobs_prefix(repo: &str) -> String {
    format!("knobs/{NAME}/{repo}/")
}

pub fn concurrency_key(repo: &str) -> String {
    format!("{}{CONCURRENCY}", knobs_prefix(repo))
}

pub fn holds_prefix(repo: &str) -> String {
    format!("{}hold/", knobs_prefix(repo))
}

/// The hold on member `id` at `tip`: created from null, so an absent key is an unheld member.
pub fn hold_key(repo: &str, id: &str, tip: &str) -> String {
    format!("{}{id}/{tip}", holds_prefix(repo))
}

/// The (member, tip) a hold key names, `None` for any other key.
pub fn parse_hold_key<'a>(repo: &str, key: &'a str) -> Option<(&'a str, &'a str)> {
    let (id, tip) = key.strip_prefix(&holds_prefix(repo))?.split_once('/')?;
    [id, tip].iter().all(|p| !p.is_empty() && !p.contains('/')).then_some((id, tip))
}

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
    pub repeat_identity_holds: Option<Vec<RepeatHold>>,
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
        ConditionDecl {
            type_: LAND_REFUSED,
            page_on_raise: true,
        },
        ConditionDecl {
            type_: EJECT_REFUSED,
            page_on_raise: true,
        },
        ConditionDecl {
            type_: TWO_ANSWERS_OPEN_ROUND,
            page_on_raise: true,
        },
        ConditionDecl {
            type_: CUT_OF_JUDGED_RED,
            page_on_raise: true,
        },
        ConditionDecl {
            type_: STUCK_RUNNER,
            page_on_raise: true,
        },
        ConditionDecl {
            type_: PRIOR_RUNNER_ALIVE,
            page_on_raise: true,
        },
        ConditionDecl {
            type_: REDRIVE_EXHAUSTED,
            page_on_raise: true,
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
        (REPEAT_IDENTITY_HOLDS, FieldShape::List),
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
            "distance": { "fn": "max(0, green_unlanded_minutes) / 5 + repeat_identity_open + 2 * lanes_blocked", "open": 1.0, "close": 0.5 },
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
    fn every_contradiction_is_declared_and_pages_on_raise() {
        let kind = crate::sections::lookup(super::NAME).unwrap();
        for t in super::CONTRADICTIONS {
            assert!(kind.condition(t).unwrap().page_on_raise, "{t}");
        }
        assert_eq!(super::runbook("EjectRefused"), "round-controller/runbook-controller#ejectrefused");
    }

    #[test]
    fn a_field_the_kind_does_not_declare_is_refused_by_name() {
        let v = section(json!({ "observedGeneration": 0, "wedgedness": { "value": 2.0, "window": "level" } }));
        let r = admit_publish(&parse_section(&v).unwrap()).unwrap_err();
        assert!(r.field.starts_with("status"), "{r:?}");
        assert!(r.rule.contains("wedgedness"), "{r:?}");
    }

    #[test]
    fn a_hold_key_names_one_member_at_one_tip_and_reads_back_as_it() {
        let k = super::hold_key("lane", "sp-a", "a1");
        assert_eq!(k, "knobs/RoundController/lane/hold/sp-a/a1");
        assert_eq!(super::parse_hold_key("lane", &k), Some(("sp-a", "a1")));
        for bad in [
            "knobs/RoundController/lane/hold/sp-a",
            "knobs/RoundController/lane/hold/sp-a/a1/x",
            "knobs/RoundController/lane/hold//a1",
            "knobs/RoundController/other/hold/sp-a/a1",
        ] {
            assert_eq!(super::parse_hold_key("lane", bad), None, "{bad}");
        }
        assert_eq!(super::concurrency_key("lane"), "knobs/RoundController/lane/concurrency");
    }

    #[test]
    fn a_repeat_hold_names_its_identity_member_and_tip_or_the_section_is_refused() {
        let with = |hold: serde_json::Value| {
            let level = |v: f64| json!({ "value": v, "window": "level" });
            section(json!({ "observedGeneration": 0,
                "green_unlanded_minutes": level(0.0), "repeat_identity_open": level(1.0),
                "waiting_with_no_slot": level(0.0), "lanes_blocked": level(0.0),
                "repeat_identity_holds": [hold] }))
        };
        let ok = with(json!({ "identity": "h1", "id": "sp-a", "tip": "a1" }));
        admit_publish(&parse_section(&ok).unwrap()).unwrap();
        let bad = with(json!({ "identity": "h1", "id": "sp-a" }));
        let r = admit_publish(&parse_section(&bad).unwrap()).unwrap_err();
        assert_eq!((r.field.as_str(), r.rule.contains("tip")), ("status.repeat_identity_holds[0]", true), "{r:?}");
    }
}
