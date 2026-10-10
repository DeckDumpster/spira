//! MENDING rows: opened by the batch's `Failed` event in the same transaction, then driven by
//! `mending pickup|event|sweep|list`. Every timestamp is stamped here from the clock, never
//! taken from the caller, so a mender cannot move its own deadline; `sweep` is how the
//! machine applies `Expire` to an overdue row.

use lifecycle::mending::{self, MendingEvent, MendingEventKind, MendingRow, MendingState};
use serde_json::Value;

use crate::db::{CascadeStep, Conn, DbError, EventRecord};
use crate::rows::escape;
use crate::{flag, CANNOT_TELL, REFUSED};

const COLUMNS: &str = "batch_id, pass, suite, state, picked_at, deadline, triage_ended_at, diagnosis, version";

fn cell(row: &Value, col: &str) -> Option<String> {
    match row.get(col) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn num(row: &Value, col: &str) -> Option<i64> {
    cell(row, col).and_then(|s| s.parse().ok())
}

fn from_json(row: &Value) -> Result<MendingRow, DbError> {
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

fn opt_str(v: &Option<String>) -> String {
    v.as_ref().map_or("NULL".to_string(), |s| format!("'{}'", escape(s)))
}

fn opt_num(v: Option<i64>) -> String {
    v.map_or("NULL".to_string(), |n| n.to_string())
}

fn set_clause(r: &MendingRow) -> String {
    format!(
        "state = '{}', picked_at = {}, deadline = {}, triage_ended_at = {}, diagnosis = {}, version = {}",
        r.state.as_str(),
        opt_num(r.picked_at),
        opt_num(r.deadline),
        opt_num(r.triage_ended_at),
        opt_str(&r.diagnosis),
        r.version
    )
}

fn kind_name(kind: &MendingEventKind) -> String {
    let v = serde_json::to_value(kind).unwrap_or(Value::Null);
    v.as_object().and_then(|m| m.keys().next()).cloned().unwrap_or_else(|| "unknown".into())
}

/// The batch's `Failed` step plus the MENDING row it opens, in one transaction. The row is
/// inserted only while the batch is still at `version`, so a stale event opens nothing.
pub fn open_with_failed(conn: &Conn, batch_step: CascadeStep, suite: &str, pass: u32, at: i64) -> Result<bool, DbError> {
    let batch = &batch_step.key;
    let preamble = format!(
        "INSERT INTO mending (batch_id, pass, suite, state, failed_at, version) \
         SELECT '{b}', {pass}, '{s}', '{state}', {at}, 0 FROM batch WHERE batch_id = '{b}' AND version = {v};\n",
        b = escape(batch),
        s = escape(suite),
        state = MendingState::Waiting.as_str(),
        v = batch_step.old_version,
    );
    Ok(conn.cascade(&preamble, &[batch_step])?.first().copied().unwrap_or(false))
}

pub fn dispatch(args: &[String], conn: &Conn) -> (i32, String) {
    let result = match args.first().map(String::as_str) {
        Some("pickup") => pickup(&args[1..], conn),
        Some("event") => event(&args[1..], conn),
        Some("sweep") => sweep(&args[1..], conn),
        Some("list") => list(&args[1..], conn),
        _ => Err((CANNOT_TELL, "usage: spira-lc mending pickup <batch> --pass N --suite S --deadline-secs D --actor A | event <batch> --pass N --suite S --kind <json> --actor A | sweep --actor A | list [--batch B] [--live]".to_string())),
    };
    match result {
        Ok(out) => (0, out),
        Err(e) => e,
    }
}

type Reply = Result<String, (i32, String)>;

fn cannot(e: DbError) -> (i32, String) {
    (CANNOT_TELL, format!("cannot tell: {e:?}"))
}

fn target(args: &[String], conn: &Conn) -> Result<MendingRow, (i32, String)> {
    let (Some(batch), Some(pass), Some(suite)) = (args.first(), flag(args, "--pass").and_then(|p| p.parse::<u32>().ok()), flag(args, "--suite")) else {
        return Err((CANNOT_TELL, "mending: <batch>, --pass N and --suite S are required".into()));
    };
    fetch(conn, batch, pass, &suite).map_err(cannot)?.ok_or((CANNOT_TELL, format!("mending: no row for {batch} pass {pass} {suite}")))
}

fn pickup(args: &[String], conn: &Conn) -> Reply {
    let row = target(args, conn)?;
    let deadline_s = flag(args, "--deadline-secs")
        .and_then(|d| d.parse::<u64>().ok())
        .ok_or((CANNOT_TELL, "mending pickup: --deadline-secs N is required".to_string()))?;
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending: --actor is required".to_string()))?;
    apply(conn, &row, MendingEventKind::Pickup { at: crate::db::now_epoch(), deadline_s }, &actor)
}

fn event(args: &[String], conn: &Conn) -> Reply {
    let row = target(args, conn)?;
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending: --actor is required".to_string()))?;
    let mut kind: Value = flag(args, "--kind")
        .and_then(|k| serde_json::from_str(&k).ok())
        .ok_or((CANNOT_TELL, "mending event: --kind must be a JSON mending event".to_string()))?;
    if let Some(body) = kind.as_object_mut().and_then(|m| m.values_mut().next()).and_then(Value::as_object_mut) {
        if body.contains_key("at") {
            body.insert("at".into(), Value::from(crate::db::now_epoch()));
        }
    }
    let kind: MendingEventKind = serde_json::from_value(kind).map_err(|e| (CANNOT_TELL, format!("mending event: --kind did not parse: {e}")))?;
    if matches!(kind, MendingEventKind::Pickup { .. } | MendingEventKind::Expire { .. }) {
        return Err((REFUSED, "mending event: pickup and expire are the machine's own, not a mender's".into()));
    }
    apply(conn, &row, kind, &actor)
}

fn apply(conn: &Conn, row: &MendingRow, kind: MendingEventKind, actor: &str) -> Reply {
    let ev = MendingEvent { expect: row.state, version: row.version, kind: kind.clone(), actor: actor.to_string() };
    let outcome = mending::apply(row, &ev);
    let at = crate::db::now_epoch();
    let rec = EventRecord::of_apply(
        "mending",
        row.key(),
        kind_name(&kind),
        row.state.as_str(),
        row.state.as_str(),
        outcome.refusal.as_ref().map(crate::refusal_name),
        serde_json::to_value(&kind).unwrap_or(Value::Null),
        actor,
        at,
    );
    if !outcome.applied {
        conn.insert_refusal_event(&rec).map_err(cannot)?;
        return Err((REFUSED, format!("refused: {:?}", outcome.refusal)));
    }
    match conn.cas_update_and_log("mending", "CONCAT(batch_id, '#', pass, '#', suite)", &row.key(), row.version, &set_clause(&outcome.row), &rec, outcome.row.state.as_str()) {
        Ok(true) => Ok(outcome.row.state.as_str().to_string()),
        Ok(false) => Err((REFUSED, "refused: lost the race to another writer".into())),
        Err(e) => Err(cannot(e)),
    }
}

fn sweep(args: &[String], conn: &Conn) -> Reply {
    let actor = flag(args, "--actor").ok_or((CANNOT_TELL, "mending sweep: --actor is required".to_string()))?;
    Ok(expire_overdue(conn, &actor).map_err(cannot)?.to_string())
}

/// Applies `Expire` to every MENDING row whose deadline has passed; returns how many applied.
pub fn expire_overdue(conn: &Conn, actor: &str) -> Result<usize, DbError> {
    let now = crate::db::now_epoch();
    let rows = conn.query(&format!("SELECT {COLUMNS} FROM mending WHERE state = 'MENDING' AND deadline <= {now}"))?;
    let mut applied = 0;
    for row in rows.iter().map(from_json) {
        let row = row?;
        if apply(conn, &row, MendingEventKind::Expire { at: now }, actor).is_ok() {
            applied += 1;
        }
    }
    Ok(applied)
}

fn list(args: &[String], conn: &Conn) -> Reply {
    let mut wheres = vec!["1 = 1".to_string()];
    if let Some(b) = flag(args, "--batch") {
        wheres.push(format!("batch_id = '{}'", escape(&b)));
    }
    if args.iter().any(|a| a == "--live") {
        wheres.push("state IN ('WAITING', 'MENDING')".into());
    }
    let rows = conn.query(&format!("SELECT {COLUMNS}, failed_at FROM mending WHERE {} ORDER BY failed_at, batch_id, pass, suite", wheres.join(" AND "))).map_err(cannot)?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let m = from_json(r).map_err(cannot)?;
            let mut v = serde_json::to_value(&m).unwrap_or(Value::Null);
            v["state"] = Value::from(m.state.as_str());
            v["failed_at"] = num(r, "failed_at").map_or(Value::Null, Value::from);
            Ok(v)
        })
        .collect::<Result<_, (i32, String)>>()?;
    Ok(Value::Array(out).to_string())
}
