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

/// Every row of an item list whose `external_ref` starts with "github" (case-insensitive):
/// the closeout's and the scheduler task's read of a bounded `plan items` reply.
pub fn ingested(v: &Value) -> Vec<KnownRef> {
    let rows = match v {
        Value::Array(a) => a.as_slice(),
        _ => &[],
    };
    known_refs(&Value::Array(rows.to_vec())).unwrap_or_default().into_iter().filter(|k| k.ext_ref.to_lowercase().starts_with("github")).collect()
}

pub fn find_ref<'a>(known: &'a [KnownRef], ext_ref: &str) -> Option<&'a KnownRef> {
    known.iter().find(|r| r.ext_ref == ext_ref)
}
