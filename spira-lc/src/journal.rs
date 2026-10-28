//! The runtime's journal verbs: `journal <op> ...` over the `runtime_*` tables. The runtime
//! reaches the lifecycle store only through this socket, so an idle runtime launches no process.
//! Every write is idempotent on its key; an event or schedule row is one JSON argument.

use serde_json::{json, Value};

use crate::db::Conn;
use crate::rows::escape;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

/// Rows one `read` returns at most, whatever the caller asks for.
const READ_MAX: u64 = 1000;

fn lit(s: &str) -> String {
    format!("'{}'", escape(s))
}

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(Value::as_str).ok_or_else(|| format!("journal: missing string {k}"))
}

fn number(v: &Value, k: &str) -> Result<u64, String> {
    v.get(k).and_then(Value::as_u64).ok_or_else(|| format!("journal: missing number {k}"))
}

fn opt_lit(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).map_or("NULL".into(), lit)
}

fn parse(arg: Option<&String>, what: &str) -> Result<Value, String> {
    let text = arg.ok_or_else(|| format!("journal: missing {what}"))?;
    if text.contains('\0') {
        return Err(format!("journal: {what} holds a NUL"));
    }
    serde_json::from_str(text).map_err(|e| format!("journal: {what} is not JSON: {e}"))
}

fn arg_u64(arg: Option<&String>, what: &str) -> Result<u64, String> {
    arg.and_then(|a| a.parse().ok()).ok_or_else(|| format!("journal: {what} must be a non-negative integer"))
}

fn text_arg<'a>(arg: Option<&'a String>, what: &str) -> Result<&'a str, String> {
    match arg.map(String::as_str) {
        Some(s) if !s.is_empty() && !s.contains('\0') => Ok(s),
        _ => Err(format!("journal: {what} must be non-empty text")),
    }
}

pub fn append_sql(ev: &Value) -> Result<String, String> {
    let kind = ev.get("kind").ok_or("journal: event has no kind")?;
    Ok(format!(
        "INSERT IGNORE INTO runtime_event (event_id, stream, kind_tag, kind_body, at, cause, correlation) VALUES ({}, {}, {}, {}, {}, {}, {})",
        lit(field(ev, "id")?),
        lit(field(ev, "stream")?),
        lit(field(kind, "tag")?),
        lit(field(kind, "body")?),
        number(ev, "at")?,
        opt_lit(ev, "cause"),
        lit(field(ev, "correlation")?),
    ))
}

pub fn schedule_sql(row: &Value) -> Result<String, String> {
    let kind = row.get("kind").ok_or("journal: row has no kind")?;
    Ok(format!(
        "INSERT IGNORE INTO runtime_schedule (row_id, at, stream, kind_tag, kind_body, cause, correlation, fired) VALUES ({}, {}, {}, {}, {}, {}, {}, 0)",
        lit(field(row, "id")?),
        number(row, "at")?,
        lit(field(row, "stream")?),
        lit(field(kind, "tag")?),
        lit(field(kind, "body")?),
        opt_lit(row, "cause"),
        lit(field(row, "correlation")?),
    ))
}

fn cell(r: &Value, k: &str) -> Value {
    match r.get(k) {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Null) | None => Value::Null,
        Some(v) => Value::String(v.to_string()),
    }
}

fn num(r: &Value, k: &str) -> Value {
    cell(r, k).as_str().and_then(|s| s.parse::<u64>().ok()).map_or(Value::Null, Value::from)
}

fn kind_of(r: &Value) -> Value {
    json!({ "tag": cell(r, "kind_tag"), "body": cell(r, "kind_body").as_str().unwrap_or("") })
}

fn event_of(r: &Value) -> Value {
    json!({ "id": cell(r, "event_id"), "stream": cell(r, "stream"), "kind": kind_of(r), "at": num(r, "at"), "cause": cell(r, "cause"), "correlation": cell(r, "correlation") })
}

fn row_of(r: &Value) -> Value {
    json!({ "id": cell(r, "row_id"), "at": num(r, "at"), "stream": cell(r, "stream"), "kind": kind_of(r), "cause": cell(r, "cause"), "correlation": cell(r, "correlation") })
}

fn write(conn: &Conn, sql: &str) -> Result<(), String> {
    conn.append_event(sql).map_err(|e| format!("{e:?}"))
}

fn read(conn: &Conn, sql: &str) -> Result<Vec<Value>, String> {
    conn.query(sql).map_err(|e| format!("{e:?}"))
}

fn run(args: &[String], conn: &Conn) -> Result<String, String> {
    let op = args.first().map(String::as_str).unwrap_or("");
    let a = |i: usize| args.get(i);
    match op {
        "append" => {
            let ev = parse(a(1), "event")?;
            let id = field(&ev, "id")?.to_string();
            write(conn, &append_sql(&ev)?)?;
            let rows = read(conn, &format!("SELECT seq FROM runtime_event WHERE event_id = {}", lit(&id)))?;
            rows.first().map(|r| num(r, "seq").to_string()).ok_or_else(|| "journal: appended event not found".into())
        }
        "read" => {
            let (from, limit) = (arg_u64(a(1), "from")?, arg_u64(a(2), "limit")?.min(READ_MAX));
            let rows = read(
                conn,
                &format!("SELECT seq, event_id, stream, kind_tag, kind_body, at, cause, correlation FROM runtime_event WHERE seq >= {from} ORDER BY seq LIMIT {limit}"),
            )?;
            Ok(Value::Array(rows.iter().map(|r| json!([num(r, "seq"), event_of(r)])).collect()).to_string())
        }
        "applied" | "mark-applied" => {
            let (handler, id) = (text_arg(a(1), "handler")?, text_arg(a(2), "event id")?);
            if op == "mark-applied" {
                write(conn, &format!("INSERT IGNORE INTO runtime_applied (handler, event_id) VALUES ({}, {})", lit(handler), lit(id)))?;
                return Ok(String::new());
            }
            let rows = read(conn, &format!("SELECT 1 AS hit FROM runtime_applied WHERE handler = {} AND event_id = {}", lit(handler), lit(id)))?;
            Ok(if rows.is_empty() { "0" } else { "1" }.into())
        }
        "cursor" => {
            let rows = read(conn, "SELECT next FROM runtime_cursor WHERE id = 1")?;
            Ok(rows.first().map_or(0, |r| num(r, "next").as_u64().unwrap_or(0)).to_string())
        }
        "set-cursor" => {
            let next = arg_u64(a(1), "cursor")?;
            write(conn, &format!("INSERT INTO runtime_cursor (id, next) VALUES (1, {next}) ON DUPLICATE KEY UPDATE next = {next}"))?;
            Ok(String::new())
        }
        "schedule" => {
            write(conn, &schedule_sql(&parse(a(1), "schedule row")?)?)?;
            Ok(String::new())
        }
        "due" => {
            let filter = match a(1) {
                Some(_) => format!(" AND at <= {}", arg_u64(a(1), "now")?),
                None => String::new(),
            };
            let rows = read(
                conn,
                &format!("SELECT row_id, at, stream, kind_tag, kind_body, cause, correlation FROM runtime_schedule WHERE fired = 0{filter} ORDER BY row_id"),
            )?;
            Ok(Value::Array(rows.iter().map(row_of).collect()).to_string())
        }
        "mark-fired" => {
            write(conn, &format!("UPDATE runtime_schedule SET fired = 1 WHERE row_id = {}", lit(text_arg(a(1), "row id")?)))?;
            Ok(String::new())
        }
        "poison" => {
            let p = parse(a(1), "poison")?;
            write(
                conn,
                &format!(
                    "INSERT IGNORE INTO runtime_poison (stream, event_id, handler, message) VALUES ({}, {}, {}, {})",
                    lit(field(&p, "stream")?),
                    lit(field(&p, "event")?),
                    lit(field(&p, "handler")?),
                    lit(field(&p, "message")?)
                ),
            )?;
            Ok(String::new())
        }
        "poisoned" => {
            let rows = read(conn, &format!("SELECT stream, event_id, handler, message FROM runtime_poison WHERE stream = {}", lit(text_arg(a(1), "stream")?)))?;
            Ok(rows.first().map_or("null".into(), |r| json!({ "stream": cell(r, "stream"), "event": cell(r, "event_id"), "handler": cell(r, "handler"), "message": cell(r, "message") }).to_string()))
        }
        _ => Err(format!("journal: unknown op {op:?} (append|read|applied|mark-applied|cursor|set-cursor|schedule|due|mark-fired|poison|poisoned)")),
    }
}

pub fn dispatch(args: &[String], conn: &Conn) -> (i32, String) {
    match run(args, conn) {
        Ok(out) => (0, out),
        Err(e) if e.starts_with("journal:") => (REFUSED, e),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> Value {
        json!({"id": "e'1", "stream": "s", "kind": {"tag": "t", "body": "b\\"}, "at": 7, "cause": null, "correlation": "c"})
    }

    #[test]
    fn append_quotes_every_text_and_nulls_an_absent_cause() {
        let sql = append_sql(&ev()).unwrap();
        assert!(sql.contains("'e\\'1'") && sql.contains("'b\\\\'") && sql.contains("NULL"), "{sql}");
    }

    #[test]
    fn an_event_missing_a_field_is_refused() {
        let mut e = ev();
        e.as_object_mut().unwrap().remove("stream");
        assert!(append_sql(&e).unwrap_err().contains("stream"));
        assert!(schedule_sql(&json!({"id": "r"})).is_err());
    }

    #[test]
    fn a_nul_in_a_payload_is_refused_before_any_sql() {
        assert!(parse(Some(&"{\"a\":\"\0\"}".to_string()), "event").unwrap_err().contains("NUL"));
        assert!(text_arg(Some(&String::new()), "handler").is_err());
    }

    #[test]
    fn a_dropped_connection_is_cannot_tell_and_a_bad_request_is_refused() {
        let conn = Conn::unreachable();
        let go = |v: &[&str]| dispatch(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &conn);
        assert_eq!(go(&["bogus"]).0, REFUSED);
        assert_eq!(go(&["read", "x", "1"]).0, REFUSED);
        assert_eq!(go(&["cursor"]).0, CANNOT_TELL);
    }
}
