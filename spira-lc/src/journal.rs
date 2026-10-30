//! The runtime's journal verbs: `journal <op> ...` over the `runtime_*` tables. The runtime
//! reaches the lifecycle store only through this socket, so an idle runtime launches no process.
//! Every write is idempotent on its key; an event or schedule row is one JSON argument.

use serde_json::{json, Value};

use crate::db::{Conn, DbError};
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

/// Widths of the text columns, mirroring migrations/0030-runtime-journal.sql.
const ID_MAX: usize = 255;
const STREAM_MAX: usize = 128;
const TAG_MAX: usize = 64;
const HANDLER_MAX: usize = 128;

fn text(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// The fields that name an event or row, so a message about it cannot lose them to a clip.
fn describe(v: &Value) -> String {
    let (id, stream, tag) = (text(v, "id"), text(v, "stream"), text(v.get("kind").unwrap_or(&Value::Null), "tag"));
    let (cause, corr) = (text(v, "cause"), text(v, "correlation"));
    format!(
        "id={id} stream={stream} kind={tag} lengths={{id:{},stream:{},tag:{},cause:{},correlation:{}}}",
        chars(&id),
        chars(&stream),
        chars(&tag),
        chars(&cause),
        chars(&corr)
    )
}

fn check_widths(what: &str, id_column: &str, v: &Value) -> Result<(), String> {
    let kind = v.get("kind").unwrap_or(&Value::Null);
    let fields = [
        (id_column, text(v, "id"), ID_MAX),
        ("stream", text(v, "stream"), STREAM_MAX),
        ("kind_tag", text(kind, "tag"), TAG_MAX),
        ("cause", text(v, "cause"), ID_MAX),
        ("correlation", text(v, "correlation"), ID_MAX),
    ];
    for (name, value, max) in fields {
        if chars(&value) > max {
            return Err(format!("journal: {what} refused: {name} is {} chars (max {max}); {}", chars(&value), describe(v)));
        }
    }
    Ok(())
}

fn check_text(what: &str, name: &str, value: &str, max: usize) -> Result<(), String> {
    match chars(value) {
        n if n > max => Err(format!("journal: {what} refused: {name} is {n} chars (max {max}); {name}={value}")),
        _ => Ok(()),
    }
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
    check_widths("append", "event_id", ev)?;
    Ok(format!(
        "INSERT INTO runtime_event (event_id, stream, kind_tag, kind_body, at, cause, correlation) SELECT {id}, {}, {}, {}, {}, {}, {} FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM runtime_event WHERE event_id = {id})",
        lit(field(ev, "stream")?),
        lit(field(kind, "tag")?),
        lit(field(kind, "body")?),
        number(ev, "at")?,
        opt_lit(ev, "cause"),
        lit(field(ev, "correlation")?),
        id = lit(field(ev, "id")?),
    ))
}

/// Requests with no outcome event naming them as cause; `runtime_event_cause_idx` makes each probe a seek.
pub fn open_effects_sql() -> String {
    "SELECT r.seq, r.event_id, r.stream, r.kind_tag, r.kind_body, r.at, r.cause, r.correlation FROM runtime_event r WHERE r.kind_tag = 'effect.requested' AND NOT EXISTS (SELECT 1 FROM runtime_event o WHERE o.cause = r.event_id AND o.kind_tag IN ('effect.succeeded', 'effect.failed', 'effect.timed_out')) ORDER BY r.seq".into()
}

/// The newest `limit` events of a stream below `below`, newest first; `runtime_event_stream_idx` walks them backward.
pub fn stream_sql(stream: &str, below: u64, limit: u64) -> String {
    format!("SELECT seq, event_id, stream, kind_tag, kind_body, at, cause, correlation FROM runtime_event WHERE stream = {} AND seq < {below} ORDER BY seq DESC LIMIT {limit}", lit(stream))
}

pub fn schedule_sql(row: &Value) -> Result<String, String> {
    let kind = row.get("kind").ok_or("journal: row has no kind")?;
    check_widths("schedule", "row_id", row)?;
    Ok(format!(
        "INSERT INTO runtime_schedule (row_id, at, stream, kind_tag, kind_body, cause, correlation, fired) VALUES ({}, {}, {}, {}, {}, {}, {}, 0) ON DUPLICATE KEY UPDATE row_id = row_id",
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

const CLIP: usize = 240;

fn clip(sql: &str) -> String {
    match sql.char_indices().nth(CLIP) {
        Some((i, _)) => format!("{}…", &sql[..i]),
        None => sql.to_string(),
    }
}

fn failed(op: &str, sql: &str, subject: Option<&Value>, e: DbError) -> String {
    let DbError::CannotTell(m) = e;
    let who = subject.map_or(String::new(), |v| format!(" [{}]", describe(v)));
    format!("journal {op}: {m}{who} — statement: {}", clip(sql))
}

fn write(conn: &Conn, op: &str, sql: &str, subject: Option<&Value>) -> Result<(), String> {
    conn.append_event(sql).map_err(|e| failed(op, sql, subject, e))
}

fn read(conn: &Conn, op: &str, sql: &str) -> Result<Vec<Value>, String> {
    conn.query(sql).map_err(|e| failed(op, sql, None, e))
}

fn run(args: &[String], conn: &Conn) -> Result<String, String> {
    let op = args.first().map(String::as_str).unwrap_or("");
    let (read, write) = (|c: &Conn, s: &str| read(c, op, s), |c: &Conn, s: &str, who: Option<&Value>| write(c, op, s, who));
    let a = |i: usize| args.get(i);
    match op {
        "append" => {
            let ev = parse(a(1), "event")?;
            let id = field(&ev, "id")?.to_string();
            write(conn, &append_sql(&ev)?, Some(&ev))?;
            let rows = read(conn, &format!("SELECT seq FROM runtime_event WHERE event_id = {}", lit(&id)))?;
            match rows.first() {
                Some(r) => Ok(num(r, "seq").to_string()),
                None => {
                    let warnings = read(conn, &format!("SELECT seq FROM runtime_event WHERE event_id = {}; SHOW WARNINGS", lit(&id))).map_or_else(|e| e, |w| Value::Array(w).to_string());
                    Err(format!("journal: appended event not found; {}; warnings={warnings}", describe(&ev)))
                }
            }
        }
        "read" => {
            let (from, limit) = (arg_u64(a(1), "from")?, arg_u64(a(2), "limit")?.min(READ_MAX));
            let rows = read(
                conn,
                &format!("SELECT seq, event_id, stream, kind_tag, kind_body, at, cause, correlation FROM runtime_event WHERE seq >= {from} ORDER BY seq LIMIT {limit}"),
            )?;
            Ok(Value::Array(rows.iter().map(|r| json!([num(r, "seq"), event_of(r)])).collect()).to_string())
        }
        "stream" => {
            let (stream, below, limit) = (text_arg(a(1), "stream")?, arg_u64(a(2), "below")?, arg_u64(a(3), "limit")?.min(READ_MAX));
            let mut rows = read(conn, &stream_sql(stream, below, limit))?;
            rows.reverse();
            Ok(Value::Array(rows.iter().map(|r| json!([num(r, "seq"), event_of(r)])).collect()).to_string())
        }
        "end" => {
            let rows = read(conn, "SELECT MAX(seq) AS top FROM runtime_event")?;
            Ok(rows.first().and_then(|r| num(r, "top").as_u64()).map_or(0, |top| top + 1).to_string())
        }
        "open-effects" => {
            let rows = read(conn, &open_effects_sql())?;
            Ok(Value::Array(rows.iter().map(|r| json!([num(r, "seq"), event_of(r)])).collect()).to_string())
        }
        "applied" | "mark-applied" => {
            let (handler, id) = (text_arg(a(1), "handler")?, text_arg(a(2), "event id")?);
            if op == "mark-applied" {
                check_text("mark-applied", "handler", handler, HANDLER_MAX)?;
                check_text("mark-applied", "event_id", id, ID_MAX)?;
                let (h, i) = (lit(handler), lit(id));
                write(conn, &format!("INSERT INTO runtime_applied (handler, event_id) SELECT {h}, {i} FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM runtime_applied WHERE handler = {h} AND event_id = {i})"), None)?;
                return Ok(String::new());
            }
            let rows = read(conn, &format!("SELECT 1 AS hit FROM runtime_applied WHERE handler = {} AND event_id = {}", lit(handler), lit(id)))?;
            Ok(if rows.is_empty() { "0" } else { "1" }.into())
        }
        "cursor" => {
            let rows = read(conn, "SELECT `next` FROM runtime_cursor WHERE id = 1")?;
            Ok(rows.first().map_or(0, |r| num(r, "next").as_u64().unwrap_or(0)).to_string())
        }
        "set-cursor" => {
            let next = arg_u64(a(1), "cursor")?;
            write(conn, &format!("INSERT INTO runtime_cursor (id, `next`) VALUES (1, {next}) ON DUPLICATE KEY UPDATE `next` = {next}"), None)?;
            Ok(String::new())
        }
        "schedule" => {
            let row = parse(a(1), "schedule row")?;
            write(conn, &schedule_sql(&row)?, Some(&row))?;
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
            write(conn, &format!("UPDATE runtime_schedule SET fired = 1 WHERE row_id = {}", lit(text_arg(a(1), "row id")?)), None)?;
            Ok(String::new())
        }
        "poison" => {
            let p = parse(a(1), "poison")?;
            let (stream, event, handler) = (field(&p, "stream")?, field(&p, "event")?, field(&p, "handler")?);
            check_text("poison", "stream", stream, STREAM_MAX)?;
            check_text("poison", "event_id", event, ID_MAX)?;
            check_text("poison", "handler", handler, HANDLER_MAX)?;
            write(
                conn,
                &format!(
                    "INSERT INTO runtime_poison (stream, event_id, handler, message) SELECT {s}, {}, {}, {} FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM runtime_poison WHERE stream = {s})",
                    lit(event),
                    lit(handler),
                    lit(field(&p, "message")?),
                    s = lit(stream),
                ),
                None,
            )?;
            Ok(String::new())
        }
        "poisoned" => {
            let rows = read(conn, &format!("SELECT stream, event_id, handler, message FROM runtime_poison WHERE stream = {}", lit(text_arg(a(1), "stream")?)))?;
            Ok(rows.first().map_or("null".into(), |r| json!({ "stream": cell(r, "stream"), "event": cell(r, "event_id"), "handler": cell(r, "handler"), "message": cell(r, "message") }).to_string()))
        }
        _ => Err(format!("journal: unknown op {op:?} (append|read|stream|end|open-effects|applied|mark-applied|cursor|set-cursor|schedule|due|mark-fired|poison|poisoned)")),
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
    fn the_runtime_window_fits_inside_one_stream_read() {
        let src = include_str!("../../runtime/src/event.rs");
        let window: u64 = src.split("pub const WINDOW: usize = ").nth(1).and_then(|r| r.split(';').next()).unwrap().trim().parse().unwrap();
        assert!(window <= READ_MAX, "the runtime asks for {window}, a stream read returns {READ_MAX}");
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
    fn an_over_length_id_is_refused_naming_the_event_its_fields_and_their_lengths() {
        let id = "i".repeat(266);
        let e = append_sql(&json!({"id": id, "stream": "runtime.heartbeat", "kind": {"tag": "heartbeat", "body": ""}, "at": 1, "cause": null, "correlation": "c"})).unwrap_err();
        assert!(e.starts_with("journal: append refused: event_id is 266 chars (max 255);") && e.contains(&id) && e.contains("stream=runtime.heartbeat") && e.contains("kind=heartbeat") && e.contains("id:266,stream:17,tag:9"), "{e}");
        let r = schedule_sql(&json!({"id": "r".repeat(260), "at": 1, "stream": "s", "kind": {"tag": "t", "body": ""}, "cause": null, "correlation": "c"})).unwrap_err();
        assert!(r.contains("row_id is 260 chars (max 255)"), "{r}");
        for (k, v) in [("stream", "s".repeat(129)), ("correlation", "c".repeat(256)), ("cause", "x".repeat(256))] {
            let mut ev = ev();
            ev[k] = json!(v);
            assert!(append_sql(&ev).unwrap_err().contains(&format!("{k} is")), "{k}");
        }
        assert!(append_sql(&json!({"id": "i".repeat(255), "stream": "s".repeat(128), "kind": {"tag": "t".repeat(64), "body": ""}, "at": 1, "cause": null, "correlation": "c"})).is_ok(), "the limits themselves are accepted");
    }

    #[test]
    fn no_write_swallows_a_conflict_but_ignore() {
        let sql = append_sql(&ev()).unwrap();
        assert!(!sql.contains("IGNORE") && sql.contains("WHERE NOT EXISTS (SELECT 1 FROM runtime_event WHERE event_id = "), "{sql}");
    }

    /// runtime_event, runtime_applied and runtime_poison are append-only for spira_lc
    /// (lifecycle/grants.sql: SELECT, INSERT): a dedupe that needs UPDATE is "command denied"
    /// in production (sp-6gsvrm, 2026-10-29) though a root-run test passes.
    #[test]
    fn appends_to_append_only_tables_need_no_update_grant() {
        let sql = append_sql(&ev()).unwrap();
        assert!(!sql.to_uppercase().contains("UPDATE"), "{sql}");
        let src = include_str!("journal.rs");
        for table in ["runtime_event", "runtime_applied", "runtime_poison"] {
            let bad = src.lines().filter(|l| l.contains(&format!("INSERT INTO {table}")) && l.contains("DUPLICATE KEY")).count();
            assert_eq!(bad, 0, "{table} is granted SELECT, INSERT only; ON DUPLICATE KEY UPDATE is denied");
        }
    }

    #[test]
    fn the_dolt_suite_explains_the_statements_the_ops_send() {
        let suite = include_str!("../tests/journal_dolt.rs");
        for sql in [stream_sql("s0", 100000, 1000), open_effects_sql()] {
            assert!(suite.contains(&format!("\"{sql}\"")), "tests/journal_dolt.rs must EXPLAIN exactly: {sql}");
        }
    }

    #[test]
    fn a_dropped_connection_is_cannot_tell_and_a_bad_request_is_refused() {
        let conn = Conn::unreachable();
        let go = |v: &[&str]| dispatch(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &conn);
        assert_eq!(go(&["bogus"]).0, REFUSED);
        assert_eq!(go(&["read", "x", "1"]).0, REFUSED);
        assert_eq!(go(&["cursor"]).0, CANNOT_TELL);
    }

    #[test]
    fn a_failed_write_names_the_event_past_the_statement_clip() {
        let ev = json!({"id": "e".repeat(250), "stream": "s", "kind": {"tag": "t", "body": ""}, "at": 1, "cause": null, "correlation": "c"});
        let (code, body) = dispatch(&["append".to_string(), ev.to_string()], &Conn::unreachable());
        assert_eq!(code, CANNOT_TELL);
        assert!(body.contains(&format!("id={}", "e".repeat(250))) && body.contains("stream=s") && body.contains("id:250"), "{body}");
    }

    #[test]
    fn a_long_statement_is_clipped_to_the_limit_and_an_ellipsis() {
        let sql = append_sql(&json!({"id": "e", "stream": "s", "kind": {"tag": "t", "body": "x".repeat(10_000)}, "at": 1, "cause": null, "correlation": "c"})).unwrap();
        assert!(sql.len() > 10_000);
        assert_eq!(clip(&sql).chars().count(), CLIP + 1);
        assert_eq!(clip("short"), "short");
    }

    #[test]
    fn a_failed_call_names_its_verb() {
        let (code, body) = dispatch(&["cursor".to_string()], &Conn::unreachable());
        assert_eq!(code, CANNOT_TELL);
        assert!(body.starts_with("cannot tell: journal cursor:") && !body.contains("CannotTell("), "{body}");
    }
}
