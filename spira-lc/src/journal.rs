//! The runtime's store verbs: `journal <op> ...` over the `runtime_*` tables. The runtime
//! reaches the lifecycle store only through this socket, so an idle runtime launches no process.
//! A turn is one transaction of upserts and updates keyed by what they change, never a delete;
//! the runtime reads the store once, at boot.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::db::{Conn, DbError};
use crate::rows::escape;

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

fn lit(s: &str) -> String {
    format!("'{}'", escape(s))
}

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(Value::as_str).ok_or_else(|| format!("journal: missing string {k}"))
}

fn number(v: &Value, k: &str) -> Result<u64, String> {
    v.get(k).and_then(Value::as_u64).ok_or_else(|| format!("journal: missing number {k}"))
}

/// Widths of the text columns, mirroring schema.sql.
const ID_MAX: usize = 255;
const STREAM_MAX: usize = 128;
const TAG_MAX: usize = 64;
const HANDLER_MAX: usize = 128;
const TASK_MAX: usize = 128;
const KEY_MAX: usize = 255;
/// `runtime_schedule.kind_body` is a TEXT column, in bytes.
const KIND_BODY_MAX: usize = 65_535;
const HEAD_KEPT: usize = 4_096;

/// The body as stored: itself when it fits the column, else a JSON stub naming its length and
/// SHA-256 with the head it began with. An over-long body is recorded, not refused.
fn stored_body(body: &str) -> String {
    if body.len() <= KIND_BODY_MAX {
        return body.to_string();
    }
    let cut = (0..=HEAD_KEPT).rev().find(|i| body.is_char_boundary(*i)).unwrap_or(0);
    let digest: String = Sha256::digest(body.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    json!({ "truncated": { "bytes": body.len(), "sha256": digest }, "head": &body[..cut] }).to_string()
}

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

pub fn schedule_sql(row: &Value) -> Result<String, String> {
    let kind = row.get("kind").ok_or("journal: row has no kind")?;
    check_widths("schedule", "row_id", row)?;
    Ok(format!(
        "INSERT INTO runtime_schedule (row_id, at, stream, kind_tag, kind_body, cause, correlation, fired) VALUES ({}, {}, {}, {}, {}, {}, {}, 0) ON DUPLICATE KEY UPDATE row_id = row_id",
        lit(field(row, "id")?),
        number(row, "at")?,
        lit(field(row, "stream")?),
        lit(field(kind, "tag")?),
        lit(&stored_body(field(kind, "body")?)),
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

fn fired_sql(row_id: &str) -> String {
    format!("UPDATE runtime_schedule SET fired = 1 WHERE row_id = {}", lit(row_id))
}

fn state_sql(row: &Value) -> Result<String, String> {
    let (task, skey) = (field(row, "task")?, field(row, "skey")?);
    check_text("turn", "task", task, TASK_MAX)?;
    check_text("turn", "skey", skey, KEY_MAX)?;
    let (value, at) = (field(row, "value")?, number(row, "at")?);
    Ok(format!("INSERT INTO runtime_task_state (task, skey, value, at) VALUES ({}, {}, {}, {at}) ON DUPLICATE KEY UPDATE value = {}, at = {at}", lit(task), lit(skey), lit(value), lit(value)))
}

fn effect_sql(row: &Value) -> Result<String, String> {
    let (task, ekey, request, stream) = (field(row, "task")?, field(row, "ekey")?, field(row, "request_id")?, field(row, "stream")?);
    check_text("turn", "task", task, TASK_MAX)?;
    check_text("turn", "ekey", ekey, KEY_MAX)?;
    check_text("turn", "request_id", request, ID_MAX)?;
    check_text("turn", "stream", stream, STREAM_MAX)?;
    let spec = row.get("spec").ok_or("journal: effect has no spec")?.to_string();
    let (requested, deadline) = (number(row, "requested_at")?, number(row, "deadline_at")?);
    Ok(format!(
        "INSERT INTO runtime_effect (task, ekey, request_id, stream, spec, requested_at, deadline_at, resolved) VALUES ({}, {}, {}, {}, {}, {requested}, {deadline}, 0) ON DUPLICATE KEY UPDATE request_id = {r}, stream = {s}, spec = {sp}, requested_at = {requested}, deadline_at = {deadline}, resolved = 0",
        lit(task),
        lit(ekey),
        lit(request),
        lit(stream),
        lit(&spec),
        r = lit(request),
        s = lit(stream),
        sp = lit(&spec),
    ))
}

fn resolved_sql(request: &str) -> Result<String, String> {
    check_text("turn", "request_id", request, ID_MAX)?;
    Ok(format!("UPDATE runtime_effect SET resolved = 1 WHERE request_id = {}", lit(request)))
}

fn list<'a>(turn: &'a Value, k: &str) -> Result<&'a [Value], String> {
    match turn.get(k) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(other) => Err(format!("journal: turn refused: {k} is not a list: {}", clip(&other.to_string()))),
    }
}

/// The statements of one turn, in the order they apply: state, effect requests, schedule rows,
/// then the outcomes and fired marks that close earlier ones. Every one is checked before any
/// is written, so a refusal writes nothing.
pub fn turn_sqls(turn: &Value) -> Result<Vec<String>, String> {
    let mut sqls = vec![];
    for r in list(turn, "state")? {
        sqls.push(state_sql(r)?);
    }
    for r in list(turn, "effects")? {
        sqls.push(effect_sql(r)?);
    }
    for r in list(turn, "schedules")? {
        sqls.push(schedule_sql(r)?);
    }
    for id in list(turn, "resolved")? {
        sqls.push(resolved_sql(id.as_str().filter(|s| !s.is_empty()).ok_or_else(|| format!("journal: turn refused: a resolved mark is not a request id: {id}"))?)?);
    }
    for id in list(turn, "fired")? {
        let id = id.as_str().filter(|s| !s.is_empty() && !s.contains('\0')).ok_or_else(|| format!("journal: turn refused: a fired mark is not a row id: {id}"))?;
        sqls.push(fired_sql(id));
    }
    Ok(sqls)
}

fn run(args: &[String], conn: &Conn) -> Result<String, String> {
    let op = args.first().map(String::as_str).unwrap_or("");
    let (read, write) = (|c: &Conn, s: &str| read(c, op, s), |c: &Conn, s: &str, who: Option<&Value>| write(c, op, s, who));
    let a = |i: usize| args.get(i);
    match op {
        "turn" => {
            let turn = parse(a(1), "turn")?;
            let sqls = turn_sqls(&turn)?;
            if !sqls.is_empty() {
                conn.append_events(&sqls).map_err(|e| failed(op, &sqls.join(";\n"), None, e))?;
            }
            Ok(String::new())
        }
        "state" => {
            let rows = read(conn, "SELECT task, skey, value, at FROM runtime_task_state ORDER BY task, skey")?;
            Ok(Value::Array(rows.iter().map(|r| json!({ "task": cell(r, "task"), "skey": cell(r, "skey"), "value": cell(r, "value"), "at": num(r, "at") })).collect()).to_string())
        }
        "open-effects" => {
            let rows = read(conn, "SELECT task, ekey, request_id, stream, spec, requested_at, deadline_at FROM runtime_effect WHERE resolved = 0 ORDER BY requested_at, request_id")?;
            let mut out = vec![];
            for r in &rows {
                let spec: Value = cell(r, "spec").as_str().and_then(|t| serde_json::from_str(t).ok()).ok_or_else(|| format!("journal: effect {} has an unreadable spec", cell(r, "request_id")))?;
                out.push(json!({ "task": cell(r, "task"), "ekey": cell(r, "ekey"), "request_id": cell(r, "request_id"), "stream": cell(r, "stream"), "spec": spec, "requested_at": num(r, "requested_at"), "deadline_at": num(r, "deadline_at") }));
            }
            Ok(Value::Array(out).to_string())
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
        _ => Err(format!("journal: unknown op {op:?} (turn|state|open-effects|due|poison|poisoned)")),
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

    fn effect() -> Value {
        json!({"task": "t", "ekey": "k'1", "request_id": "r1", "stream": "s", "spec": {"name": "n", "input": "i", "deadline_ms": 5, "key": "k'1"}, "requested_at": 1, "deadline_at": 6})
    }

    #[test]
    fn a_turn_upserts_state_and_effects_and_updates_what_it_closes_and_deletes_nothing() {
        let row = json!({"id": "r1", "at": 5, "stream": "s", "kind": {"tag": "tick", "body": ""}, "cause": null, "correlation": "c"});
        let turn = json!({"state": [{"task": "t", "skey": "carry", "value": "v'", "at": 2}], "effects": [effect()], "schedules": [row], "resolved": ["r0"], "fired": ["r9"]});
        let sqls = turn_sqls(&turn).unwrap();
        let heads: Vec<&str> = sqls.iter().map(|s| s.split(" (").next().unwrap().split(" SET").next().unwrap()).collect();
        assert_eq!(heads, ["INSERT INTO runtime_task_state", "INSERT INTO runtime_effect", "INSERT INTO runtime_schedule", "UPDATE runtime_effect", "UPDATE runtime_schedule"], "{sqls:?}");
        assert!(sqls[0].contains("'v\\''") && sqls[0].contains("ON DUPLICATE KEY UPDATE value"), "{}", sqls[0]);
        assert!(sqls[1].contains("'k\\'1'") && sqls[1].contains("resolved = 0"), "{}", sqls[1]);
        assert_eq!(sqls[3], "UPDATE runtime_effect SET resolved = 1 WHERE request_id = 'r0'");
        assert!(sqls.iter().all(|s| !s.to_uppercase().contains("DELETE")), "{sqls:?}");
        assert!(turn_sqls(&json!({})).unwrap().is_empty(), "an empty turn writes nothing");
    }

    #[test]
    fn a_turn_with_a_bad_write_refuses_all_of_it() {
        let wide = |k: &str, n: usize| {
            let mut e = effect();
            e[k] = json!("x".repeat(n));
            json!({"effects": [e]})
        };
        for bad in [
            json!({"state": [{"task": "t".repeat(129), "skey": "carry", "value": "v", "at": 1}]}),
            json!({"state": [{"task": "t", "skey": "k".repeat(256), "value": "v", "at": 1}]}),
            wide("ekey", 256),
            wide("request_id", 256),
            wide("stream", 129),
            json!({"effects": [{"task": "t"}]}),
            json!({"resolved": [""]}),
            json!({"fired": [7]}),
            json!({"state": {"task": "t"}}),
            json!({"state": [{"task": "t", "skey": "carry", "value": "v", "at": 1}], "fired": [""]}),
        ] {
            assert!(turn_sqls(&bad).unwrap_err().starts_with("journal:"), "{bad}");
        }
        assert!(turn_sqls(&wide("ekey", 255)).is_ok(), "the limit itself is accepted");
    }

    #[test]
    fn a_nul_in_a_payload_is_refused_before_any_sql() {
        assert!(parse(Some(&"{\"a\":\"\0\"}".to_string()), "event").unwrap_err().contains("NUL"));
        assert!(text_arg(Some(&String::new()), "handler").is_err());
    }

    #[test]
    fn an_over_length_schedule_id_is_refused_naming_the_row_its_fields_and_their_lengths() {
        let r = schedule_sql(&json!({"id": "r".repeat(260), "at": 1, "stream": "s", "kind": {"tag": "t", "body": ""}, "cause": null, "correlation": "c"})).unwrap_err();
        assert!(r.starts_with("journal: schedule refused: row_id is 260 chars (max 255);") && r.contains("kind=t"), "{r}");
        assert!(schedule_sql(&json!({"id": "r"})).is_err());
    }

    /// runtime_poison is append-only for spira_lc (lifecycle/grants.sql: SELECT, INSERT): a dedupe
    /// that needs UPDATE is "command denied" in production though a root-run test passes.
    #[test]
    fn appends_to_append_only_tables_need_no_update_grant() {
        let src = include_str!("journal.rs");
        let table = "runtime_poison";
        let bad = src.lines().filter(|l| l.contains(&format!("INSERT INTO {table}")) && l.contains("DUPLICATE KEY")).count();
        assert_eq!(bad, 0, "{table} is granted SELECT, INSERT only; ON DUPLICATE KEY UPDATE is denied");
    }

    /// The runtime checks a write against these widths before it sends it (runtime/src/socket.rs).
    #[test]
    fn the_runtime_checks_only_inside_these_widths() {
        let src = include_str!("../../runtime/src/socket.rs");
        for (name, max) in [("ID_MAX", ID_MAX), ("STREAM_MAX", STREAM_MAX), ("TASK_MAX", TASK_MAX), ("KEY_MAX", KEY_MAX)] {
            assert!(src.contains(&format!("const {name}: usize = {max};")), "runtime/src/socket.rs {name} differs from {max}");
        }
    }

    #[test]
    fn a_schedule_body_over_the_column_is_stored_as_a_digest_and_head_never_dropped() {
        let big = format!("{{\"runs\":[\"{}é\"]}}", "r".repeat(KIND_BODY_MAX + 10));
        let row = |body: &str| json!({"id": "r", "at": 1, "stream": "s", "kind": {"tag": "t", "body": body}, "cause": null, "correlation": "c"});
        let sql = schedule_sql(&row(&big)).unwrap();
        assert!(sql.contains("truncated") && sql.len() < KIND_BODY_MAX, "{} bytes", sql.len());
        let stub = stored_body(&big);
        let v: Value = serde_json::from_str(&stub).unwrap();
        assert_eq!(v["truncated"]["bytes"], big.len());
        assert_eq!(v["truncated"]["sha256"].as_str().unwrap().len(), 64);
        assert!(big.starts_with(v["head"].as_str().unwrap()));
        let at_limit = "x".repeat(KIND_BODY_MAX);
        assert_eq!(stored_body(&at_limit), at_limit, "a body that fits is stored as sent");
    }

    #[test]
    fn a_long_statement_is_clipped_to_the_limit_and_an_ellipsis() {
        let sql = state_sql(&json!({"task": "t", "skey": "carry", "value": "x".repeat(10_000), "at": 1})).unwrap();
        assert!(sql.len() > 10_000);
        assert_eq!(clip(&sql).chars().count(), CLIP + 1);
        assert_eq!(clip("short"), "short");
    }
}
