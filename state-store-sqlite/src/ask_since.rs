//! Gives every ask-held lifecycle record its `ask_since` from the audits in its history.
//! Run in Rust after the day files are open, because SQL on the main file cannot see them.

use rusqlite::params;
use serde_json::Value;
use state_store::{events_key, Backend, Result};

use crate::{db, exec, lock, SqliteBackend};

const CANDIDATES: &str = "SELECT substr(CAST(key AS TEXT), 11) FROM latest
    WHERE key >= CAST('lifecycle/' AS BLOB) AND key < CAST('lifecycle0' AS BLOB)
      AND instr(substr(CAST(key AS TEXT), 11), '/') = 0
      AND json_valid(CAST(value AS TEXT))
      AND json_type(CAST(value AS TEXT), '$.data.holds') = 'array'
      AND json_type(CAST(value AS TEXT), '$.data.ask_since') IS NULL
      AND EXISTS (SELECT 1 FROM json_each(CAST(latest.value AS TEXT), '$.data.holds') AS j WHERE j.value = 'Ask')
    ORDER BY key";

const PAGE: usize = 500;

fn candidates(b: &SqliteBackend) -> Result<Vec<String>> {
    let w = lock(&b.0.writer);
    let mut stmt = w.conn.prepare(CANDIDATES).map_err(db)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).map_err(db)?;
    rows.collect::<std::result::Result<_, _>>().map_err(db)
}

fn raised_at(a: &Value, id: &str) -> Option<i64> {
    let ok = a["machine"] == "bead"
        && a["event"] == "Hold"
        && a["applied"] == true
        && a["lc_key"] == id
        && a["evidence"]["Hold"]["kind"] == "Ask";
    if ok { a["at"].as_i64() } else { None }
}

fn parse(v: &[u8]) -> Value {
    serde_json::from_slice(v).unwrap_or(Value::Null)
}

/// The newest applied Ask raise in `id`'s history, as `(at, main)`.
fn newest_raise(b: &SqliteBackend, id: &str, head: u64) -> Result<Option<(i64, u64)>> {
    let prefix = format!("history/{id}/");
    let mut best: Option<(i64, u64)> = None;
    let mut after: Option<String> = None;
    loop {
        let page = b.range(&prefix, after.as_deref(), PAGE, head)?;
        let Some((last, _)) = page.last() else { break };
        after = Some(last.clone());
        let full = page.len() == PAGE;
        let mut pending = Vec::new();
        for (_, ver) in &page {
            let data = parse(&ver.value)["data"].clone();
            let main = ver.mod_rev.main;
            if data["seq"].is_null() {
                pending.push(main);
            } else if let Some(at) = raised_at(&data["audit"], id) {
                best = best.max(Some((at, main)));
            }
        }
        let keys: Vec<String> = pending.iter().map(|m| events_key(*m)).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        for (main, ev) in pending.iter().zip(b.get_many(&refs, head)?) {
            let Some(ev) = ev else { continue };
            let data = parse(&ev.value)["data"].clone();
            let branch = if data["refused"] == true { "else" } else { "then" };
            for a in data["evidence"][branch].as_array().into_iter().flatten() {
                if let Some(at) = raised_at(a, id) {
                    best = best.max(Some((at, *main)));
                }
            }
        }
        if !full {
            break;
        }
    }
    Ok(best)
}

/// Fills `ask_since` on each ask-held record lacking it, and returns the ids no applied Ask
/// hold could be found for; those records stay as they were.
pub(crate) fn fill(b: &SqliteBackend) -> Result<Vec<String>> {
    let ids = candidates(b)?;
    if ids.is_empty() {
        return Ok(ids);
    }
    let head = b.head()?;
    let mut found = Vec::new();
    for id in &ids {
        if let Some((at, _)) = newest_raise(b, id, head)? {
            found.push((id.as_str(), at));
        }
    }
    {
        let mut w = lock(&b.0.writer);
        let tx = w.conn.transaction().map_err(db)?;
        for (id, at) in &found {
            let key = format!("lifecycle/{id}");
            exec(
                &tx,
                "UPDATE latest SET value = CAST(json_set(CAST(value AS TEXT), '$.data.ask_since', ?2) AS BLOB) WHERE key = ?1",
                params![key.as_bytes(), at],
            )?;
            exec(
                &tx,
                "UPDATE versions SET value = l.value FROM latest AS l
                 WHERE l.key = versions.key AND l.main = versions.main AND l.key = ?1 AND versions.value IS NOT l.value",
                params![key.as_bytes()],
            )?;
        }
        tx.commit().map_err(db)?;
    }
    candidates(b)
}

pub(crate) fn warning(unfilled: &[String]) -> Option<String> {
    (!unfilled.is_empty()).then(|| {
        format!(
            "ask_since: {} ask-held record(s) have no applied Ask hold in their history and stay without ask_since: {}",
            unfilled.len(),
            unfilled.join(", ")
        )
    })
}
