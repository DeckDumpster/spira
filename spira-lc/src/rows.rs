//! Row <-> SQL mapping. The only place this binary knows the schema's column names; the
//! `lifecycle` crate never sees a column or a query.

use std::collections::BTreeSet;

use lifecycle::batch::{BatchRow, BatchState};
use lifecycle::bead::{BeadRow, BeadState, HoldKind};
use lifecycle::delivery::{DeliveryRow, DeliveryState, Exit, Mode};
use serde_json::Value;

use crate::db::{Conn, DbError};

pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn text(row: &Value, col: &str) -> Option<String> {
    match row.get(col) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn number(row: &Value, col: &str) -> Option<i64> {
    text(row, col).and_then(|s| s.parse::<i64>().ok())
}

/// A JSON column comes back from `dolt sql -r json` as a JSON-encoded string (the whole
/// row is already string-valued); normalize either that or a directly-nested value.
fn json_col(row: &Value, col: &str) -> Value {
    match row.get(col) {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

pub fn fetch_bead(conn: &Conn, bead_id: &str) -> Result<Option<BeadRow>, DbError> {
    let rows = conn.query(&format!(
        "SELECT bead_id, state, tip, gate_key, holder, persona, lease_until, holds, reason, version, stack, stack_depth, since, express, aeon_phase, disposition, disposition_note FROM bead WHERE bead_id = '{}'",
        escape(bead_id)
    ))?;
    let Some(row) = rows.first() else { return Ok(None) };
    let state = BeadState::from_str(&text(row, "state").unwrap_or_default())
        .ok_or_else(|| DbError::CannotTell(format!("bad state in bead row: {row}")))?;
    let holds: BTreeSet<HoldKind> = json_col(row, "holds")
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).filter_map(HoldKind::from_str).collect())
        .unwrap_or_default();
    let stack: lifecycle::bead::Stack = json_col(row, "stack")
        .as_object()
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    Ok(Some(BeadRow {
        bead_id: bead_id.to_string(),
        state,
        tip: text(row, "tip"),
        gate_key: text(row, "gate_key"),
        holder: text(row, "holder"),
        persona: text(row, "persona"),
        lease_until: number(row, "lease_until"),
        holds,
        reason: text(row, "reason"),
        version: number(row, "version").unwrap_or(0) as u64,
        stack,
        stack_depth: number(row, "stack_depth").unwrap_or(0) as u32,
        since: number(row, "since"),
        express: number(row, "express").is_some_and(|n| n != 0),
        phase: text(row, "aeon_phase").as_deref().and_then(lifecycle::bead::AeonPhase::from_str),
        disposition: text(row, "disposition").as_deref().and_then(lifecycle::bead::DispositionStatus::from_str),
        disposition_note: text(row, "disposition_note"),
    }))
}

pub fn bead_set_clause(row: &BeadRow) -> String {
    let holds_json = Value::Array(row.holds.iter().map(|h| Value::String(h.as_str().to_string())).collect());
    let stack_json = Value::Object(row.stack.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect());
    format!(
        "state = '{}', tip = {}, gate_key = {}, holder = {}, persona = {}, lease_until = {}, holds = '{}', reason = {}, version = {}, stack = '{}', stack_depth = {}, since = {}, express = {}, aeon_phase = {}, disposition = {}, disposition_note = {}, updated_at = {}",
        row.state.as_str(),
        opt_str(&row.tip),
        opt_str(&row.gate_key),
        opt_str(&row.holder),
        opt_str(&row.persona),
        opt_num(row.lease_until),
        holds_json,
        opt_str(&row.reason),
        row.version,
        stack_json,
        row.stack_depth,
        opt_num(row.since),
        u8::from(row.express),
        row.phase.map_or_else(|| "NULL".to_string(), |p| format!("'{}'", p.as_str())),
        row.disposition.map_or_else(|| "NULL".to_string(), |d| format!("'{}'", d.as_str())),
        opt_str(&row.disposition_note),
        crate::db::now_epoch(),
    )
}

pub fn fetch_delivery(conn: &Conn, bead_id: &str) -> Result<Option<DeliveryRow>, DbError> {
    let rows = conn.query(&format!(
        "SELECT bead_id, mode, state, batch_id, pr, merge_sha, version FROM delivery WHERE bead_id = '{}'",
        escape(bead_id)
    ))?;
    let Some(row) = rows.first() else { return Ok(None) };
    let mode = Mode::from_str(&text(row, "mode").unwrap_or_default())
        .ok_or_else(|| DbError::CannotTell(format!("bad mode in delivery row: {row}")))?;
    let state = DeliveryState::from_str(&text(row, "state").unwrap_or_default())
        .ok_or_else(|| DbError::CannotTell(format!("bad state in delivery row: {row}")))?;
    Ok(Some(DeliveryRow {
        bead_id: bead_id.to_string(),
        mode,
        state,
        batch_id: text(row, "batch_id"),
        pr: number(row, "pr").map(|n| n as u64),
        merge_sha: text(row, "merge_sha"),
        exit: None,
        version: number(row, "version").unwrap_or(0) as u64,
    }))
}

pub fn delivery_set_clause(row: &DeliveryRow) -> String {
    let exit_note = match row.exit {
        Some(Exit::Delivered) => "delivered",
        Some(Exit::Returned) => "returned",
        Some(Exit::Requeued) => "requeued",
        None => "",
    };
    let _ = exit_note; // the exit itself is recorded on the event, not on this cache row.
    format!(
        "state = '{}', batch_id = {}, pr = {}, merge_sha = {}, version = {}",
        row.state.as_str(),
        opt_str(&row.batch_id),
        opt_num(row.pr.map(|n| n as i64)),
        opt_str(&row.merge_sha),
        row.version,
    )
}

pub fn fetch_batch(conn: &Conn, batch_id: &str) -> Result<Option<BatchRow>, DbError> {
    let rows = conn.query(&format!(
        "SELECT batch_id, repo, state, parent, head, base, run, reason, pass, phase, version FROM batch WHERE batch_id = '{}'",
        escape(batch_id)
    ))?;
    let Some(row) = rows.first() else { return Ok(None) };
    let state = BatchState::from_str(&text(row, "state").unwrap_or_default())
        .ok_or_else(|| DbError::CannotTell(format!("bad state in batch row: {row}")))?;
    Ok(Some(BatchRow {
        batch_id: batch_id.to_string(),
        repo: text(row, "repo").unwrap_or_default(),
        state,
        parent: text(row, "parent"),
        head: text(row, "head"),
        base: text(row, "base"),
        run: text(row, "run"),
        reason: text(row, "reason"),
        pass: number(row, "pass").unwrap_or(0) as u32,
        phase: text(row, "phase").as_deref().and_then(lifecycle::batch::BatchPhase::from_str),
        version: number(row, "version").unwrap_or(0) as u64,
    }))
}

pub fn batch_set_clause(row: &BatchRow) -> String {
    format!(
        "state = '{}', parent = {}, head = {}, base = {}, run = {}, reason = {}, pass = {}, phase = {}, version = {}",
        row.state.as_str(),
        opt_str(&row.parent),
        opt_str(&row.head),
        opt_str(&row.base),
        opt_str(&row.run),
        opt_str(&row.reason),
        row.pass,
        opt_str(&row.phase.map(|p| p.as_str().to_string())),
        row.version,
    )
}

fn opt_str(v: &Option<String>) -> String {
    match v {
        Some(s) => format!("'{}'", escape(s)),
        None => "NULL".to_string(),
    }
}

fn opt_num(v: Option<i64>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "NULL".to_string(),
    }
}
