//! What `bd list --all --json` already holds, read the way `_total`/`_ingested` did.

use serde_json::Value;

/// `d if isinstance(d,list) else d.get("issues", d.get("data", []))` — `bd list --json`'s
/// shape has varied across versions; this accepts a bare array or either wrapper key.
fn rows(v: &Value) -> Vec<&Value> {
    match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o
            .get("issues")
            .or_else(|| o.get("data"))
            .and_then(|x| x.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `_total()`: every bead in the store, github-prefixed or not — the positive control that
/// the query itself is not broken (a store that genuinely holds zero beads never happens in
/// production; see gh-intake's die() on this).
pub fn total_beads(v: &Value) -> usize {
    rows(v).len()
}

#[derive(Debug, Clone)]
pub struct KnownRef {
    pub id: String,
    pub ext_ref: String,
    pub labels: Vec<String>,
}

/// `_ingested()`: every bead whose `external_ref` starts with "github" (case-insensitive) —
/// both work beads (`github:...`) and untrusted records (`github-untrusted:...`) come back
/// from this one query, exactly as the bash read them in one pass.
pub fn ingested(v: &Value) -> Vec<KnownRef> {
    rows(v)
        .into_iter()
        .filter_map(|r| {
            let ext_ref = r.get("external_ref").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if !ext_ref.to_lowercase().starts_with("github") {
                return None;
            }
            let id = r.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let labels = r
                .get("labels")
                .and_then(|l| l.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            Some(KnownRef { id, ext_ref, labels })
        })
        .collect()
}

pub fn known_work(all: &[KnownRef], scope: &str) -> Vec<KnownRef> {
    all.iter()
        .filter(|r| r.ext_ref.starts_with("github:") && r.labels.iter().any(|l| l == scope))
        .cloned()
        .collect()
}

pub fn known_untrusted(all: &[KnownRef]) -> Vec<KnownRef> {
    all.iter().filter(|r| r.ext_ref.starts_with("github-untrusted:")).cloned().collect()
}

pub fn find_ref<'a>(known: &'a [KnownRef], ext_ref: &str) -> Option<&'a KnownRef> {
    known.iter().find(|r| r.ext_ref == ext_ref)
}
