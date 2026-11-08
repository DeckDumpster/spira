//! MENDING rows: opened by the batch's `Failed` event in the same transaction, then driven by
//! `mending pickup|event|sweep|list`. Every timestamp is stamped here from the clock, never
//! taken from the caller, so a mender cannot move its own deadline; `sweep` is how the
//! machine applies `Expire` to an overdue row.

use lifecycle::mending::{MendingRow, MendingState};
use serde_json::Value;

use crate::db::{Conn, DbError};
use crate::rows::escape;
use crate::CANNOT_TELL;

const COLUMNS: &str = "batch_id, pass, suite, state, picked_at, deadline, triage_ended_at, diagnosis, awaiting_ask, version";

fn cell(row: &Value, col: &str) -> Option<String> {
    match row.get(col) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn num(row: &Value, col: &str) -> Option<i64> {
    cell(row, col).and_then(|s| s.parse().ok())
}

pub(crate) fn from_json(row: &Value) -> Result<MendingRow, DbError> {
    let state = MendingState::from_str(&cell(row, "state").unwrap_or_default())
        .ok_or_else(|| DbError::CannotTell(format!("bad state in mending row: {row}")))?;
    Ok(MendingRow {
        batch_id: cell(row, "batch_id").unwrap_or_default(),
        pass: num(row, "pass").unwrap_or(0) as u32,
        suite: cell(row, "suite").unwrap_or_default(),
        state,
        picked_at: num(row, "picked_at"),
        deadline: num(row, "deadline"),
        triage_ended_at: num(row, "triage_ended_at"),
        diagnosis: cell(row, "diagnosis"),
        awaiting_ask: cell(row, "awaiting_ask"),
        version: num(row, "version").unwrap_or(0) as u64,
    })
}

pub fn fetch(conn: &Conn, batch: &str, pass: u32, suite: &str) -> Result<Option<MendingRow>, DbError> {
    let rows = conn.query(&format!(
        "SELECT {COLUMNS} FROM mending WHERE batch_id = '{}' AND pass = {pass} AND suite = '{}'",
        escape(batch),
        escape(suite)
    ))?;
    rows.first().map(from_json).transpose()
}

const BATCH_MAX: usize = 64;
const SUITE_MAX: usize = 160;
const KEY_MAX: usize = 256;

/// Refuse an over-long field by name before it reaches SQL, mirroring `mending` and `event.lc_key`.
pub fn check_widths(batch: &str, pass: u32, suite: &str) -> Result<(), (i32, String)> {
    let key = lifecycle::mending::key(batch, pass, suite);
    for (name, value, max) in [("batch_id", batch, BATCH_MAX), ("suite", suite, SUITE_MAX), ("lc_key", key.as_str(), KEY_MAX)] {
        let n = value.chars().count();
        if n > max {
            return Err((CANNOT_TELL, format!("mending: refused: {name} is {n} chars (max {max}); {name}={value}")));
        }
    }
    Ok(())
}

