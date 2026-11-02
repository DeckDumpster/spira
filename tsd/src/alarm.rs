//! One row per alarm attempt, whoever made it — `incident file`, `incident alarm`, `mail send`
//! or the alarms pass's pages: the `alarm-attempt` family. An attempt that leaves no row is a
//! silent failure, so the row is a closed shape: an outcome outside the three is refused, a
//! delivery names its id and a failure its reason.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub const FAMILY: &str = "alarm-attempt";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered { id: String },
    DeliveredFallback,
    Failed { reason: String },
}

/// What a failed attempt would send, so a retry can send it again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub mailbox: String,
    pub subject: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// Epoch seconds.
    pub at: u64,
    pub caller: String,
    pub kind: String,
    /// The alarm this attempt delivers: two attempts with one reference are one alarm.
    pub reference: String,
    pub outcome: Outcome,
    pub message: Option<Message>,
}

impl Attempt {
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [("caller", &self.caller), ("kind", &self.kind), ("reference", &self.reference)] {
            if v.trim().is_empty() {
                return Err(format!("an alarm attempt needs a {name}"));
            }
        }
        match &self.outcome {
            Outcome::Delivered { id } if id.trim().is_empty() => Err("a delivered attempt names the id it was delivered as".into()),
            Outcome::Failed { reason } if reason.trim().is_empty() => Err("a failed attempt says why".into()),
            _ => Ok(()),
        }
    }

    pub fn fields(&self) -> Vec<(String, Value)> {
        let f = |k: &str, v: Value| (k.to_string(), v);
        let mut out = vec![
            f("at", self.at.into()),
            f("caller", self.caller.clone().into()),
            f("kind", self.kind.clone().into()),
            f("reference", self.reference.clone().into()),
        ];
        match &self.outcome {
            Outcome::Delivered { id } => out.extend([f("outcome", "delivered".into()), f("id", id.clone().into())]),
            Outcome::DeliveredFallback => out.push(f("outcome", "delivered-fallback".into())),
            Outcome::Failed { reason } => out.extend([f("outcome", "failed".into()), f("reason", reason.clone().into())]),
        }
        if let Some(m) = &self.message {
            out.push(f("message", serde_json::to_value(m).unwrap_or(Value::Null)));
        }
        out
    }

    /// One row back as an attempt: the envelope is dropped, and anything outside the shape refused.
    pub fn from_row(row: &Value) -> Result<Attempt, String> {
        let m = row.as_object().ok_or("an alarm-attempt row is a JSON object")?;
        const KEYS: [&str; 11] = ["ts", "host", "family", "at", "caller", "kind", "reference", "outcome", "id", "reason", "message"];
        if let Some(k) = m.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return Err(format!("{k:?} is not a field of an alarm attempt"));
        }
        let s = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| format!("an alarm attempt has no {k}"));
        let outcome = match s("outcome")?.as_str() {
            "delivered" => Outcome::Delivered { id: s("id")? },
            "delivered-fallback" => Outcome::DeliveredFallback,
            "failed" => Outcome::Failed { reason: s("reason")? },
            other => return Err(format!("outcome {other:?} is not delivered, delivered-fallback or failed")),
        };
        let message = match m.get("message") {
            None => None,
            Some(v) => Some(serde_json::from_value(v.clone()).map_err(|e| format!("message: {e}"))?),
        };
        let a = Attempt {
            at: m.get("at").and_then(Value::as_u64).ok_or("an alarm attempt has no numeric at")?,
            caller: s("caller")?,
            kind: s("kind")?,
            reference: s("reference")?,
            outcome,
            message,
        };
        a.validate()?;
        Ok(a)
    }

    /// Appends this attempt's one row to `<root>/tsd/alarm-attempt.jsonl`.
    pub fn write(&self, root: &Path, host: &str) -> Result<(), String> {
        self.validate()?;
        let line = crate::build_row(&crate::iso_utc(self.at), host, FAMILY, &self.fields())?;
        crate::append_line(&crate::family_path(root, FAMILY), &line)
    }
}

/// Every attempt the family's file under `root` holds, oldest first; a line that is not one is
/// skipped, and a missing file is none.
pub fn read(root: &Path) -> Vec<Attempt> {
    let text = std::fs::read_to_string(crate::family_path(root, FAMILY)).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| Attempt::from_row(&v).ok()).collect()
}
