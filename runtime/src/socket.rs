//! The journal through spira-lc's socket: one request per call to the one door onto
//! the lifecycle store. No process is launched; an idle runtime holds no database connection.
//!
//! Events are shipped to VictoriaLogs, best effort, before the turn that made them commits:
//! [`SocketJournal::record`] queues one for the shipper thread and returns. The store sees one
//! `journal turn` per turn and reads only at boot.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;
use spira_otel::vlogs::{bead_id, Shipper, ShipCounts};

use crate::event::*;
use crate::journal::{Boot, EffectRow, Journal, Poison, ScheduleRow, StateRow, Turn};
use crate::{Result, RuntimeError};

/// spira-lc's exit code for a request the journal looked at and refused.
const REFUSED: i64 = 3;
/// spira-lc's exit code for a request it could not answer: a store call that ran out its deadline.
const CANNOT_TELL: i64 = 2;

/// law-calls-have-deadlines: the service stops at 3 s, a client waits 5 s.
const DEADLINE: Duration = Duration::from_secs(5);

/// Widths spira-lc refuses past (spira-lc/src/journal.rs, schema.sql). A turn over them is
/// refused here, before any send, so it is the caller that hears which write was too wide.
const ID_MAX: usize = 255;
const STREAM_MAX: usize = 128;
const TASK_MAX: usize = 128;
const KEY_MAX: usize = 255;

fn within(what: &str, value: &str, max: usize) -> Result<()> {
    if value.chars().count() <= max && !value.contains('\0') {
        return Ok(());
    }
    Err(RuntimeError::refused(format!("journal: {what} is {} chars (max {max}) or holds a NUL: {}", value.chars().count(), value.chars().take(80).collect::<String>())))
}

pub(crate) fn within_widths(turn: &Turn) -> Result<()> {
    for s in &turn.state {
        within("state task", &s.task, TASK_MAX)?;
        within("state key", &s.skey, KEY_MAX)?;
        if s.value.contains('\0') {
            return Err(RuntimeError::refused(format!("journal: state {}/{} holds a NUL", s.task, s.skey)));
        }
    }
    for (task, skey) in &turn.cleared {
        within("state task", task, TASK_MAX)?;
        within("state key", skey, KEY_MAX)?;
    }
    for e in &turn.effects {
        within("effect task", &e.task, TASK_MAX)?;
        within("effect key", &e.ekey, KEY_MAX)?;
        within("effect request id", &e.request_id, ID_MAX)?;
        within("effect stream", &e.stream.0, STREAM_MAX)?;
    }
    for id in turn.resolved.iter().chain(&turn.fired) {
        within("mark", id, ID_MAX)?;
    }
    for r in &turn.schedules {
        within("schedule row", &r.id, ID_MAX)?;
        within("schedule stream", &r.stream.0, STREAM_MAX)?;
    }
    Ok(())
}

/// The record an event is shipped as: the stream fields, the event's own, and the bead it
/// concerns (the first `sp-` id in the stream name, else in the body).
pub fn event_record(ev: &Event) -> serde_json::Value {
    serde_json::json!({
        "stream": ev.stream.0,
        "kind_tag": ev.kind.tag,
        "event_id": ev.id.0,
        "at": ev.at,
        "cause": ev.cause.as_ref().map(|c| c.0.as_str()),
        "correlation": ev.correlation.0,
        "bead_id": bead_id(&ev.stream.0).or_else(|| bead_id(&ev.kind.body)),
        "body": ev.kind.body,
    })
}

pub struct SocketJournal {
    path: PathBuf,
    shipper: Shipper,
}

impl SocketJournal {
    pub fn new(path: impl Into<PathBuf>, shipper: Shipper) -> Self {
        SocketJournal { path: path.into(), shipper }
    }

    fn send(&self, args: &[&str]) -> Result<String> {
        let mut argv = vec!["journal"];
        argv.extend_from_slice(args);
        lc_call_as(&self.path, &argv, Some(crate::journal::SCHEDULER_TASK), DEADLINE)
    }

    fn json<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T> {
        serde_json::from_str(&self.send(args)?).map_err(|e| RuntimeError::new(format!("journal reply: {e}")))
    }

    fn json_after<T: DeserializeOwned>(&self, verb: &str, after: &str) -> Result<T> {
        self.json(&[verb, "--after", after]).map_err(|e| RuntimeError(format!("journal {verb}: {}", e.0), e.1))
    }

    fn json_of<T: DeserializeOwned>(&self, verb: &str) -> Result<T> {
        self.json(&[verb]).map_err(|e| RuntimeError(format!("journal {verb}: {}", e.0), e.1))
    }
}

fn connect(path: &std::path::Path) -> std::io::Result<UnixStream> {
    let sock = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    sock.connect_timeout(&socket2::SockAddr::unix(path)?, DEADLINE)?;
    Ok(std::os::fd::OwnedFd::from(sock).into())
}

/// One request to spira-lc's socket: the argv it would take on its command line.
pub fn lc_call(path: &std::path::Path, argv: &[&str]) -> Result<String> {
    lc_call_within(path, argv, DEADLINE)
}

/// [`lc_call`] for a verb that does work of its own (a fetch) and so waits longer than a store read.
pub fn lc_call_within(path: &std::path::Path, argv: &[&str], deadline: Duration) -> Result<String> {
    lc_call_as(path, argv, None, deadline)
}

/// [`lc_call_within`] for a request made on behalf of `task`, which the store records as the task of the request.
pub fn lc_call_as(path: &std::path::Path, argv: &[&str], task: Option<&str>, deadline: Duration) -> Result<String> {
    let down = |why: &dyn std::fmt::Display| RuntimeError::cannot_tell(format!("cannot tell: spira-lc is not answering on {}: {why}", path.display()));
    let stream = connect(path).map_err(|e| down(&e))?;
    stream.set_read_timeout(Some(deadline)).and_then(|_| stream.set_write_timeout(Some(deadline))).map_err(|e| down(&e))?;
    let request = match task {
        Some(task) => serde_json::to_string(&serde_json::json!({ "argv": argv, "task": task })),
        None => serde_json::to_string(argv),
    }
    .map_err(|e| RuntimeError::new(e.to_string()))?;
    writeln!(&stream, "{request}").map_err(|e| down(&e))?;

    let lost = |why: &str| RuntimeError::cannot_tell(format!("cannot tell: spira-lc took the journal request but its reply was lost ({why})"));
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    if !matches!(reader.read_line(&mut head), Ok(n) if n > 0) {
        return Err(lost("no reply"));
    }
    let head: serde_json::Value = serde_json::from_str(head.trim()).map_err(|_| lost("malformed reply"))?;
    let (Some(code), Some(len)) = (head["exit_code"].as_i64(), head["len"].as_u64()) else { return Err(lost("malformed reply")) };
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).map_err(|_| lost("truncated reply"))?;
    let body = String::from_utf8(body).map_err(|_| lost("non-utf8 reply"))?;
    match code {
        0 => Ok(body),
        REFUSED => Err(RuntimeError::refused(body)),
        CANNOT_TELL => Err(RuntimeError::cannot_tell(body)),
        _ => Err(RuntimeError::new(body)),
    }
}

/// `lc_call`'s exit code for a reply: 0, `REFUSED`, `CANNOT_TELL`, or 1 for a fault.
fn exit_code(r: &Result<String>) -> i64 {
    match r {
        Ok(_) => 0,
        Err(e) if e.is_refused() => REFUSED,
        Err(e) if e.is_cannot_tell() => CANNOT_TELL,
        Err(_) => 1,
    }
}

/// The argv of a request head: a bare array, or the `{"argv", …}` object a write under a request id sends.
fn argv_of(head: &str) -> serde_json::Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Head {
        Bare(Vec<String>),
        Object { argv: Vec<String> },
    }
    serde_json::from_str(head).map(|h| match h {
        Head::Bare(argv) | Head::Object { argv } => argv,
    })
}

/// Serves one connection in the protocol `lc_call` speaks: a JSON argv line in; a head line
/// `{"exit_code","len"}` and `len` bytes of body out; reads and writes held to the client's deadline.
pub fn answer_one(stream: UnixStream, respond: &dyn Fn(&[String]) -> Result<String>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(DEADLINE))?;
    stream.set_write_timeout(Some(DEADLINE))?;
    let mut line = String::new();
    BufReader::new(&stream).take(64 * 1024).read_line(&mut line)?;
    let reply = match argv_of(line.trim()) {
        Ok(argv) => respond(&argv),
        Err(e) => Err(RuntimeError::refused(format!("request is not a JSON argv: {e}"))),
    };
    let code = exit_code(&reply);
    let body = reply.unwrap_or_else(|e| e.0);
    let mut out = &stream;
    writeln!(out, "{}", serde_json::json!({ "exit_code": code, "len": body.len() }))?;
    out.write_all(body.as_bytes())?;
    out.flush()
}

/// Answers connections one at a time until `listener` fails. A client that stalls holds the
/// next one for at most the deadline.
pub fn answer(listener: &std::os::unix::net::UnixListener, respond: &dyn Fn(&[String]) -> Result<String>) -> std::io::Result<()> {
    loop {
        let (stream, _) = listener.accept()?;
        let _ = answer_one(stream, respond);
    }
}

#[derive(serde::Deserialize)]
struct StatePage {
    rows: Vec<StateRow>,
    next: Option<String>,
}

fn enc<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| RuntimeError::new(e.to_string()))
}

impl Journal for SocketJournal {
    fn record(&self, ev: &Event) {
        self.shipper.ship(&event_record(ev));
    }

    fn boot(&self) -> Result<Boot> {
        let mut state: Vec<StateRow> = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page: StatePage = match &after {
                None => self.json_of("state")?,
                Some(c) => self.json_after("state", c)?,
            };
            state.extend(page.rows);
            match page.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        let effects: Vec<EffectRow> = self.json_of("open-effects")?;
        let schedules: Vec<ScheduleRow> = self.json_of("due")?;
        Ok(Boot { state, effects, schedules })
    }

    fn commit(&self, turn: &Turn) -> Result<()> {
        within_widths(turn)?;
        self.send(&["turn", &enc(turn)?]).map(|_| ())
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.send(&["poison", &enc(p)?]).map(|_| ())
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.json(&["poisoned", &stream.0])
    }

    fn poisoned_all(&self) -> Result<Vec<Poison>> {
        self.json(&["poisoned", "--all"]).map_err(|e| RuntimeError(format!("journal poisoned --all: {}", e.0), e.1))
    }

    fn shipped(&self) -> Option<ShipCounts> {
        Some(self.shipper.counts())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t4y223x)]
    #[test]
    fn a_write_sent_under_a_request_id_is_answered_with_its_argv() {
        use std::io::{Read, Write};
        let (mut client, server) = UnixStream::pair().unwrap();
        let t = std::thread::spawn(move || answer_one(server, &|argv| Ok(argv.join(" "))).unwrap());
        writeln!(client, "{}", serde_json::json!({ "argv": ["hold", "sp-a"], "request_id": "r-1" })).unwrap();
        let mut got = String::new();
        client.read_to_string(&mut got).unwrap();
        t.join().unwrap();
        assert_eq!(got.lines().nth(1), Some("hold sp-a"), "{got}");
    }

    #[test_id(t1io6s9)]
    #[test]
    fn a_plain_lc_call_claims_no_task() {
        let dir = testkit::TempDir::new("socket-no-task");
        let path = dir.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            line
        });
        let _ = lc_call(&path, &["scheduler", "status"]);
        assert_eq!(t.join().unwrap().trim(), r#"["scheduler","status"]"#);
    }

    #[test_id(tgwwk9o)]
    #[test]
    fn a_journal_request_head_names_the_scheduler_task_beside_the_argv() {
        let dir = testkit::TempDir::new("socket-task-head");
        let path = dir.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let body = r#"{"rows":[],"next":null}"#;
            writeln!(&stream, "{}", serde_json::json!({ "exit_code": 0, "len": body.len() })).unwrap();
            (&stream).write_all(body.as_bytes()).unwrap();
            line
        });
        let _ = SocketJournal::new(&path, Shipper::start("http://127.0.0.1:9")).send(&["cursor"]);
        let head: serde_json::Value = serde_json::from_str(t.join().unwrap().trim()).unwrap();
        assert_eq!(head["task"], crate::journal::SCHEDULER_TASK);
        assert_eq!(head["argv"], serde_json::json!(["journal", "cursor"]));
    }

    #[test_id(trnxatt)]
    #[test]
    fn a_closed_connection_with_no_reply_is_a_cannot_tell() {
        let dir = testkit::TempDir::new("socket-closed");
        let path = dir.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || drop(listener.accept().unwrap()));
        let err = lc_call(&path, &["cursor"]).unwrap_err();
        t.join().unwrap();
        assert!(err.is_cannot_tell(), "{err}");
    }

    #[test_id(t10ryji)]
    #[test]
    fn a_boot_that_the_store_stops_answering_names_the_verb_it_was_waiting_on() {
        let dir = testkit::TempDir::new("socket-boot-verb");
        let path = dir.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            answer_one(stream, &|_| Ok(r#"{"rows":[],"next":null}"#.into())).unwrap();
            drop(listener.accept().unwrap());
        });
        let j = SocketJournal::new(&path, Shipper::start("http://127.0.0.1:9"));
        let Err(err) = j.boot() else { panic!("the second ask gets no reply") };
        t.join().unwrap();
        assert!(err.is_cannot_tell() && err.0.starts_with("journal open-effects:"), "{err}");
    }

    #[test_id(tg71igg)]
    #[test]
    fn a_store_that_is_not_listening_is_a_cannot_tell() {
        let err = lc_call(&testkit::TempDir::new("socket-absent").path().join("s.sock"), &["cursor"]).unwrap_err();
        assert!(err.is_cannot_tell(), "{err}");
    }

    #[test_id(tesespv)]
    #[test]
    fn the_stores_own_cannot_tell_exit_arrives_as_a_cannot_tell_and_a_fault_does_not() {
        let dir = testkit::TempDir::new("socket-exit2");
        let path = dir.path().join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            for reply in [RuntimeError::cannot_tell("cannot tell: deadline"), RuntimeError::new("broken")] {
                let (stream, _) = listener.accept().unwrap();
                answer_one(stream, &|_| Err(reply.clone())).unwrap();
            }
        });
        let first = lc_call(&path, &["cursor"]).unwrap_err();
        let second = lc_call(&path, &["cursor"]).unwrap_err();
        t.join().unwrap();
        assert!(first.is_cannot_tell(), "{first}");
        assert!(!second.is_cannot_tell() && !second.is_refused(), "{second}");
    }

    #[test_id(tgwbvtx)]
    #[test]
    fn a_turn_past_the_stores_widths_is_refused_before_any_send() {
        let j = SocketJournal::new(format!("{}/lc.sock", system_paths::NONEXISTENT), Shipper::start("http://127.0.0.1:9"));
        let wide = Turn { state: vec![StateRow { task: "t".into(), skey: "k".repeat(KEY_MAX + 1), value: "v".into(), at: 1 }], ..Turn::default() };
        let e = j.commit(&wide).unwrap_err();
        assert!(e.is_refused() && e.0.contains("state key"), "{e}");
        let cleared = Turn { cleared: vec![("t".into(), "k".repeat(KEY_MAX + 1))], ..Turn::default() };
        assert!(within_widths(&cleared).is_err());
        let ok = Turn { state: vec![StateRow { task: "t".into(), skey: "k".repeat(KEY_MAX), value: "v".into(), at: 1 }], ..Turn::default() };
        assert!(j.commit(&ok).unwrap_err().is_cannot_tell(), "an in-width turn goes to the socket, which is absent");
    }

    #[test_id(t3f809p)]
    #[test]
    fn the_in_memory_journal_refuses_the_widths_the_store_refuses() {
        let j = crate::journal::MemJournal::new();
        let wide = Turn { state: vec![StateRow { task: "t".into(), skey: "k".repeat(KEY_MAX + 1), value: "v".into(), at: 1 }], ..Turn::default() };
        assert!(j.commit(&wide).unwrap_err().is_refused());
        assert!(j.commits().is_empty(), "a refused turn is not kept");
    }
}
