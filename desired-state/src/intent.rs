//! The intent contract: a reconciler step at its own key, `intents/<Kind>/<name>/<id>`, never
//! inside a section, so an install's spec apply can never overwrite one. The reconciler writes
//! it and later its judgement; the target section's owner writes its ack, once.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::section::{parse_duration, parse_rfc3339, registered, Refusal};
use crate::sections::{FieldShape, SectionKind};

pub const INTENTS_PREFIX: &str = "intents/";
/// Longer than the 24 h back-off cap, so the back-off history is always readable.
pub const INTENT_TTL_SECS: u64 = 48 * 3600;
/// The one actor that writes intents and judgements.
pub const RECONCILER: &str = "reconciler";
/// The lock every intent and judgement write is fenced on, at `locks/<RECONCILER_LOCK>`.
pub const RECONCILER_LOCK: &str = "reconciler";

pub fn intent_key(kind: &str, name: &str, id: &str) -> String {
    format!("{INTENTS_PREFIX}{kind}/{name}/{id}")
}

/// The kind, section name and id an intent key names.
pub fn parse_key(key: &str) -> Option<(&str, &str, &str)> {
    let rest = key.strip_prefix(INTENTS_PREFIX)?;
    let mut parts = rest.split('/');
    let (kind, name, id) = (parts.next()?, parts.next()?, parts.next()?);
    let ok = parts.next().is_none() && [kind, name, id].iter().all(|p| !p.is_empty());
    ok.then_some((kind, name, id))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub id: String,
    pub action: String,
    /// A round, a member@tip, or a knob.
    pub target: String,
    pub from: Value,
    pub to: Value,
    pub moves: String,
    /// The `moves` field's value when the step was taken.
    pub baseline: f64,
    pub expected_effect: f64,
    pub horizon: String,
    pub addresses: Vec<String>,
    pub taken_at: String,
    pub by: By,
    #[serde(default)]
    pub ack: Option<Ack>,
    #[serde(default)]
    pub judgement: Option<Judgement>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct By {
    pub pass: String,
    pub lock_token: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    pub at: String,
    pub result: AckResult,
    pub observed_rev: u64,
    /// What the owner measured when it took the step, for an owner whose step a statute asks to
    /// name its measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AckResult {
    Applied,
    AlreadyApplied,
    Refused {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stale: Option<Value>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Judgement {
    pub at: String,
    pub result: JudgementResult,
    pub observed: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JudgementResult {
    Fulfilled,
    Failed,
}

pub fn parse_intent(v: &Value) -> Result<Intent, Refusal> {
    serde_path_to_error::deserialize(v).map_err(|e| {
        let path = e.path().to_string();
        let field = if path.is_empty() || path == "." { "intent".to_string() } else { path };
        Refusal::new(field, "schema", e.inner().to_string())
    })
}

/// The schema of a new intent at `key`, written under the lock token `token`.
pub fn admit_put(key: &str, i: &Intent, token: &str) -> Result<&'static SectionKind, Refusal> {
    let Some((kind, _, id)) = parse_key(key) else {
        return Err(Refusal::new("key", "key", format!("{key:?} is not intents/<Kind>/<name>/<id>")));
    };
    let kind = registered(kind)?;
    if i.id != id {
        return Err(Refusal::new("id", "key", format!("{:?} is not the key's id {id:?}", i.id)));
    }
    if i.by.lock_token != token {
        return Err(Refusal::new("by.lock_token", "token", "is not the token the write is fenced on"));
    }
    if i.ack.is_some() {
        return Err(Refusal::new("ack", "ownership", "only the target section's owner acks, and only once"));
    }
    if i.judgement.is_some() {
        return Err(Refusal::new("judgement", "ownership", "an intent is judged after its ack, by its own write"));
    }
    if kind.shape(&i.moves) != Some(FieldShape::Numeric) {
        return Err(Refusal::new("moves", "moves", format!("{} is not a numeric status field of {}", i.moves, kind.name)));
    }
    for (j, c) in i.addresses.iter().enumerate() {
        if kind.condition(c).is_none() {
            return Err(Refusal::new(
                format!("addresses[{j}]"),
                "addresses",
                format!("{c:?} is not a condition type {} declares", kind.name),
            ));
        }
    }
    if parse_duration(&i.horizon).is_none() {
        return Err(Refusal::new("horizon", "duration", format!("{:?} is not a duration", i.horizon)));
    }
    if parse_rfc3339(&i.taken_at).is_none() {
        return Err(Refusal::new("taken_at", "time", "is not YYYY-MM-DDTHH:MM:SSZ"));
    }
    Ok(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_names_kind_section_and_id_and_nothing_else() {
        assert_eq!(intent_key("Probe", "a", "7"), "intents/Probe/a/7");
        assert_eq!(parse_key("intents/Probe/a/7"), Some(("Probe", "a", "7")));
        for bad in ["intents/Probe/a", "intents/Probe/a/7/x", "intents//a/7", "telemetry/Probe/a/7"] {
            assert_eq!(parse_key(bad), None, "{bad}");
        }
    }

    #[test]
    fn ack_results_are_kebab_case_and_a_refusal_carries_its_reason() {
        let r = serde_json::to_value(AckResult::AlreadyApplied).unwrap();
        assert_eq!(r, "already-applied");
        let r = serde_json::to_value(AckResult::Refused {
            reason: "stale".into(),
            stale: Some(Value::from(3)),
        })
        .unwrap();
        assert_eq!(r, serde_json::json!({"refused": {"reason": "stale", "stale": 3}}));
    }
}
