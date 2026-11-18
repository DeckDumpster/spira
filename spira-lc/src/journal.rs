//! The runtime's store verbs: `journal <op> ...` over the control store (`control::journal`). The
//! runtime reaches the store only through this socket, so an idle runtime launches no process.
//! A turn is upserts keyed by what they change, never a delete of a row; the runtime reads the
//! store once, at boot.

use serde_json::{json, Value};

use crate::control::journal::{EffectRow, PoisonRow, ScheduleRow, StateRow, Turn};

const CANNOT_TELL: i32 = 2;
const REFUSED: i32 = 3;

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(Value::as_str).ok_or_else(|| format!("journal: missing string {k}"))
}

fn number(v: &Value, k: &str) -> Result<u64, String> {
    v.get(k).and_then(Value::as_u64).ok_or_else(|| format!("journal: missing number {k}"))
}

/// Widths the runtime checks a write against before it sends it (runtime/src/socket.rs).
const ID_MAX: usize = 255;
const STREAM_MAX: usize = 128;
const TAG_MAX: usize = 64;
const HANDLER_MAX: usize = 128;
const TASK_MAX: usize = 128;
const KEY_MAX: usize = 255;

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

fn schedule_row(row: &Value) -> Result<ScheduleRow, String> {
    let kind = row.get("kind").ok_or("journal: row has no kind")?;
    check_widths("schedule", "row_id", row)?;
    Ok(ScheduleRow {
        id: field(row, "id")?.to_string(),
        at: number(row, "at")?,
        stream: field(row, "stream")?.to_string(),
        tag: field(kind, "tag")?.to_string(),
        body: field(kind, "body")?.to_string(),
        cause: row.get("cause").and_then(Value::as_str).map(str::to_string),
        correlation: field(row, "correlation")?.to_string(),
    })
}

fn row_of(r: &ScheduleRow) -> Value {
    json!({ "id": r.id, "at": r.at, "stream": r.stream, "kind": { "tag": r.tag, "body": r.body }, "cause": r.cause, "correlation": r.correlation })
}

const CLIP: usize = 240;

fn clip(text: &str) -> String {
    match text.char_indices().nth(CLIP) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

fn state_row(row: &Value) -> Result<StateRow, String> {
    let (task, skey) = (field(row, "task")?, field(row, "skey")?);
    check_text("turn", "task", task, TASK_MAX)?;
    check_text("turn", "skey", skey, KEY_MAX)?;
    Ok(StateRow { task: task.into(), skey: skey.into(), value: field(row, "value")?.into(), at: number(row, "at")? })
}

fn effect_row(row: &Value) -> Result<EffectRow, String> {
    let (task, ekey, request, stream) = (field(row, "task")?, field(row, "ekey")?, field(row, "request_id")?, field(row, "stream")?);
    check_text("turn", "task", task, TASK_MAX)?;
    check_text("turn", "ekey", ekey, KEY_MAX)?;
    check_text("turn", "request_id", request, ID_MAX)?;
    check_text("turn", "stream", stream, STREAM_MAX)?;
    let spec = row.get("spec").ok_or("journal: effect has no spec")?.clone();
    Ok(EffectRow { task: task.into(), ekey: ekey.into(), request_id: request.into(), stream: stream.into(), spec, requested_at: number(row, "requested_at")?, deadline_at: number(row, "deadline_at")? })
}

fn list<'a>(turn: &'a Value, k: &str) -> Result<&'a [Value], String> {
    match turn.get(k) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) => Ok(a),
        Some(other) => Err(format!("journal: turn refused: {k} is not a list: {}", clip(&other.to_string()))),
    }
}

/// One turn, every write checked before any is made, so a refusal writes nothing. The order it
/// applies in: state, effect requests, schedule rows, then the outcomes and fired marks that
/// close earlier ones.
pub fn turn_of(turn: &Value) -> Result<Turn, String> {
    let mut out = Turn::default();
    for r in list(turn, "state")? {
        out.state.push(state_row(r)?);
    }
    for r in list(turn, "effects")? {
        out.effects.push(effect_row(r)?);
    }
    for r in list(turn, "schedules")? {
        out.schedules.push(schedule_row(r)?);
    }
    for id in list(turn, "resolved")? {
        let id = id.as_str().filter(|s| !s.is_empty()).ok_or_else(|| format!("journal: turn refused: a resolved mark is not a request id: {id}"))?;
        check_text("turn", "request_id", id, ID_MAX)?;
        out.resolved.push(id.to_string());
    }
    for id in list(turn, "fired")? {
        let id = id.as_str().filter(|s| !s.is_empty() && !s.contains('\0')).ok_or_else(|| format!("journal: turn refused: a fired mark is not a row id: {id}"))?;
        out.fired.push(id.to_string());
    }
    Ok(out)
}

fn failed(op: &str, what: impl std::fmt::Display) -> String {
    format!("journal {op}: {what}")
}

fn run<B: state_store::Backend>(args: &[String], store: &crate::control::store::Store<B>) -> Result<String, String> {
    let op = args.first().map(String::as_str).unwrap_or("");
    let a = |i: usize| args.get(i);
    let cannot = |e: state_store::Error| failed(op, e);
    match op {
        "turn" => {
            let turn = turn_of(&parse(a(1), "turn")?)?;
            store.journal_turn(&turn, "runtime").map_err(cannot)?;
            Ok(String::new())
        }
        "state" => {
            let rows = store.journal_state().map_err(cannot)?;
            Ok(Value::Array(rows.iter().map(|r| json!({ "task": r.task, "skey": r.skey, "value": r.value, "at": r.at })).collect()).to_string())
        }
        "open-effects" => {
            let rows = store.journal_open_effects().map_err(cannot)?;
            Ok(Value::Array(rows.iter().map(|r| json!({ "task": r.task, "ekey": r.ekey, "request_id": r.request_id, "stream": r.stream, "spec": r.spec, "requested_at": r.requested_at, "deadline_at": r.deadline_at })).collect()).to_string())
        }
        "due" => {
            let now = match a(1) {
                Some(_) => Some(arg_u64(a(1), "now")?),
                None => None,
            };
            let rows = store.journal_due(now).map_err(cannot)?;
            Ok(Value::Array(rows.iter().map(row_of).collect()).to_string())
        }
        "poison" => {
            let p = parse(a(1), "poison")?;
            let (stream, event, handler) = (field(&p, "stream")?, field(&p, "event")?, field(&p, "handler")?);
            check_text("poison", "stream", stream, STREAM_MAX)?;
            check_text("poison", "event_id", event, ID_MAX)?;
            check_text("poison", "handler", handler, HANDLER_MAX)?;
            let row = PoisonRow { stream: stream.into(), event: event.into(), handler: handler.into(), message: field(&p, "message")?.into() };
            store.journal_poison(&row, "runtime").map_err(cannot)?;
            Ok(String::new())
        }
        "poisoned" => {
            let stream = text_arg(a(1), "stream")?;
            let row = store.journal_poisoned(stream).map_err(cannot)?;
            Ok(row.map_or("null".into(), |r| json!({ "stream": r.stream, "event": r.event, "handler": r.handler, "message": r.message }).to_string()))
        }
        _ => Err(format!("journal: unknown op {op:?} (turn|state|open-effects|due|poison|poisoned)")),
    }
}

pub fn dispatch(args: &[String]) -> (i32, String) {
    let store = match crate::control::served::store() {
        Ok(s) => s,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    answer(run(args, &*store))
}

fn answer(r: Result<String, String>) -> (i32, String) {
    match r {
        Ok(out) => (0, out),
        Err(e) if e.starts_with("journal:") => (REFUSED, e),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports};

    fn store() -> crate::control::store::Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    fn call(s: &crate::control::store::Store<MemBackend>, argv: &[&str]) -> (i32, String) {
        answer(run(&argv.iter().map(|a| a.to_string()).collect::<Vec<_>>(), s))
    }

    fn effect() -> Value {
        json!({"task": "t", "ekey": "k'1/x", "request_id": "r1", "stream": "s", "spec": {"name": "n", "input": "i", "deadline_ms": 5, "key": "k'1"}, "requested_at": 1, "deadline_at": 6})
    }

    fn sched(id: &str, at: u64) -> Value {
        json!({"id": id, "at": at, "stream": "s", "kind": {"tag": "tick", "body": "b"}, "cause": null, "correlation": "c"})
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
            assert!(turn_of(&bad).unwrap_err().starts_with("journal:"), "{bad}");
        }
        assert!(turn_of(&wide("ekey", 255)).is_ok(), "the limit itself is accepted");
        assert_eq!(turn_of(&json!({})).unwrap(), Turn::default(), "an empty turn writes nothing");
    }

    #[test]
    fn a_nul_in_a_payload_is_refused_before_any_write() {
        assert!(parse(Some(&"{\"a\":\"\0\"}".to_string()), "event").unwrap_err().contains("NUL"));
        assert!(text_arg(Some(&String::new()), "handler").is_err());
    }

    #[test]
    fn an_over_length_schedule_id_is_refused_naming_the_row_its_fields_and_their_lengths() {
        let r = schedule_row(&sched(&"r".repeat(260), 1)).unwrap_err();
        assert!(r.starts_with("journal: schedule refused: row_id is 260 chars (max 255);") && r.contains("kind=tick"), "{r}");
        assert!(schedule_row(&json!({"id": "r"})).is_err());
    }

    #[test]
    fn what_a_turn_wrote_reads_back_and_what_it_closed_is_no_longer_open() {
        let s = store();
        let turn = json!({"state": [{"task": "t", "skey": "carry", "value": "v'", "at": 2}], "effects": [effect()], "schedules": [sched("r2", 5), sched("r1", 4)]});
        assert_eq!(call(&s, &["turn", &turn.to_string()]).0, 0);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["state"]).1).unwrap(), json!([{"task": "t", "skey": "carry", "value": "v'", "at": 2}]));
        let open: Value = serde_json::from_str(&call(&s, &["open-effects"]).1).unwrap();
        assert_eq!(open[0]["ekey"], "k'1/x");
        let due: Value = serde_json::from_str(&call(&s, &["due"]).1).unwrap();
        assert_eq!(due.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>(), ["r1", "r2"], "by row id");
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["due", "4"]).1).unwrap().as_array().unwrap().len(), 1, "only what is due at `now`");
        let close = json!({"resolved": ["r1"], "fired": ["r1"]});
        assert_eq!(call(&s, &["turn", &close.to_string()]).0, 0);
        assert_eq!(call(&s, &["open-effects"]).1, "[]");
        let due: Value = serde_json::from_str(&call(&s, &["due"]).1).unwrap();
        assert_eq!(due.as_array().unwrap().len(), 1);
        assert_eq!(due[0]["id"], "r2");
    }

    #[test]
    fn an_effect_upserted_again_after_it_resolved_is_open_again_and_a_schedule_row_inserted_again_is_kept_as_it_was() {
        let s = store();
        let first = json!({"effects": [effect()], "schedules": [sched("r1", 4)]});
        call(&s, &["turn", &first.to_string()]);
        call(&s, &["turn", &json!({"resolved": ["r1"], "fired": ["r1"]}).to_string()]);
        let again = json!({"effects": [effect()], "schedules": [sched("r1", 99)]});
        call(&s, &["turn", &again.to_string()]);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["open-effects"]).1).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(call(&s, &["due"]).1, "[]", "the fired row is not re-armed by a second insert");
    }

    #[test]
    fn a_fired_mark_for_a_row_not_yet_inserted_does_not_swallow_its_later_insert() {
        let s = store();
        call(&s, &["turn", &json!({"fired": ["r9"]}).to_string()]);
        call(&s, &["turn", &json!({"schedules": [sched("r9", 1)]}).to_string()]);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["due"]).1).unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_stream_keeps_its_first_poison() {
        let s = store();
        let p = |m: &str| json!({"stream": "st", "event": "e", "handler": "h", "message": m}).to_string();
        assert_eq!(call(&s, &["poisoned", "st"]).1, "null");
        call(&s, &["poison", &p("first")]);
        call(&s, &["poison", &p("second")]);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["poisoned", "st"]).1).unwrap()["message"], "first");
    }

    #[test]
    fn a_turn_over_the_ops_a_txn_carries_still_commits_whole() {
        let s = store();
        let rows: Vec<Value> = (0..300).map(|n| json!({"task": "t", "skey": format!("k{n}"), "value": "v", "at": n})).collect();
        assert_eq!(call(&s, &["turn", &json!({"state": rows}).to_string()]).0, 0);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["state"]).1).unwrap().as_array().unwrap().len(), 300);
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
    fn a_long_text_is_clipped_to_the_limit_and_an_ellipsis() {
        assert_eq!(clip(&"x".repeat(10_000)).chars().count(), CLIP + 1);
        assert_eq!(clip("short"), "short");
    }
}
