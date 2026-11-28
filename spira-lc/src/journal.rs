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
    for r in list(turn, "cleared")? {
        let pair = r.as_array().filter(|a| a.len() == 2).and_then(|a| Some((a[0].as_str()?, a[1].as_str()?)));
        let (task, skey) = pair.ok_or_else(|| format!("journal: turn refused: a cleared key is not a [task, skey] pair: {}", clip(&r.to_string())))?;
        check_text("turn", "task", task, TASK_MAX)?;
        check_text("turn", "skey", skey, KEY_MAX)?;
        out.cleared.push((task.into(), skey.into()));
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

/// The tasks whose rows a `journal turn` request carries, comma-joined, for its span.
pub(crate) fn turn_tasks(argv: &[String]) -> Option<String> {
    let [journal, op, body, ..] = argv else { return None };
    if journal != "journal" || op != "turn" {
        return None;
    }
    let turn: runtime::journal::Turn = serde_json::from_str(body).ok()?;
    let names = turn.task_names();
    (!names.is_empty()).then(|| names.into_iter().collect::<Vec<_>>().join(","))
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
            let refused = || "journal: state takes only --after <cursor>".to_string();
            let after = match &args[1..] {
                [] => None,
                [flag, cursor] if flag == "--after" => Some(cursor.as_str()),
                _ => return Err(refused()),
            };
            let (rows, next) = store.journal_state_page(after, crate::control::journal::STATE_PAGE).map_err(cannot)?;
            let rows: Vec<Value> = rows.iter().map(|r| json!({ "task": r.task, "skey": r.skey, "value": r.value, "at": r.at })).collect();
            Ok(json!({ "rows": rows, "next": next }).to_string())
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
        "done" | "has-done" => {
            let id = text_arg(a(1), "request_id")?;
            check_text(op, "request_id", id, ID_MAX)?;
            let answer = if op == "done" { store.journal_done(id, "runtime") } else { store.journal_has_done(id) };
            Ok(answer.map_err(cannot)?.to_string())
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
            Ok(json!({ "dry_run": dry_run, "idless_schedules": s.idless_schedules, "legacy_deleted": s.legacy_deleted, "deleted_effects": s.deleted_effects, "deleted_markers": s.deleted_markers, "indexed_schedules": s.indexed_schedules, "indexed_effects": s.indexed_effects, "next": next.map(|c| c.to_string()) }).to_string())
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
            let of = |r: PoisonRow| json!({ "stream": r.stream, "event": r.event, "handler": r.handler, "message": r.message });
            if stream == "--all" {
                return Ok(Value::Array(store.journal_poisons().map_err(cannot)?.into_iter().map(of).collect()).to_string());
            }
            let row = store.journal_poisoned(stream).map_err(cannot)?;
            Ok(row.map_or("null".into(), |r| of(r).to_string()))
        }
        _ => Err(format!("journal: unknown op {op:?} (turn|state|open-effects|due|done|has-done|poison|poisoned [--all]|sweep)")),
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

/// Reads `journal sweep [--dry-run] [--from <cursor>]`; `None` when `args` is not a sweep.
pub type SweepArgs = Result<(bool, Option<String>), (i32, String)>;

pub fn parse_sweep_args(args: &[String]) -> Option<SweepArgs> {
    if !matches!(args, [j, s, ..] if j == "journal" && s == "sweep") {
        return None;
    }
    let refused = || (REFUSED, "journal: sweep takes only --dry-run and --from <cursor>".to_string());
    let (mut dry_run, mut from) = (false, None);
    let mut rest = args[2..].iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--dry-run" if !dry_run => dry_run = true,
            "--from" if from.is_none() => {
                let Some(c) = rest.next() else { return Some(Err(refused())) };
                if let Err(e) = c.parse::<SweepCursor>() {
                    return Some(Err((REFUSED, e)));
                }
                from = Some(c.clone());
            }
            _ => return Some(Err(refused())),
        }
    }
    Some(Ok((dry_run, from)))
}

/// Sends `journal sweep` steps through `step`, from `from`, until the cursor runs out and sums their counts.
pub fn sweep_until_done(step: &mut dyn FnMut(&[String]) -> (i32, String), dry_run: bool, from: Option<String>) -> (i32, String) {
    const COUNTS: [&str; 6] = ["idless_schedules", "indexed_schedules", "indexed_effects", "legacy_deleted", "deleted_effects", "deleted_markers"];
    let mut totals = [0u64; COUNTS.len()];
    let mut cursor = from;
    let mut steps = 0;
    let summary = |totals: &[u64; COUNTS.len()], steps: i32, complete: bool| {
        let mut v = json!({ "dry_run": dry_run, "steps": steps, "complete": complete });
        for (k, t) in COUNTS.iter().zip(totals) {
            v[*k] = json!(t);
        }
        v
    };
    loop {
        let mut argv: Vec<String> = vec!["journal".into(), "sweep".into()];
        if dry_run {
            argv.push("--dry-run".into());
        }
        if let Some(c) = &cursor {
            argv.extend(["--from".into(), c.clone()]);
        }
        let (code, out) = step(&argv);
        if code != 0 {
            let mut head = summary(&totals, steps, false);
            head["resume"] = json!(cursor);
            let command = format!("spira-lc journal sweep{}{}", if dry_run { " --dry-run" } else { "" }, cursor.as_ref().map_or(String::new(), |c| format!(" --from {c}")));
            return (code, format!("{head}\n{out}\nresume: {command}"));
        }
        let Ok(v) = serde_json::from_str::<Value>(&out) else { return (CANNOT_TELL, format!("cannot tell: sweep step answered {out:?}")) };
        steps += 1;
        for (t, k) in totals.iter_mut().zip(COUNTS) {
            *t += v[k].as_u64().unwrap_or(0);
        }
        match v["next"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    (0, summary(&totals, steps, true).to_string())
}

#[cfg(test)]
pub(crate) mod counting {
    use state_store::MemBackend;
    #[derive(Clone)]
    pub(crate) struct Counting(pub MemBackend, pub std::sync::Arc<std::sync::atomic::AtomicUsize>, pub std::sync::Arc<std::sync::Mutex<Vec<(&'static str, String)>>>);

    impl Counting {
        pub(crate) fn new(inner: MemBackend, keys: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
            Counting(inner, keys, Default::default())
        }
        pub(crate) fn calls(&self, kind: &str) -> usize {
            self.2.lock().unwrap().iter().filter(|k| k.0 == kind).count()
        }
        pub(crate) fn calls_on(&self, kind: &str, key_part: &str) -> usize {
            self.2.lock().unwrap().iter().filter(|k| k.0 == kind && k.1.contains(key_part)).count()
        }
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
            self.2.lock().unwrap().push(("get", key.to_string()));
            self.0.get(key, at)
        }
        fn get_many(&self, keys: &[&str], at: u64) -> state_store::Result<Vec<Option<state_store::Version>>> {
            self.spent(keys.len());
            self.2.lock().unwrap().push(("get_many", keys.join(" ")));
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

    /// A backend charging a virtual clock what production measured: 1 ms per 16 KiB read and per `get`.
    #[derive(Clone)]
    pub(crate) struct Costed {
        pub inner: MemBackend,
        pub clock: runtime::ports::VirtualClock,
        pub armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Costed {
        fn charge(&self, bytes: usize, gets: u64) {
            self.clock.advance((bytes / 16_384) as u64 + gets);
        }
    }

    impl state_store::Backend for Costed {
        fn head(&self) -> state_store::Result<u64> {
            self.inner.head()
        }
        fn get(&self, key: &str, at: u64) -> state_store::Result<Option<state_store::Version>> {
            let v = self.inner.get(key, at)?;
            self.charge(v.as_ref().map_or(0, |v| v.value.len()), 1);
            Ok(v)
        }
        fn get_many(&self, keys: &[&str], at: u64) -> state_store::Result<Vec<Option<state_store::Version>>> {
            let v = self.inner.get_many(keys, at)?;
            self.charge(v.iter().flatten().map(|v| v.value.len()).sum(), 0);
            Ok(v)
        }
        fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, state_store::Version)>> {
            let rows = self.inner.range(prefix, after, limit, at)?;
            self.charge(rows.iter().map(|r| r.1.value.len()).sum(), 0);
            Ok(rows)
        }
        fn owned_by(&self, owner: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
            self.inner.owned_by(owner, after, limit, at)
        }
        fn due(&self, now: u64, limit: usize, at: u64) -> state_store::Result<Vec<String>> {
            self.inner.due(now, limit, at)
        }
        fn commit(&self, expect: u64, batch: &[state_store::Write]) -> state_store::Result<u64> {
            let first = self.armed.swap(false, std::sync::atomic::Ordering::SeqCst);
            let rev = self.inner.commit(expect, batch)?;
            if first {
                std::mem::forget(state_store::phases::deadline_scoped(std::time::Instant::now()));
            }
            Ok(rev)
        }
        fn log(&self, from: u64, max_mains: usize) -> state_store::Result<Vec<state_store::LogEntry>> {
            self.inner.log(from, max_mains)
        }
        fn compact(&self, rev: u64) -> state_store::Result<()> {
            self.inner.compact(rev)
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

    #[test_id(tlfebv3)]
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
            json!({"cleared": [["t"]]}),
            json!({"cleared": [["t", "k".repeat(256)]]}),
            json!({"state": [{"task": "t", "skey": "carry", "value": "v", "at": 1}], "fired": [""]}),
        ] {
            assert!(turn_of(&bad).unwrap_err().starts_with("journal:"), "{bad}");
        }
        assert!(turn_of(&wide("ekey", 255)).is_ok(), "the limit itself is accepted");
        let sent = serde_json::to_value(runtime::journal::Turn { cleared: vec![("t".into(), "k".into())], ..Default::default() }).unwrap();
        assert_eq!(turn_of(&sent).unwrap().cleared, vec![("t".to_string(), "k".to_string())], "a cleared key the runtime sends is read as sent");
        assert_eq!(turn_of(&json!({})).unwrap(), Turn::default(), "an empty turn writes nothing");
    }

    #[test_id(tpqdds6)]
    #[test]
    fn a_turns_span_names_the_task_of_each_row_it_carries_once() {
        let argv = |turn: &Value| vec!["journal".to_string(), "turn".to_string(), turn.to_string()];
        let state = json!({"state": [{"task": "claim-count", "skey": "carry", "value": "v", "at": 1}]});
        assert_eq!(turn_tasks(&argv(&state)).as_deref(), Some("claim-count"));
        let sent = runtime::journal::Turn { cleared: vec![("c".into(), "k".into())], fired: vec!["r1".into()], tasks: ["b".to_string()].into(), ..Default::default() };
        let mut body = serde_json::to_value(sent).unwrap();
        body["effects"] = json!([effect(), effect()]);
        body["state"] = json!([{"task": "c", "skey": "carry", "value": "v", "at": 1}]);
        assert_eq!(turn_tasks(&argv(&body)).as_deref(), Some("b,c,t"), "state, cleared, effect and the rows that name no task of their own");
        assert!(turn_of(&body).is_ok(), "the body the runtime sends is accepted");
        for other in [vec!["journal".to_string(), "poisoned".into(), "--all".into()], argv(&json!({})), vec!["journal".to_string(), "turn".into(), "not json".into()], vec!["show".to_string()]] {
            assert_eq!(turn_tasks(&other), None, "{other:?}");
        }
    }

    #[test_id(tvp6bm9)]
    #[test]
    fn poisoned_all_answers_every_poisoned_stream_in_one_call() {
        let s = store();
        assert_eq!(call(&s, &["poisoned", "--all"]), (0, "[]".into()));
        for stream in ["a", "b"] {
            let p = json!({"stream": stream, "event": "e", "handler": "h", "message": "m"});
            assert_eq!(call(&s, &["poison", &p.to_string()]).0, 0);
        }
        let all = parsed(&s, &["poisoned", "--all"]);
        assert_eq!(all, json!([parsed(&s, &["poisoned", "a"]), parsed(&s, &["poisoned", "b"])]));
        assert!(call(&s, &["no-such-op"]).1.contains("poisoned [--all]"));
    }

    #[test_id(thpzk1e)]
    #[test]
    fn a_nul_in_a_payload_is_refused_before_any_write() {
        assert!(parse(Some(&"{\"a\":\"\0\"}".to_string()), "event").unwrap_err().contains("NUL"));
        assert!(text_arg(Some(&String::new()), "handler").is_err());
    }

    #[test_id(tztcvj9)]
    #[test]
    fn an_over_length_schedule_id_is_refused_naming_the_row_its_fields_and_their_lengths() {
        let r = schedule_row(&sched(&"r".repeat(260), 1)).unwrap_err();
        assert!(r.starts_with("journal: schedule refused: row_id is 260 chars (max 255);") && r.contains("kind=tick"), "{r}");
        assert!(schedule_row(&json!({"id": "r"})).is_err());
    }

    #[test_id(tehy5f9)]
    #[test]
    fn what_a_turn_wrote_reads_back_and_what_it_closed_is_no_longer_open() {
        let s = store();
        let turn = json!({"state": [{"task": "t", "skey": "carry", "value": "v'", "at": 2}], "effects": [effect()], "schedules": [sched("r2", 5), sched("r1", 4)]});
        assert_eq!(call(&s, &["turn", &turn.to_string()]).0, 0);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["state"]).1).unwrap(), json!({"rows": [{"task": "t", "skey": "carry", "value": "v'", "at": 2}], "next": null}));
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

    #[test_id(twrdddy)]
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

    #[test_id(tqz9sq4)]
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

    fn put_raw<B: state_store::Backend>(raw: &Contract<B>, key: &str, v: Value) {
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

    const MUCH_LATER_MS: u64 = 400 * 24 * 60 * 60 * 1_000;

    /// The effect keys the journal holds: rows, open index, old markers and the ran ledger.
    fn effect_keys<B: state_store::Backend>(s: &crate::control::store::Store<B>) -> Vec<String> {
        let tables: Vec<String> = ["effect", "open-effect", "resolved", "done"].iter().map(|t| crate::control::keys::runtime_table(t)).collect();
        let refs: Vec<&str> = tables.iter().map(String::as_str).collect();
        s.contract().list(&refs, &state_store::Selector::Everything).unwrap().0.into_iter().map(|i| i.key).collect()
    }

    fn turned<B: state_store::Backend>(s: &crate::control::store::Store<B>, turn: Value) {
        let (code, out) = call(s, &["turn", &turn.to_string()]);
        assert_eq!(code, 0, "{out}");
    }

    fn resolving_a_request_leaves_no_key_for_it<B: state_store::Backend>(s: &crate::control::store::Store<B>) {
        turned(s, json!({"effects": [effect_n("r1")]}));
        call(s, &["done", "r1"]);
        turned(s, json!({"resolved": ["r1"]}));
        assert_eq!(effect_keys(s), Vec::<String>::new());
    }

    /// A schedule insert carries a compare, so it commits the request ahead of the resolve; without
    /// one the two share a txn.
    fn a_request_resolved_in_its_own_turn_leaves_no_key<B: state_store::Backend>(s: &crate::control::store::Store<B>) {
        turned(s, json!({"effects": [effect_n("r1")], "schedules": [sched("timer", 4)], "resolved": ["r1"]}));
        assert_eq!(effect_keys(s), Vec::<String>::new());
        turned(s, json!({"effects": [effect_n("r2")], "resolved": ["r2"]}));
        assert_eq!(effect_keys(s), Vec::<String>::new());
    }

    fn a_re_request_in_the_turn_that_resolves_the_first_keeps_the_open_row<B: state_store::Backend>(s: &crate::control::store::Store<B>) {
        turned(s, json!({"effects": [effect_n("r1")]}));
        let mut again = effect_n("r1");
        again["request_id"] = json!("r2");
        turned(s, json!({"effects": [again], "resolved": ["r1"]}));
        assert_eq!(effect_keys(s), ["runtime/effect/t/kr1", "runtime/open-effect/r2"]);
        assert_eq!(s.journal_open_effects().unwrap().iter().map(|r| r.request_id.as_str()).collect::<Vec<_>>(), ["r2"]);
    }

    fn an_effect_is_marked_run_only_while_it_is_open<B: state_store::Backend>(s: &crate::control::store::Store<B>) {
        let answer = |argv: &[&str]| call(s, argv);
        assert_eq!(answer(&["done", "r1"]), (0, "false".to_string()), "an effect never requested");
        turned(s, json!({"effects": [effect_n("r1")]}));
        assert_eq!(answer(&["has-done", "r1"]), (0, "false".to_string()));
        assert_eq!(answer(&["done", "r1"]), (0, "true".to_string()));
        assert_eq!(answer(&["has-done", "r1"]), (0, "true".to_string()));
        turned(s, json!({"resolved": ["r1"]}));
        assert_eq!(answer(&["done", "r1"]), (0, "false".to_string()), "a runner that finishes after its timeout");
        assert_eq!(answer(&["has-done", "r1"]), (0, "false".to_string()));
        assert_eq!(effect_keys(s), Vec::<String>::new());
    }

    fn on_mem(body: fn(&crate::control::store::Store<MemBackend>)) {
        body(&store());
    }

    fn sqlite_at(path: &std::path::Path) -> crate::control::store::Store<state_store_sqlite::SqliteBackend> {
        let clock = VirtualClock::new(1_000);
        let opts = state_store_sqlite::Options { clock: std::sync::Arc::new(clock.clone()), max_read_ms: testkit::LOADED_HOST_MAX_READ_MS, ..Default::default() };
        let backend = state_store_sqlite::SqliteBackend::open(path, opts).unwrap();
        let c = Contract::open(backend, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    fn on_sqlite(body: fn(&crate::control::store::Store<state_store_sqlite::SqliteBackend>)) {
        let dir = testkit::TempDir::new("spira-lc-journal");
        body(&sqlite_at(&dir.path().join("control.db")));
    }

    macro_rules! on_each_backend {
        ($( #[test_id($mem:ident)] #[test_id($sqlite:ident)] $name:ident ),* $(,)?) => {
            mod mem { $( #[test_id($mem)] #[test] fn $name() { super::super::on_mem(super::super::$name); } )* }
            mod sqlite { $( #[test_id($sqlite)] #[test] fn $name() { super::super::on_sqlite(super::super::$name); } )* }
        };
    }

    on_each_backend!(
        #[test_id(tmqfjzu)] #[test_id(t0tm38h)] resolving_a_request_leaves_no_key_for_it,
        #[test_id(t36vy6r)] #[test_id(t8w12tw)] a_request_resolved_in_its_own_turn_leaves_no_key,
        #[test_id(tn2ijrw)] #[test_id(tktnvxy)] a_re_request_in_the_turn_that_resolves_the_first_keeps_the_open_row,
        #[test_id(t6iw2h6)] #[test_id(t9x2cqr)] an_effect_is_marked_run_only_while_it_is_open,
    );

    #[test_id(te758zu)]
    #[test]
    fn an_open_effect_is_returned_with_its_spec_by_a_store_reopened_on_the_same_file() {
        let dir = testkit::TempDir::new("spira-lc-journal-restart");
        let path = dir.path().join("control.db");
        turned(&sqlite_at(&path), json!({"effects": [effect_n("r1")]}));
        let open = sqlite_at(&path).journal_open_effects().unwrap();
        assert_eq!(open.iter().map(|r| (r.request_id.as_str(), r.spec.clone())).collect::<Vec<_>>(), [("r1", effect()["spec"].clone())]);
    }

    #[test_id(txf0n7a)]
    #[test]
    fn resolving_a_superseded_request_leaves_the_re_requested_effect_open_for_good() {
        use state_store::gc::Controller;
        let (s, raw, clock) = clocked_store();
        let mut again = effect_n("a");
        again["request_id"] = json!("a2");
        call(&s, &["turn", &json!({"effects": [effect_n("a")]}).to_string()]);
        call(&s, &["turn", &json!({"effects": [again]}).to_string()]);
        call(&s, &["turn", &json!({"resolved": ["a"]}).to_string()]);
        clock.advance(MUCH_LATER_MS);
        Controller::new(&raw).tick(clock.now_ms()).unwrap();
        assert!(present(&raw, "a").0);
        assert_eq!(parsed(&s, &["open-effects"]).as_array().unwrap().len(), 1);
    }

    #[test_id(thnz3lz)]
    #[test]
    fn a_carried_resolved_effect_is_not_written_and_a_carried_open_one_is() {
        use crate::control::journal::{Carried, EffectRow};
        let s = store();
        let row = |id: &str| EffectRow { task: "t".into(), ekey: format!("k{id}"), request_id: id.into(), stream: "s".into(), spec: json!({}), requested_at: 1, deadline_at: 6 };
        let carried = Carried { effects: vec![(row("a"), true), (row("b"), false)], ..Default::default() };
        assert_eq!(s.journal_carry(&carried, "test").unwrap(), 1);
        assert_eq!(effect_keys(&s), ["runtime/effect/t/kb", "runtime/open-effect/b"]);
    }

    /// A row as an older release left it: resolved with a marker, and a lifetime or none.
    fn put_old<B: state_store::Backend>(raw: &Contract<B>, id: &str, ttl: bool) {
        let object = |v: Value| match ttl {
            true => state_store::Object::new(v).with_ttl(7 * 24 * 60 * 60),
            false => state_store::Object::new(v),
        };
        let key = crate::control::keys::runtime("effect", &["t", &format!("k{id}")]);
        let mut t = state_store::Txn::new().put(&key, object(effect_n(id)));
        if ttl {
            t = t.put(&crate::control::keys::runtime("resolved", &[id]), object(json!({})));
        }
        assert!(raw.txn(&t, &state_store::Request::new(&format!("old-{id}"), "test", "raw")).unwrap().succeeded);
    }

    #[test_id(tkloc8t)]
    #[test]
    fn a_sweep_of_an_indexed_store_leaves_only_the_open_rows_and_no_marker() {
        let (s, raw) = shared_store();
        put_raw(&raw, &crate::control::keys::runtime("journal-meta", &["indexed"]), json!({}));
        let open: Vec<Value> = (0..20).map(|n| effect_n(&format!("open{n:02}"))).collect();
        turned(&s, json!({ "effects": open }));
        for n in 0..20 {
            put_old(&raw, &format!("retained{n:02}"), true);
            put_old(&raw, &format!("forever{n:02}"), false);
        }
        let dry = swept(&s, true);
        assert_eq!((dry["indexed_effects"].clone(), dry["deleted_effects"].clone(), dry["deleted_markers"].clone()), (json!(0), json!(40), json!(20)), "{dry}");
        let done = swept(&s, false);
        assert_eq!((done["indexed_effects"].clone(), done["deleted_effects"].clone(), done["deleted_markers"].clone()), (json!(0), json!(40), json!(20)), "{done}");
        let want: Vec<String> = (0..20).map(|n| format!("runtime/effect/t/kopen{n:02}")).chain((0..20).map(|n| format!("runtime/open-effect/open{n:02}"))).collect();
        assert_eq!(effect_keys(&s), want);
        assert_eq!(s.journal_open_effects().unwrap().len(), 20);
    }

    #[test_id(tnjeu4v)]
    #[test]
    fn a_sweep_of_an_unindexed_store_deletes_marked_rows_and_indexes_the_unmarked_ones() {
        let (s, raw) = shared_store();
        put_old(&raw, "closed", true);
        put_old(&raw, "live", false);
        let done = swept(&s, false);
        assert_eq!((done["indexed_effects"].clone(), done["deleted_effects"].clone(), done["deleted_markers"].clone()), (json!(1), json!(1), json!(1)), "{done}");
        assert_eq!(effect_keys(&s), ["runtime/effect/t/klive", "runtime/open-effect/live"]);
    }

    #[test_id(txqvfnd)]
    #[test]
    fn an_effect_sweep_larger_than_a_step_deletes_at_most_one_step_of_rows_at_a_time_and_keeps_the_open_ones() {
        use crate::control::journal::SWEEP_STEP;
        let (s, raw) = shared_store();
        put_raw(&raw, &crate::control::keys::runtime("journal-meta", &["indexed"]), json!({}));
        let open: Vec<Value> = (0..20).map(|n| effect_n(&format!("open{n:02}"))).collect();
        turned(&s, json!({ "effects": open }));
        let closed = SWEEP_STEP + 5;
        for n in 0..closed {
            put_old(&raw, &format!("closed{n:05}"), false);
        }
        let (mut cursor, mut per_step) = (Some("effect:".parse().unwrap()), Vec::new());
        while let Some(c) = cursor {
            let (out, next) = s.journal_sweep_step(Some(&c), false, "t").unwrap();
            per_step.push(out.deleted_effects);
            cursor = next;
        }
        assert!(per_step.iter().all(|d| *d <= SWEEP_STEP), "{per_step:?}");
        assert!(per_step.iter().filter(|d| **d > 0).count() > 1, "the rows took more than one step: {per_step:?}");
        assert_eq!(per_step.iter().sum::<usize>(), closed);
        assert_eq!(effect_keys(&s).len(), 40, "20 open rows and their 20 index keys");
    }

    #[test_id(t2vlnyo)]
    #[test]
    fn a_marker_step_deletes_at_most_one_step_of_rows_and_resumes_after_them() {
        use crate::control::journal::SWEEP_STEP;
        let (s, raw) = shared_store();
        for n in 0..(SWEEP_STEP + 5) {
            put_raw(&raw, &crate::control::keys::runtime("resolved", &[&format!("g{n:05}")]), json!({}));
        }
        let (out, next) = s.journal_sweep_step(Some(&"resolved:".parse().unwrap()), false, "t").unwrap();
        assert_eq!(out.deleted_markers, SWEEP_STEP);
        let next = next.expect("more markers remain");
        assert!(next.to_string().starts_with("resolved:runtime/resolved/"), "{next}");
        let (out, next) = s.journal_sweep_step(Some(&next.to_string().parse().unwrap()), false, "t").unwrap();
        assert_eq!((out.deleted_markers, next), (5, None));
        assert_eq!(effect_keys(&s), Vec::<String>::new());
    }

    #[test_id(tnyc250)]
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

    #[test_id(tlg90tf)]
    #[test]
    fn a_schedule_row_with_an_empty_id_is_refused_by_a_turn_and_by_a_carry_naming_the_row() {
        let (code, msg) = call(&store(), &["turn", &json!({"schedules": [sched("", 4)]}).to_string()]);
        assert!(code == REFUSED && msg.contains("row_id is empty") && msg.contains("kind=tick") && msg.contains("stream=s"), "{msg}");
        let row = crate::control::journal::ScheduleRow { id: String::new(), at: 9, stream: "s".into(), tag: "tick".into(), body: String::new(), cause: None, correlation: String::new() };
        let carried = crate::control::journal::Carried { schedules: vec![(row, false)], ..Default::default() };
        let err = store().journal_carry(&carried, "test").unwrap_err().to_string();
        assert!(err.contains("empty id") && err.contains("stream=s") && err.contains("kind=tick"), "{err}");
    }

    #[test_id(tw5a4to)]
    #[test]
    fn the_boot_reads_ask_the_backend_for_the_rows_they_return_not_for_every_row_ever_written() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = Counting::new(MemBackend::new(), counter.clone());
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

    #[test_id(tr9dgwu)]
    #[test]
    fn a_turn_looks_up_its_schedules_in_one_read_and_writes_only_the_new_ones() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let backend = Counting::new(MemBackend::new(), counter.clone());
        let c = Contract::open(backend.clone(), Ports::virtual_clock(&VirtualClock::new(1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        let s = crate::control::store::Store::new(c, 1_000);
        call(&s, &["turn", &json!({"schedules": [sched("old0", 4), sched("old1", 4)]}).to_string()]);
        let (gets, many) = (backend.calls_on("get", "schedule/old"), backend.calls("get_many"));
        let all: Vec<Value> = ["old0", "old1", "n0", "n1", "n2", "n3", "n4"].iter().map(|i| sched(i, 9)).collect();
        let (code, msg) = call(&s, &["turn", &json!({"schedules": all}).to_string()]);
        assert_eq!(code, 0, "{msg}");
        assert_eq!((backend.calls_on("get", "schedule/old") - gets, backend.calls("get_many") - many), (0, 1));
        let due = parsed(&s, &["due"]);
        let at = |id: &str| due.as_array().unwrap().iter().find(|r| r["id"] == id).map(|r| r["at"].clone());
        assert_eq!((at("old0"), at("n4")), (Some(json!(4)), Some(json!(9))));
    }

    #[test_id(tlmb4bp)]
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

    #[test_id(t4ngcq0)]
    #[test]
    fn the_sweep_dry_run_counts_what_it_would_remove_and_the_sweep_removes_exactly_that() {
        let (s, raw) = shared_store();
        let mut resolved_effect = effect();
        resolved_effect["request_id"] = json!("gone");
        resolved_effect["ekey"] = json!("other");
        call(&s, &["turn", &json!({"effects": [effect(), resolved_effect], "schedules": [sched("r1", 4), sched("r2", 5)]}).to_string()]);
        call(&s, &["turn", &json!({"resolved": ["gone"], "fired": ["r2"]}).to_string()]);
        put_raw(&raw, &crate::control::keys::runtime("resolved", &["ghost"]), json!({}));
        for n in 0..3 {
            put_raw(&raw, &crate::control::keys::runtime("schedule", &[&format!("carried{n}")]), json!({"at": 0, "stream": "s"}));
        }
        let (open_before, due_before) = (parsed(&s, &["open-effects"]), parsed(&s, &["due"]));
        let key_count = |table: &str| raw.list(&[crate::control::keys::runtime_table(table).as_str()], &state_store::Selector::Everything).unwrap().0.len();
        let before = (key_count("schedule"), key_count("resolved"));
        assert_eq!(before.1, 1);
        let dry = swept(&s, true);
        assert_eq!((dry["idless_schedules"].clone(), dry["deleted_markers"].clone()), (json!(3), json!(1)), "{dry}");
        assert_eq!((key_count("schedule"), key_count("resolved")), before, "a dry run removes nothing");
        assert_eq!(key_count("pending"), 1, "a dry run writes no index");
        assert_eq!(key_count("journal-meta"), 0, "a dry run marks nothing indexed");
        let done = swept(&s, false);
        assert_eq!((done["idless_schedules"].clone(), done["indexed_schedules"].clone(), done["indexed_effects"].clone(), done["next"].clone()), (json!(3), json!(1), json!(1), json!(null)), "{done}");
        assert_eq!((done["deleted_markers"].clone(), key_count("schedule"), key_count("resolved")), (json!(1), before.0 - 3, 0), "an orphan marker goes too");
        assert_eq!((parsed(&s, &["open-effects"]), parsed(&s, &["due"])), (open_before, due_before), "every legitimate row reads as it did");
    }

    fn swept<B: state_store::Backend>(s: &crate::control::store::Store<B>, dry_run: bool) -> Value {
        let (code, out) = sweep_until_done(&mut in_process(s), dry_run, None);
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

    #[test_id(tufqpnf)]
    #[test]
    fn the_sweep_loop_sums_the_counts_of_every_step_until_the_cursor_runs_out() {
        let (s, _raw) = seeded(2_500);
        let (code, out) = sweep_until_done(&mut in_process(&s), false, None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(code, 0, "{out}");
        assert_eq!((v["idless_schedules"].clone(), v["indexed_schedules"].clone(), v["steps"].clone()), (json!(2_500), json!(1), json!(8)), "{v}");
        assert_eq!(swept(&s, false)["steps"], json!(6), "a swept store ends in the schedule step, one step per legacy table, the effect step and the marker step");
    }

    #[test_id(t1lvoy9)]
    #[test]
    fn a_sweep_step_that_fails_stops_the_loop_naming_its_code_and_cursor_and_a_rerun_reaches_the_same_store() {
        let (s, raw) = seeded(1_500);
        let key_count = |table: &str| raw.list(&[crate::control::keys::runtime_table(table).as_str()], &state_store::Selector::Everything).unwrap().0.len();
        let mut calls = 0;
        let mut inner = in_process(&s);
        let (code, out) = sweep_until_done(&mut |argv| {
            calls += 1;
            if calls == 2 { (CANNOT_TELL, "cannot tell: deadline".into()) } else { inner(argv) }
        }, false, None);
        assert_eq!(code, CANNOT_TELL);
        let head: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!((head["idless_schedules"].clone(), head["steps"].clone(), head["complete"].clone()), (json!(1_000), json!(1), json!(false)), "{out}");
        let resume = head["resume"].as_str().unwrap().to_string();
        assert!(resume.starts_with("schedule:"), "{out}");
        assert!(out.contains("cannot tell: deadline") && out.contains("resume: spira-lc journal sweep --from schedule:"), "{out}");
        assert_eq!(key_count("journal-meta"), 0, "not indexed while a step is missing");
        let (code, out) = sweep_until_done(&mut in_process(&s), false, Some(resume));
        assert_eq!(code, 0, "{out}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["idless_schedules"], json!(500));
        let after_rerun = (key_count("schedule"), key_count("pending"), key_count("open-effect"), key_count("journal-meta"));
        assert_eq!(after_rerun, (1, 1, 1, 1));
        sweep_until_done(&mut in_process(&s), false, None);
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

    fn all_keys<B: state_store::Backend>(raw: &Contract<B>) -> Vec<String> {
        raw.list(&["runtime/"], &state_store::Selector::Everything).unwrap().0.into_iter().map(|i| i.key).collect()
    }

    #[test_id(t70qkv6)]
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

    #[test_id(ttfsyga)]
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

    #[test_id(tidn7vu)]
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

    fn costed_store() -> (crate::control::store::Store<counting::Costed>, Contract<counting::Costed>, VirtualClock, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let clock = VirtualClock::new(1_000);
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let backend = counting::Costed { inner: MemBackend::new(), clock: clock.clone(), armed: armed.clone() };
        let open = || Contract::open(backend.clone(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        (crate::control::store::Store::new(open(), 1_000), open(), clock, armed)
    }

    fn sweep_timed(s: &crate::control::store::Store<counting::Costed>, clock: &VirtualClock, dry_run: bool) -> (Value, u64) {
        let mut slowest = 0;
        let mut inner = in_process(s);
        let (code, out) = sweep_until_done(&mut |argv| {
            let before = clock.now_ms();
            let r = inner(argv);
            slowest = slowest.max(clock.now_ms() - before);
            r
        }, dry_run, None);
        assert_eq!(code, 0, "{out}");
        (serde_json::from_str(&out).unwrap(), slowest)
    }

    #[test_id(tk7s3hh)]
    #[test]
    fn every_sweep_step_answers_inside_the_store_target_whatever_the_size_of_the_rows_it_reads() {
        let (s, raw, clock, _) = costed_store();
        let big = json!({"pad": "x".repeat(16 * 1024)});
        for n in 0..300 {
            put_raw(&raw, &crate::control::keys::runtime("event", &[&format!("e{n:05}")]), big.clone());
        }
        for n in 0..200 {
            let id = format!("r{n:05}");
            put_raw(&raw, &crate::control::keys::runtime("effect", &["t", &format!("k{id}")]), effect_n(&id));
            if n % 2 == 0 {
                put_raw(&raw, &crate::control::keys::runtime("resolved", &[&id]), json!({}));
            }
        }
        for dry_run in [true, false] {
            let (v, slowest) = sweep_timed(&s, &clock, dry_run);
            assert_eq!((v["legacy_deleted"].clone(), v["indexed_effects"].clone(), v["deleted_effects"].clone(), v["deleted_markers"].clone()), (json!(300), json!(100), json!(100), json!(100)), "{v}");
            assert!(slowest <= 100, "a step took {slowest} ms of virtual time (dry_run {dry_run})");
        }
        assert!(all_keys(&raw).iter().all(|k| !k.starts_with("runtime/event/")));
        assert!(raw.get(&crate::control::keys::runtime("journal-meta", &["indexed"])).unwrap().is_some());
    }

    #[test_id(ts8n1bx)]
    #[test]
    fn a_step_whose_later_write_is_refused_for_the_deadline_answers_the_pages_that_committed() {
        use crate::control::journal::SWEEP_PAGE;
        let (s, raw, _clock, armed) = costed_store();
        let ev = |n: usize| crate::control::keys::runtime("event", &[&format!("e{n:05}")]);
        for n in 0..3 * SWEEP_PAGE {
            put_raw(&raw, &ev(n), json!({}));
        }
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let (code, out) = call(&s, &["sweep", "--from", "legacy:"]);
        drop(state_store::phases::deadline_scoped(std::time::Instant::now()));
        assert_eq!(code, 0, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!((v["legacy_deleted"].clone(), v["next"].clone()), (json!(SWEEP_PAGE), json!(format!("legacy:{}", ev(SWEEP_PAGE - 1)))), "{v}");
        let (code, out) = call(&s, &["sweep", "--from", v["next"].as_str().unwrap()]);
        assert_eq!(code, 0, "{out}");
        let rest: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(rest["legacy_deleted"], json!(2 * SWEEP_PAGE), "{rest}");
    }

    #[test_id(tdp2kc2)]
    #[test]
    fn a_first_page_whose_write_is_refused_for_the_deadline_still_answers_the_error() {
        use crate::control::journal::SWEEP_PAGE;
        let (s, raw, _clock, armed) = costed_store();
        for n in 0..SWEEP_PAGE {
            let id = format!("r{n:05}");
            put_raw(&raw, &crate::control::keys::runtime("effect", &["t", &format!("k{id}")]), effect_n(&id));
            put_raw(&raw, &crate::control::keys::runtime("resolved", &[&id]), json!({}));
        }
        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let (code, out) = call(&s, &["sweep", "--from", "effect:"]);
        drop(state_store::phases::deadline_scoped(std::time::Instant::now()));
        assert_eq!(code, CANNOT_TELL, "{out}");
        assert!(out.contains("Deadline"), "{out}");
    }

    #[test_id(teva8fl)]
    #[test]
    fn a_sweep_refuses_an_argument_it_does_not_know() {
        let s = store();
        for argv in [&["sweep", "--now"][..], &["sweep", "--from"][..], &["sweep", "--from", "nowhere"][..], &["sweep", "--dry-run", "--dry-run"][..]] {
            assert_eq!(call(&s, argv).0, REFUSED, "{argv:?}");
        }
        assert_eq!(call(&s, &["sweep", "--from", "legacy:zzz"]).0, REFUSED);
        assert_eq!(call(&s, &["sweep", "--from", "effect:", "--dry-run"]).0, 0);
    }

    #[test_id(t2ynpvl)]
    #[test]
    fn a_stream_keeps_its_first_poison() {
        let s = store();
        let p = |m: &str| json!({"stream": "st", "event": "e", "handler": "h", "message": m}).to_string();
        assert_eq!(call(&s, &["poisoned", "st"]).1, "null");
        call(&s, &["poison", &p("first")]);
        call(&s, &["poison", &p("second")]);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["poisoned", "st"]).1).unwrap()["message"], "first");
    }

    #[test_id(tluflho)]
    #[test]
    fn a_turn_over_the_ops_a_txn_carries_still_commits_whole() {
        let s = store();
        let rows: Vec<Value> = (0..300).map(|n| json!({"task": "t", "skey": format!("k{n}"), "value": "v", "at": n})).collect();
        assert_eq!(call(&s, &["turn", &json!({"state": rows}).to_string()]).0, 0);
        assert_eq!(serde_json::from_str::<Value>(&call(&s, &["state"]).1).unwrap()["rows"].as_array().unwrap().len(), 300);
    }

    #[test_id(tbnip8m)]
    #[test]
    fn a_state_answer_is_one_page_and_the_pages_hold_every_row_once() {
        use crate::control::journal::STATE_PAGE;
        let s = store();
        let big = "b".repeat(6_365);
        let mid = "m".repeat(120);
        let row = |n: usize| {
            let value = match n {
                0 => big.clone(),
                1..=4_010 => mid.clone(),
                _ => String::new(),
            };
            json!({"task": format!("task{}", n % 57), "skey": format!("k{n}"), "value": value, "at": n})
        };
        let all: Vec<Value> = (0..12_116).map(row).collect();
        for chunk in all.chunks(200) {
            assert_eq!(call(&s, &["turn", &json!({"state": chunk}).to_string()]).0, 0);
        }
        let (mut seen, mut after) = (Vec::new(), None::<String>);
        loop {
            let argv: Vec<&str> = match &after {
                None => vec!["state"],
                Some(c) => vec!["state", "--after", c],
            };
            let (code, out) = call(&s, &argv);
            assert_eq!(code, 0, "{out}");
            assert!(out.len() < 256 * 1024, "a page is {} bytes", out.len());
            let page: Value = serde_json::from_str(&out).unwrap();
            let rows = page["rows"].as_array().unwrap();
            assert!(rows.len() <= STATE_PAGE);
            seen.extend(rows.iter().map(|r| (r["task"].as_str().unwrap().to_string(), r["skey"].as_str().unwrap().to_string())));
            match page["next"].as_str() {
                Some(n) => after = Some(n.to_string()),
                None => break,
            }
        }
        let distinct: std::collections::BTreeSet<_> = seen.iter().cloned().collect();
        assert_eq!(seen.len(), 12_116);
        assert_eq!(distinct.len(), 12_116, "a row answered twice");
        let whole: std::collections::BTreeSet<_> = s.journal_state().unwrap().into_iter().map(|r| (r.task, r.skey)).collect();
        assert_eq!(distinct, whole);
    }

    #[test_id(t1yng6z)]
    #[test]
    fn a_state_call_takes_only_after_with_a_cursor() {
        let s = store();
        assert_eq!(call(&s, &["state", "--bogus"]).0, REFUSED);
        assert_eq!(call(&s, &["state", "--after"]).0, REFUSED);
        assert_eq!(call(&s, &["state", "--after", "a", "b"]).0, REFUSED);
    }

    /// The runtime checks a write against these widths before it sends it (runtime/src/socket.rs).
    #[test_id(thn0vc1)]
    #[test]
    fn the_runtime_checks_only_inside_these_widths() {
        let src = include_str!("../../runtime/src/socket.rs");
        for (name, max) in [("ID_MAX", ID_MAX), ("STREAM_MAX", STREAM_MAX), ("TASK_MAX", TASK_MAX), ("KEY_MAX", KEY_MAX)] {
            assert!(src.contains(&format!("const {name}: usize = {max};")), "runtime/src/socket.rs {name} differs from {max}");
        }
    }

    #[test_id(tbwlv0u)]
    #[test]
    fn a_long_text_is_clipped_to_the_limit_and_an_ellipsis() {
        assert_eq!(clip(&"x".repeat(10_000)).chars().count(), CLIP + 1);
        assert_eq!(clip("short"), "short");
    }

    #[test_id(t9cvb0s)]
    #[test]
    fn the_sweep_command_line_takes_from_in_either_order_and_refuses_a_bad_cursor() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_sweep_args(&argv(&["journal", "sweep", "--from", "effect:x", "--dry-run"])), Some(Ok((true, Some("effect:x".into())))));
        assert_eq!(parse_sweep_args(&argv(&["journal", "sweep", "--dry-run", "--from", "effect:x"])), Some(Ok((true, Some("effect:x".into())))));
        assert_eq!(parse_sweep_args(&argv(&["journal", "sweep"])), Some(Ok((false, None))));
        assert_eq!(parse_sweep_args(&argv(&["journal", "due"])), None);
        let Some(Err((code, msg))) = parse_sweep_args(&argv(&["journal", "sweep", "--from", "nope:x"])) else { panic!("accepted") };
        assert_eq!(code, REFUSED);
        assert!(msg.contains("is not schedule:<key>, legacy:<key>, effect:<key> or resolved:<key>"), "{msg}");
    }

    #[test_id(tk5d2hm)]
    #[test]
    fn a_sweep_malformed_flag_is_refused_and_a_stop_names_the_command_that_resumes_it() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for bad in [&["--from"][..], &["--from", "effect:x", "--from", "effect:y"], &["--bogus"]] {
            let mut a = vec!["journal", "sweep"];
            a.extend_from_slice(bad);
            let Some(Err((code, _))) = parse_sweep_args(&argv(&a)) else { panic!("accepted {bad:?}") };
            assert_eq!(code, REFUSED, "{bad:?}");
        }
        let (code, out) = sweep_until_done(&mut |_| (CANNOT_TELL, "cannot tell: deadline".into()), true, None);
        assert_eq!(code, CANNOT_TELL);
        let head: Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!((head["steps"].clone(), head["resume"].clone(), head["complete"].clone()), (json!(0), Value::Null, json!(false)), "{out}");
        assert!(out.ends_with("resume: spira-lc journal sweep --dry-run"), "{out}");
        let (_, out) = sweep_until_done(&mut |_| (CANNOT_TELL, "x".into()), true, Some("effect:k".into()));
        assert!(out.ends_with("resume: spira-lc journal sweep --dry-run --from effect:k"), "{out}");
    }
}
