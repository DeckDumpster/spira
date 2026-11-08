//! `claim-verdicts`, a JSON object on stdin: the dispatcher's verdict on each READY or REWORK
//! bead from one pass, written onto the bead rows so the ops views read them with the rows.
//! It is a reading of the pass, not a transition — no event, no version bump. The verdict
//! vocabulary belongs to the producer (`spira-claim`); here a kind need only be a plain word.

use serde::Deserialize;

use crate::cutover::{is_row_key, q};

#[derive(Deserialize)]
pub(crate) struct Payload {
    pub(crate) computed_at: String,
    pub(crate) entries: Vec<Entry>,
}

#[derive(Deserialize)]
pub(crate) struct Entry {
    pub(crate) id: String,
    pub(crate) verdict: String,
    #[serde(default)]
    pub(crate) blocker: Option<String>,
    #[serde(default)]
    pub(crate) runes: Vec<String>,
}

fn word(s: &str) -> bool {
    !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
}

fn stamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20 && b[19] == b'Z' && b[10] == b'T' && b.iter().enumerate().all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit())
}

fn script(p: &Payload) -> Result<(String, Vec<String>), String> {
    if !stamp(&p.computed_at) {
        return Err(format!("computed_at {:?} is not an ISO-8601 UTC stamp", p.computed_at));
    }
    let mut sql = String::new();
    for e in &p.entries {
        let runes = e.runes.join(",");
        if !is_row_key(&e.id) || !word(&e.verdict) || e.blocker.as_deref().is_some_and(|b| !is_row_key(b)) || runes.len() > 255 || e.runes.iter().any(|r| !word(r)) {
            return Err(format!("entry for {:?} is malformed", e.id));
        }
        sql.push_str(&format!(
            "UPDATE bead SET claim_verdict = {}, claim_blocker = {}, claim_runes = {}, claim_at = {} WHERE bead_id = {};\n",
            q(&e.verdict),
            e.blocker.as_deref().map_or("NULL".to_string(), q),
            if runes.is_empty() { "NULL".to_string() } else { q(&runes) },
            q(&p.computed_at),
            q(&e.id),
        ));
    }
    Ok((sql, p.entries.iter().map(|e| e.id.clone()).collect()))
}

/// The payload as either store takes it: parsed, and every entry well formed, or the refusal.
pub(crate) fn checked(body: &str) -> Result<Payload, String> {
    let payload: Payload = serde_json::from_str(body).map_err(|e| format!("claim-verdicts: {e}"))?;
    script(&payload).map_err(|e| format!("claim-verdicts: {e}"))?;
    Ok(payload)
}

