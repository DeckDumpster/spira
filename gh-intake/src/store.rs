//! A bead the store answered for one external reference.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownRef {
    pub id: String,
    pub ext_ref: String,
    pub labels: Vec<String>,
}

/// The rows `content ref` answers (a bare array); `None` when the reply is no array.
pub fn known_refs(v: &Value) -> Option<Vec<KnownRef>> {
    let text = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Some(
        v.as_array()?
            .iter()
            .map(|r| KnownRef {
                id: text(r, "id"),
                ext_ref: text(r, "external_ref"),
                labels: r.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            })
            .collect(),
    )
}

/// The labels of a `bd show --json` reply, which is a bare object or a one-element array.
pub fn shown_labels(v: &Value) -> Vec<String> {
    let bead = v.as_array().and_then(|a| a.first()).unwrap_or(v);
    bead.get("labels").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}
