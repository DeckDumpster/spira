//! The runtime's store verbs: `journal <op> ...` over the control store (`control::journal`). The
//! runtime reaches the store only through this socket, so an idle runtime launches no process.
//! A turn is upserts keyed by what they change, never a delete of a row; the runtime reads the
//! store once, at boot.

use serde_json::{json, Value};

use crate::control::journal::{SweepCursor, EffectRow, PoisonRow, ScheduleRow, StateRow, Turn};

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
    if field(row, "id")?.is_empty() {
        return Err(format!("journal: schedule refused: row_id is empty; {}", describe(row)));
    }
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

pub(crate) fn run<B: state_store::Backend>(args: &[String], store: &crate::control::store::Store<B>) -> Result<String, String> {
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
        "sweep" => {
            let refused = || "journal: sweep takes only --dry-run and --from <cursor>".to_string();
            let (mut dry_run, mut from) = (false, None);
            let mut rest = args[1..].iter();
            while let Some(flag) = rest.next() {
                match flag.as_str() {
                    "--dry-run" if !dry_run => dry_run = true,
                    "--from" if from.is_none() => from = Some(rest.next().ok_or_else(refused)?.parse::<SweepCursor>()?),
                    _ => return Err(refused()),
                }
            }
            let (s, next) = store.journal_sweep_step(from.as_ref(), dry_run, "journal-sweep").map_err(cannot)?;
            Ok(json!({ "dry_run": dry_run, "idless_schedules": s.idless_schedules, "legacy_deleted": s.legacy_deleted, "retained": s.retained, "indexed_schedules": s.indexed_schedules, "indexed_effects": s.indexed_effects, "next": next.map(|c| c.to_string()) }).to_string())
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
        _ => Err(format!("journal: unknown op {op:?} (turn|state|open-effects|due|poison|poisoned|sweep)")),
    }
}

pub fn dispatch(args: &[String]) -> (i32, String) {
    let store = match crate::control::served::store() {
        Ok(s) => s,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    answer(run(args, &*store))
}

pub(crate) fn answer(r: Result<String, String>) -> (i32, String) {
    match r {
        Ok(out) => (0, out),
        Err(e) if e.starts_with("journal:") => (REFUSED, e),
        Err(e) => (CANNOT_TELL, format!("cannot tell: {e}")),
    }
}

/// Sends `journal sweep` steps through `step` until the cursor runs out and sums their counts.
pub fn sweep_until_done(step: &mut dyn FnMut(&[String]) -> (i32, String), dry_run: bool) -> (i32, String) {
    let mut totals = [0u64; 5];
    let mut cursor: Option<String> = None;
    let mut steps = 0;
    loop {
        let mut argv: Vec<String> = vec!["journal".into(), "sweep".into()];
        if dry_run {
            argv.push("--dry-run".into());
        }
        if let Some(c) = &cursor {
            argv.extend(["--from".into(), c.clone()]);
        }
        let (code, out) = step(&argv);
        let after = || format!("{out}\nafter step {steps}, cursor {}", cursor.as_deref().unwrap_or("(start)"));
        if code != 0 {
            return (code, after());
        }
        let Ok(v) = serde_json::from_str::<Value>(&out) else { return (CANNOT_TELL, format!("cannot tell: sweep step answered {out:?}")) };
        steps += 1;
        for (t, k) in totals.iter_mut().zip(["idless_schedules", "indexed_schedules", "indexed_effects", "legacy_deleted", "retained"]) {
            *t += v[k].as_u64().unwrap_or(0);
        }
        match v["next"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    (0, json!({ "dry_run": dry_run, "idless_schedules": totals[0], "indexed_schedules": totals[1], "indexed_effects": totals[2], "legacy_deleted": totals[3], "retained": totals[4], "steps": steps }).to_string())
}

#[cfg(test)]
pub(crate) mod counting {
    use state_store::MemBackend;
    #[derive(Clone)]
    pub(crate) struct Counting(pub MemBackend, pub std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl Counting {
        pub(crate) fn spent(&self, n: usize) {
            self.1.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl state_store::Backend for Counting {
        fn head(&self) -> state_store::Result<u64> {
            self.0.head()
        }
        fn get(&self, key: &str, at: u64) -> state_store::Result<Option<state_store::Version>> {
            self.spent(1);
            self.0.get(key, at)
        }
        fn get_many(&self, keys: &[&str], at: u64) -> state_store::Result<Vec<Option<state_store::Version>>> {
            self.spent(keys.len());
            self.0.get_many(keys, at)
        }
        fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
            let rows = self.0.range(prefix, after, limit, at)?;
            self.spent(rows.len());
            Ok(rows)
        }
        fn owned_by(&self, owner: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
            self.0.owned_by(owner, after, limit, at)
        }
        fn due(&self, now: u64, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
            self.0.due(now, limit, at)
        }
        fn commit(&self, expect: u64, batch: &[state_store::Write]) -> state_store::Result<u64> {
            self.0.commit(expect, batch)
        }
        fn log(&self, from: u64, max_mains: usize) -> state_store::Result<Vec<state_store::LogEntry>> {
            self.0.log(from, max_mains)
        }
        fn compact(&self, rev: u64) -> state_store::Result<()> {
            self.0.compact(rev)
        }
    }

}

#[cfg(test)]
mod tests {
    use super::counting::Counting;
    use super::*;
    use runtime::ports::{Clock, VirtualClock};
    use state_store::{Config, Contract, MemBackend, Ports};

    fn store() -> crate::control::store::Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    fn call<B: state_store::Backend>(s: &crate::control::store::Store<B>, argv: &[&str]) -> (i32, String) {
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

    fn shared_store() -> (crate::control::store::Store<MemBackend>, Contract<MemBackend>) {
        let backend = MemBackend::new();
        let ports = || Ports::virtual_clock(&VirtualClock::new(1_000));
        let raw = Contract::open(backend.clone(), ports(), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let c = Contract::open(backend, ports(), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        (crate::control::store::Store::new(c, 1_000), raw)
    }

    fn put_raw(raw: &Contract<MemBackend>, key: &str, v: Value) {
        let t = state_store::Txn::new().put(key, state_store::Object::new(v));
        assert!(raw.txn(&t, &state_store::Request::new(&format!("raw-{key}"), "test", "raw")).unwrap().succeeded);
    }

    fn parsed<B: state_store::Backend>(s: &crate::control::store::Store<B>, argv: &[&str]) -> Value {
        let (code, out) = call(s, argv);
        assert_eq!(code, 0, "{out}");
        serde_json::from_str(&out).unwrap()
    }

    fn clocked_store() -> (crate::control::store::Store<MemBackend>, Contract<MemBackend>, VirtualClock) {
        let (backend, clock) = (MemBackend::new(), VirtualClock::new(1_000));
        let open = || Contract::open(backend.clone(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        (crate::control::store::Store::new(open(), 1_000), open(), clock)
    }

    fn present(raw: &Contract<MemBackend>, id: &str) -> (bool, bool) {
        let k = |t: &str, parts: &[&str]| raw.get(&crate::control::keys::runtime(t, parts)).unwrap().is_some();
        (k("effect", &["t", &format!("k{id}")]), k("resolved", &[id]))
    }

    fn effect_n(id: &str) -> Value {
        let mut e = effect();
        e["request_id"] = json!(id);
        e["ekey"] = json!(format!("k{id}"));
        e
    }

    fn retain_ms() -> u64 {
        crate::control::journal::JOURNAL_RETAIN_SECS * 1_000
    }

    #[test]
    fn a_resolved_effect_and_its_marker_expire_after_the_retention_and_an_open_one_never_does() {
        use state_store::gc::Controller;
        let (s, raw, clock) = clocked_store();
        let gc = Controller::new(&raw);
        call(&s, &["turn", &json!({"effects": [effect_n("a"), effect_n("b")]}).to_string()]);
        call(&s, &["turn", &json!({"resolved": ["a"]}).to_string()]);
        assert_eq!(present(&raw, "a"), (true, true));
        clock.advance(retain_ms() - 1);
        gc.tick(clock.now_ms()).unwrap();
        assert_eq!(present(&raw, "a"), (true, true));
        clock.advance(1);
        gc.tick(clock.now_ms()).unwrap();
        assert_eq!(present(&raw, "a"), (false, false));
        clock.advance(100 * retain_ms());
        gc.tick(clock.now_ms()).unwrap();
        assert_eq!(present(&raw, "b"), (true, false));
        assert_eq!(parsed(&s, &["open-effects"]).as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_sweep_gives_each_resolved_effect_without_a_lifetime_one_and_a_second_sweep_gives_none() {
        use state_store::gc::Controller;
        let (s, raw, clock) = clocked_store();
        for id in ["a", "b"] {
            for (k, v) in [(crate::control::keys::runtime("effect", &["t", &format!("k{id}")]), effect_n(id)), (crate::control::keys::runtime("resolved", &[id]), json!({}))] {
                put_raw(&raw, &k, v);
            }
        }
        call(&s, &["turn", &json!({"effects": [effect_n("c")]}).to_string()]);
        assert_eq!(swept(&s, false)["retained"], json!(2));
        assert_eq!(swept(&s, false)["retained"], json!(0));
        clock.advance(retain_ms());
        Controller::new(&raw).tick(clock.now_ms()).unwrap();
        assert_eq!((present(&raw, "a"), present(&raw, "b"), present(&raw, "c")), ((false, false), (false, false), (true, false)));
    }

    #[test]
    fn a_schedule_row_stored_without_an_id_is_not_due() {
        let (s, raw) = shared_store();
        put_raw(&raw, &crate::control::keys::runtime("schedule", &["carried"]), json!({"at": 0, "stream": "s", "tag": "", "body": "", "correlation": ""}));
        call(&s, &["turn", &json!({"schedules": [sched("r1", 4)]}).to_string()]);
        let due = parsed(&s, &["due"]);
        assert_eq!(due.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>(), ["r1"]);
        swept(&s, false);
        assert_eq!(parsed(&s, &["due"]).as_array().unwrap().len(), 1, "still so once the store is indexed");
    }

    #[test]
    fn a_schedule_row_with_an_empty_id_is_refused_by_a_turn_and_by_a_carry_naming_the_row() {
        let (code, msg) = call(&store(), &["turn", &json!({"schedules": [sched("", 4)]}).to_string()]);
        assert!(code == REFUSED && msg.contains("row_id is empty") && msg.contains("kind=tick") && msg.contains("stream=s"), "{msg}");
        let row = crate::control::journal::ScheduleRow { id: String::new(), at: 9, stream: "s".into(), tag: "tick".into(), body: String::new(), cause: None, correlation: String::new() };
        let carried = crate::control::journal::Carried { schedules: vec![(row, false)], ..Default::default() };
        let err = store().journal_carry(&carried, "test").unwrap_err().to_string();
        assert!(err.contains("empty id") && err.contains("stream=s") && err.contains("kind=tick"), "{err}");
    }

    #[test]
    fn the_boot_reads_ask_the_backend_for_the_rows_they_return_not_for_every_row_ever_written() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = Counting(MemBackend::new(), counter.clone());
        let c = Contract::open(backend.clone(), Ports::virtual_clock(&VirtualClock::new(1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = crate::control::store::Store::new(c, 1_000);
        let n = 1_000;
        for chunk in 0..n / 100 {
            let effects: Vec<Value> = (0..100).map(|i| {
                let mut e = effect();
                e["ekey"] = json!(format!("k{chunk}-{i}"));
                e["request_id"] = json!(format!("done{chunk}-{i}"));
                e
            }).collect();
            let scheds: Vec<Value> = (0..100).map(|i| sched(&format!("old{chunk}-{i}"), 1)).collect();
            let ids: Vec<String> = (0..100).map(|i| format!("{chunk}-{i}")).collect();
            let resolved: Vec<String> = ids.iter().map(|i| format!("done{i}")).collect();
            let fired: Vec<String> = ids.iter().map(|i| format!("old{i}")).collect();
            call(&s, &["turn", &json!({"effects": effects, "schedules": scheds}).to_string()]);
            call(&s, &["turn", &json!({"resolved": resolved, "fired": fired}).to_string()]);
        }
        call(&s, &["turn", &json!({"effects": [effect()], "schedules": [sched("live", 4)]}).to_string()]);
        swept(&s, false);
        for argv in [&["open-effects"][..], &["due"][..]] {
            counter.store(0, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(parsed(&s, argv).as_array().unwrap().len(), 1);
            let asked = counter.load(std::sync::atomic::Ordering::SeqCst);
            assert!(asked <= 8, "{argv:?} asked the backend for {asked} keys");
        }
    }

    #[test]
    fn an_index_key_left_for_a_row_that_closed_meanwhile_does_not_reopen_it() {
        let (s, raw) = shared_store();
        call(&s, &["turn", &json!({"effects": [effect()], "schedules": [sched("r1", 4)]}).to_string()]);
        swept(&s, false);
        call(&s, &["turn", &json!({"resolved": ["r1"], "fired": ["r1"]}).to_string()]);
        put_raw(&raw, &crate::control::keys::runtime("pending", &["r1"]), json!({"id": "r1"}));
        put_raw(&raw, &crate::control::keys::runtime("open-effect", &["r1"]), json!({"task": "t", "ekey": "k'1/x"}));
        assert_eq!((call(&s, &["due"]).1, call(&s, &["open-effects"]).1), ("[]".to_string(), "[]".to_string()));
    }

    #[test]
    fn the_sweep_dry_run_counts_what_it_would_remove_and_the_sweep_removes_exactly_that() {
        let (s, raw) = shared_store();
        let mut resolved_effect = effect();
        resolved_effect["request_id"] = json!("gone");
        resolved_effect["ekey"] = json!("other");
        call(&s, &["turn", &json!({"effects": [effect(), resolved_effect], "schedules": [sched("r1", 4), sched("r2", 5)]}).to_string()]);
        call(&s, &["turn", &json!({"resolved": ["gone", "ghost"], "fired": ["r2"]}).to_string()]);
        for n in 0..3 {
            put_raw(&raw, &crate::control::keys::runtime("schedule", &[&format!("carried{n}")]), json!({"at": 0, "stream": "s"}));
        }
        let (open_before, due_before) = (parsed(&s, &["open-effects"]), parsed(&s, &["due"]));
        let key_count = |table: &str| raw.list(&[crate::control::keys::runtime_table(table).as_str()], &state_store::Selector::Everything).unwrap().0.len();
        let before = (key_count("schedule"), key_count("resolved"));
        let dry = swept(&s, true);
        assert_eq!(dry["idless_schedules"], json!(3), "{dry}");
        assert_eq!((key_count("schedule"), key_count("resolved")), before, "a dry run removes nothing");
        assert_eq!(key_count("pending"), 1, "a dry run writes no index");
        assert_eq!(key_count("journal-meta"), 0, "a dry run marks nothing indexed");
        let done = swept(&s, false);
        assert_eq!((done["idless_schedules"].clone(), done["indexed_schedules"].clone(), done["indexed_effects"].clone(), done["next"].clone()), (json!(3), json!(1), json!(1), json!(null)), "{done}");
        assert_eq!((key_count("schedule"), key_count("resolved")), (before.0 - 3, before.1), "orphan resolved markers stay");
        assert_eq!((parsed(&s, &["open-effects"]), parsed(&s, &["due"])), (open_before, due_before), "every legitimate row reads as it did");
    }

    fn swept<B: state_store::Backend>(s: &crate::control::store::Store<B>, dry_run: bool) -> Value {
        let (code, out) = sweep_until_done(&mut in_process(s), dry_run);
        assert_eq!(code, 0, "{out}");
        let mut v: Value = serde_json::from_str(&out).unwrap();
        v["next"] = Value::Null;
        v
    }

    fn seeded(idless: usize) -> (crate::control::store::Store<MemBackend>, Contract<MemBackend>) {
        let (s, raw) = shared_store();
        call(&s, &["turn", &json!({"effects": [effect()], "schedules": [sched("live", 4)]}).to_string()]);
        for n in 0..idless {
            put_raw(&raw, &crate::control::keys::runtime("schedule", &[&format!("carried{n:05}")]), json!({"at": 0, "stream": "s"}));
        }
        (s, raw)
    }

    fn in_process<B: state_store::Backend>(s: &crate::control::store::Store<B>) -> impl FnMut(&[String]) -> (i32, String) + '_ {
        move |argv| answer(run(&argv[1..], s))
    }

    #[test]
    fn the_sweep_loop_sums_the_counts_of_every_step_until_the_cursor_runs_out() {
        let (s, _raw) = seeded(2_500);
        let (code, out) = sweep_until_done(&mut in_process(&s), false);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(code, 0, "{out}");
        assert_eq!((v["idless_schedules"].clone(), v["indexed_schedules"].clone(), v["steps"].clone()), (json!(2_500), json!(1), json!(7)), "{v}");
        assert_eq!(swept(&s, false)["steps"], json!(5), "a swept store ends in the schedule step, one step per legacy table and the effect step");
    }

    #[test]
    fn a_sweep_step_that_fails_stops_the_loop_naming_its_code_and_cursor_and_a_rerun_reaches_the_same_store() {
        let (s, raw) = seeded(1_500);
        let key_count = |table: &str| raw.list(&[crate::control::keys::runtime_table(table).as_str()], &state_store::Selector::Everything).unwrap().0.len();
        let mut calls = 0;
        let mut inner = in_process(&s);
        let (code, out) = sweep_until_done(&mut |argv| {
            calls += 1;
            if calls == 2 { (CANNOT_TELL, "cannot tell: deadline".into()) } else { inner(argv) }
        }, false);
        assert_eq!(code, CANNOT_TELL);
        assert!(out.contains("cannot tell: deadline") && out.contains("after step 1, cursor schedule:"), "{out}");
        assert_eq!(key_count("journal-meta"), 0, "not indexed while a step is missing");
        let (code, out) = sweep_until_done(&mut in_process(&s), false);
        assert_eq!(code, 0, "{out}");
        let after_rerun = (key_count("schedule"), key_count("pending"), key_count("open-effect"), key_count("journal-meta"));
        assert_eq!(after_rerun, (1, 1, 1, 1));
        sweep_until_done(&mut in_process(&s), false);
        assert_eq!(after_rerun, (key_count("schedule"), key_count("pending"), key_count("open-effect"), key_count("journal-meta")));
    }

    fn legacy_seeded() -> (crate::control::store::Store<MemBackend>, Contract<MemBackend>) {
        let (s, raw) = seeded(0);
        for (table, tail) in [("event", "a"), ("applied", "asks/boot"), ("cursor", "1")] {
            put_raw(&raw, &crate::control::keys::runtime(table, &[tail]), json!({"main": 255023}));
        }
        put_raw(&raw, &crate::control::keys::runtime("schedule", &["carried"]), json!({"at": 0, "stream": "s"}));
        put_raw(&raw, &crate::control::keys::runtime("state", &["keep"]), json!({"v": 1}));
        (s, raw)
    }

    fn all_keys(raw: &Contract<MemBackend>) -> Vec<String> {
        raw.list(&["runtime/"], &state_store::Selector::Everything).unwrap().0.into_iter().map(|i| i.key).collect()
    }

    #[test]
    fn the_sweep_removes_the_legacy_import_rows_and_nothing_a_reader_uses() {
        let (s, raw) = legacy_seeded();
        let before = all_keys(&raw);
        let dry = swept(&s, true);
        assert_eq!((dry["legacy_deleted"].clone(), dry["idless_schedules"].clone()), (json!(3), json!(1)), "{dry}");
        assert_eq!(all_keys(&raw), before, "a dry run writes nothing");
        let done = swept(&s, false);
        assert_eq!(done["legacy_deleted"], json!(3), "{done}");
        let gone = |k: &String| ["runtime/event/", "runtime/applied/", "runtime/cursor/", "runtime/schedule/carried"].iter().any(|p| k.starts_with(p));
        let removed: Vec<_> = before.iter().filter(|k| !all_keys(&raw).contains(k)).cloned().collect();
        assert_eq!(removed.len(), 4, "{removed:?}");
        assert!(removed.iter().all(gone), "{removed:?}");
        let after = all_keys(&raw);
        let kept: Vec<_> = before.iter().filter(|k| !gone(k)).cloned().collect();
        assert!(kept.iter().all(|k| after.contains(k)), "a key a reader uses was removed");
        assert!(kept.iter().any(|k| k.starts_with("runtime/effect/")) && kept.iter().any(|k| k.starts_with("runtime/open-effect/")) && kept.contains(&"runtime/state/keep".to_string()));
        assert!(after.contains(&"runtime/journal-meta/indexed".to_string()));
    }

    #[test]
    fn a_sweep_resumed_inside_the_legacy_phase_finishes_the_rest_and_deletes_nothing_twice() {
        let (s, raw) = legacy_seeded();
        let (out, next) = s.journal_sweep_step(Some(&"legacy:runtime/event/a".parse().unwrap()), false, "t").unwrap();
        assert_eq!((out.legacy_deleted, next.as_ref().map(|c| c.to_string())), (0, Some("legacy:runtime/applied/".into())));
        let mut cursor = next;
        let mut total = 0;
        while let Some(c) = cursor {
            let (o, n) = s.journal_sweep_step(Some(&c), false, "t").unwrap();
            total += o.legacy_deleted;
            cursor = n;
        }
        assert_eq!(total, 2, "applied and cursor remain; event was left for the earlier run");
        let after = all_keys(&raw);
        assert!(after.iter().any(|k| k.starts_with("runtime/event/")), "the resume starts after the cursor");
        assert!(!after.iter().any(|k| k.starts_with("runtime/applied/") || k.starts_with("runtime/cursor/")));
    }

    #[test]
    fn a_legacy_step_reads_at_most_one_page() {
        let (s, raw) = seeded(0);
        for n in 0..(crate::control::journal::SWEEP_STEP + 5) {
            put_raw(&raw, &crate::control::keys::runtime("event", &[&format!("e{n:05}")]), json!({}));
        }
        let (out, next) = s.journal_sweep_step(Some(&"legacy:".parse().unwrap()), false, "t").unwrap();
        assert_eq!(out.legacy_deleted, crate::control::journal::SWEEP_STEP);
        assert!(next.unwrap().to_string().starts_with("legacy:runtime/event/"), "the cursor stays inside event");
        assert_eq!(all_keys(&raw).iter().filter(|k| k.starts_with("runtime/event/")).count(), 5);
    }

    #[test]
    fn a_sweep_refuses_an_argument_it_does_not_know() {
        let s = store();
        for argv in [&["sweep", "--now"][..], &["sweep", "--from"][..], &["sweep", "--from", "nowhere"][..], &["sweep", "--dry-run", "--dry-run"][..]] {
            assert_eq!(call(&s, argv).0, REFUSED, "{argv:?}");
        }
        assert_eq!(call(&s, &["sweep", "--from", "legacy:zzz"]).0, REFUSED);
        assert_eq!(call(&s, &["sweep", "--from", "effect:", "--dry-run"]).0, 0);
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
