//! The journal through spira-lc's socket: one request per call to the one door onto
//! `spira_lifecycle`, every offset-free write riding the next request ([`Staged`]). No process is
//! launched; an idle runtime holds no database connection.
//!
//! Group commit (sp-4du857). Records, schedule rows, fired marks, `applied` marks and the cursor
//! are staged, and the next request carries them all to spira-lc's `journal group`, which writes
//! them in one transaction, in the order they were made, before the request's own op. So a read
//! sees every write made before it, journal order is record order, and a scheduler turn costs a
//! transaction per read instead of one per write. A request goes out when the engine reads (each
//! pump reads back what it recorded, so a turn's records are durable before its effects are
//! offered), when [`MAX_STAGED`] records or [`MAX_STAGED_BYTES`] are staged, on [`Journal::flush`]
//! (a clean stop: the engine's drop, and this journal's), and from the flusher.
//!
//! The crash-loss window: what is staged and not yet sent. [`SocketJournal::flushing`] sends any
//! write staged [`FLUSH_AGE`] ago, checking every quarter of that, so a write reaches spira-lc at
//! most 1.25 × [`FLUSH_AGE`] (940 ms) after it was made and commits when that request answers. A
//! crash inside the window loses only writes the replay makes again: `applied` marks and the
//! cursor, and records whose cause's mark is lost with them, re-derived under the same ids.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::event::*;
use crate::journal::{Journal, Offset, Poison, ScheduleRow};
use crate::{Result, RuntimeError};

/// spira-lc's exit code for a request the journal looked at and refused.
const REFUSED: i64 = 3;
/// …and for one it could not answer (a deadline), which is safe to ask again.
const CANNOT_TELL: i64 = 2;

/// spira-lc's exit code for a request it could not answer: a store call that ran out its deadline.
const CANNOT_TELL: i64 = 2;

/// law-calls-have-deadlines: the service stops at 3 s, a client waits 5 s.
const DEADLINE: Duration = Duration::from_secs(5);

/// Widths spira-lc refuses past (spira-lc/src/journal.rs, migrations/0030-runtime-journal.sql).
/// A write is staged only inside them, so a staged write is never the one a group refuses.
const ID_MAX: usize = 255;
const STREAM_MAX: usize = 128;
const TAG_MAX: usize = 64;
const HANDLER_MAX: usize = 128;

/// The largest record staged; a bigger one goes alone, at once, so a store that cannot hold it
/// answers the caller that made it. `kind_body` is a 65,535-byte TEXT column.
const STAGE_MAX_BYTES: usize = 8 * 1024;

/// The store's `kind_body` column, in bytes; a record over it is refused here, naming it, rather
/// than by a store error that the scheduler takes for a fault and exits on.
pub const KIND_BODY_MAX: usize = 65_535;

fn within_column(stream: &StreamKey, kind: &EventKind) -> Result<()> {
    if kind.body.len() <= KIND_BODY_MAX {
        return Ok(());
    }
    Err(RuntimeError::refused(format!("journal: record on stream {} of kind {} has a {}-byte body, over the {KIND_BODY_MAX}-byte column; carry a reference, not the data", stream.0, kind.tag, kind.body.len())))
}

/// Staged records past which the next one sends them at once: one engine batch of reads.
pub const MAX_STAGED: usize = 256;

/// Staged bytes past which the next write sends them at once, keeping one request well inside a
/// 64 KiB line.
pub const MAX_STAGED_BYTES: usize = 48 * 1024;

/// How old a staged write may be before the flusher sends it (sp-4du857's bound: under 1 s).
/// Longer than a busy scheduler's gap between turns, so the flusher rarely costs a transaction
/// the next turn's read would not have carried anyway.
pub const FLUSH_AGE: Duration = Duration::from_millis(750);

/// One offset-free write in a group, in the order it was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Staging {
    Append(Event),
    Schedule(ScheduleRow),
    Fired(String),
}

/// The writes waiting for the next request (sp-rkb6wl, sp-4du857): records, schedule rows and
/// fired marks in order, then `applied` marks and the cursor. They ride whatever request goes
/// next, in its one transaction and before its own op, so every read sees them. Every one is
/// idempotent on its key, so a request that failed puts them back to ride the next.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<Staging>,
    #[serde(default)]
    pub applied: Vec<(String, String)>,
    #[serde(default)]
    pub cursor: Option<Offset>,
    /// When the oldest write still staged was made.
    #[serde(skip)]
    since: Option<Instant>,
    #[serde(skip)]
    bytes: usize,
}

impl Staged {
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.applied.is_empty() && self.cursor.is_none()
    }

    fn touch(&mut self, bytes: usize) {
        self.since.get_or_insert_with(Instant::now);
        self.bytes += bytes;
    }

    fn full(&self) -> bool {
        self.writes.len() >= MAX_STAGED || self.bytes >= MAX_STAGED_BYTES
    }

    /// Whether the oldest staged write was made `age` or more before `now`.
    fn due(&self, now: Instant, age: Duration) -> bool {
        self.since.is_some_and(|t| now.saturating_duration_since(t) >= age)
    }

    /// Put back writes a failed request carried, ahead of any staged since: a newer cursor wins.
    fn restore(&mut self, older: Staged) {
        let newer = std::mem::replace(&mut self.writes, older.writes);
        self.writes.extend(newer);
        let newer = std::mem::replace(&mut self.applied, older.applied);
        self.applied.extend(newer);
        self.cursor = self.cursor.or(older.cursor);
        self.since = older.since.or(self.since);
        self.bytes += older.bytes;
    }
}

/// Text spira-lc stores without refusing: present, NUL-free, inside its column.
fn fits(s: &str, max: usize) -> bool {
    !s.is_empty() && !s.contains('\0') && s.chars().count() <= max
}

fn fits_opt(s: Option<&EventId>) -> bool {
    s.is_none_or(|c| fits(&c.0, ID_MAX))
}

fn fits_kind(k: &EventKind) -> bool {
    fits(&k.tag, TAG_MAX) && !k.body.contains('\0')
}

pub struct SocketJournal {
    path: PathBuf,
    staged: Mutex<Staged>,
    /// Held across taking the staged writes and sending them, so two requests (the engine's and
    /// the flusher's) never commit out of order and a read never passes a write sent before it.
    wire: Mutex<()>,
    /// False once spira-lc answered that it has no `journal group` (a service older than this
    /// client): writes then go one request each, and marks by `batch` while it has that.
    group: AtomicBool,
    batch: AtomicBool,
}

impl SocketJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SocketJournal { path: path.into(), staged: Mutex::default(), wire: Mutex::new(()), group: AtomicBool::new(true), batch: AtomicBool::new(true) }
    }

    /// [`new`](Self::new), plus a flusher thread that sends any write staged `age` ago, checking
    /// every quarter of `age`; it stops once the journal is dropped. The service passes
    /// [`FLUSH_AGE`].
    pub fn flushing(path: impl Into<PathBuf>, age: Duration) -> Arc<Self> {
        let j = Arc::new(Self::new(path));
        let weak: Weak<Self> = Arc::downgrade(&j);
        let every = (age / 4).max(Duration::from_millis(1));
        let spawned = std::thread::Builder::new().name("journal-flush".into()).spawn(move || loop {
            std::thread::sleep(every);
            let Some(j) = weak.upgrade() else { return };
            if j.staged().due(Instant::now(), age) {
                // A failure puts the writes back; the next request or tick sends them again.
                let _ = j.flush();
            }
        });
        // Without the thread the writes still ride the next request, as they did before.
        drop(spawned);
        j
    }

    fn staged(&self) -> MutexGuard<'_, Staged> {
        self.staged.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, args: &[&str]) -> Result<String> {
        let mut argv = vec!["journal"];
        argv.extend_from_slice(args);
        lc_call(&self.path, &argv)
    }

    /// Stage one write, sending everything staged at once when that fills the group.
    fn stage(&self, w: Staging, bytes: usize) -> Result<()> {
        let full = {
            let mut s = self.staged();
            s.writes.push(w);
            s.touch(bytes);
            s.full()
        };
        if full {
            self.flush()?;
        }
        Ok(())
    }

    /// One request: `args` (none for a bare flush), carrying the staged writes. Every staged
    /// write is idempotent, so a request that failed puts them back to ride the next.
    fn call(&self, args: &[&str]) -> Result<String> {
        let _wire = self.wire.lock().unwrap_or_else(|e| e.into_inner());
        let staged = std::mem::take(&mut *self.staged());
        if staged.is_empty() {
            return if args.is_empty() { Ok(String::new()) } else { self.send(args) };
        }
        let r = self.carry(&staged, args);
        if r.is_err() {
            self.staged().restore(staged);
        }
        r
    }

    fn carry(&self, staged: &Staged, args: &[&str]) -> Result<String> {
        let unknown = |e: &RuntimeError, op: &str| e.is_refused() && e.0.contains(&format!("unknown op \"{op}\""));
        if self.group.load(Ordering::Relaxed) {
            let writes = enc(staged)?;
            let mut argv = vec!["group", writes.as_str()];
            argv.extend_from_slice(args);
            match self.send(&argv) {
                Err(e) if unknown(&e, "group") => self.group.store(false, Ordering::Relaxed),
                r => return r,
            }
        }
        for w in &staged.writes {
            match w {
                Staging::Append(ev) => self.send(&["append", &enc(ev)?]).map(|_| ())?,
                Staging::Schedule(row) => self.send(&["schedule", &enc(row)?]).map(|_| ())?,
                Staging::Fired(id) => self.send(&["mark-fired", id]).map(|_| ())?,
            }
        }
        let marks = Staged { applied: staged.applied.clone(), cursor: staged.cursor, ..Staged::default() };
        if marks.is_empty() {
            return if args.is_empty() { Ok(String::new()) } else { self.send(args) };
        }
        if self.batch.load(Ordering::Relaxed) {
            let writes = enc(&marks)?;
            let mut argv = vec!["batch", writes.as_str()];
            argv.extend_from_slice(args);
            match self.send(&argv) {
                Err(e) if unknown(&e, "batch") => self.batch.store(false, Ordering::Relaxed),
                r => return r,
            }
        }
        for (h, id) in &marks.applied {
            self.send(&["mark-applied", h, id])?;
        }
        if let Some(c) = marks.cursor {
            self.send(&["set-cursor", &c.to_string()])?;
        }
        if args.is_empty() { Ok(String::new()) } else { self.send(args) }
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<T> {
        serde_json::from_str(&self.call(args)?).map_err(|e| RuntimeError::new(format!("journal reply: {e}")))
    }

    fn number(&self, args: &[&str]) -> Result<u64> {
        self.call(args)?.trim().parse().map_err(|_| RuntimeError::new("journal reply is not a number"))
    }
}

/// A clean stop sends what is still staged; the engine's own drop has usually sent it already.
impl Drop for SocketJournal {
    fn drop(&mut self) {
        let _ = self.flush();
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
    let down = |why: &dyn std::fmt::Display| RuntimeError::cannot_tell(format!("cannot tell: spira-lc is not answering on {}: {why}", path.display()));
    let stream = connect(path).map_err(|e| down(&e))?;
    stream.set_read_timeout(Some(deadline)).and_then(|_| stream.set_write_timeout(Some(deadline))).map_err(|e| down(&e))?;
    let request = serde_json::to_string(argv).map_err(|e| RuntimeError::new(e.to_string()))?;
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

/// Serves one connection in the protocol `lc_call` speaks: a JSON argv line in; a head line
/// `{"exit_code","len"}` and `len` bytes of body out; reads and writes held to the client's deadline.
pub fn answer_one(stream: UnixStream, respond: &dyn Fn(&[String]) -> Result<String>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(DEADLINE))?;
    stream.set_write_timeout(Some(DEADLINE))?;
    let mut line = String::new();
    BufReader::new(&stream).take(64 * 1024).read_line(&mut line)?;
    let reply = match serde_json::from_str::<Vec<String>>(line.trim()) {
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

fn enc<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|e| RuntimeError::new(e.to_string()))
}

impl Journal for SocketJournal {
    fn append(&self, ev: &Event) -> Result<Offset> {
        within_column(&ev.stream, &ev.kind)?;
        self.number(&["append", &enc(ev)?])
    }

    fn record(&self, ev: &Event) -> Result<()> {
        within_column(&ev.stream, &ev.kind)?;
        let body = enc(ev)?;
        let fits = fits(&ev.id.0, ID_MAX) && fits(&ev.stream.0, STREAM_MAX) && fits_kind(&ev.kind) && fits_opt(ev.cause.as_ref()) && fits(&ev.correlation.0, ID_MAX);
        if !fits || body.len() > STAGE_MAX_BYTES {
            return self.call(&["append", &body]).map(|_| ());
        }
        self.stage(Staging::Append(ev.clone()), body.len())
    }

    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.json(&["read", &from.to_string(), &limit.to_string()])
    }

    fn read_stream(&self, stream: &StreamKey, below: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.json(&["stream", &stream.0, &below.to_string(), &limit.to_string()])
    }

    fn end(&self) -> Result<Offset> {
        Ok(self.number(&["end"])?.max(self.cursor()?))
    }

    fn unresolved_effects(&self) -> Result<Vec<(Offset, Event)>> {
        self.json(&["open-effects"])
    }

    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        Ok(self.number(&["applied", handler, &id.0])? == 1)
    }

    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        if !(fits(handler, HANDLER_MAX) && fits(&id.0, ID_MAX)) {
            return self.call(&["mark-applied", handler, &id.0]).map(|_| ());
        }
        let mut s = self.staged();
        s.applied.push((handler.into(), id.0.clone()));
        s.touch(handler.len() + id.0.len());
        Ok(())
    }

    fn cursor(&self) -> Result<Offset> {
        self.number(&["cursor"])
    }

    fn set_cursor(&self, next: Offset) -> Result<()> {
        let mut s = self.staged();
        s.cursor = Some(next);
        s.touch(0);
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.call(&[]).map(|_| ())
    }

    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        within_column(&row.stream, &row.kind)?;
        let body = enc(row)?;
        let fits = fits(&row.id, ID_MAX) && fits(&row.stream.0, STREAM_MAX) && fits_kind(&row.kind) && fits_opt(row.cause.as_ref()) && fits(&row.correlation.0, ID_MAX);
        if !fits || body.len() > STAGE_MAX_BYTES {
            return self.call(&["schedule", &body]).map(|_| ());
        }
        self.stage(Staging::Schedule(row.clone()), body.len())
    }

    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.json(&["due"])
    }

    fn mark_fired(&self, row_id: &str) -> Result<()> {
        if !fits(row_id, ID_MAX) {
            return self.call(&["mark-fired", row_id]).map(|_| ());
        }
        self.stage(Staging::Fired(row_id.into()), row_id.len())
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.call(&["poison", &enc(p)?]).map(|_| ())
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.json(&["poisoned", &stream.0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;


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

    #[test]
    fn a_store_that_is_not_listening_is_a_cannot_tell() {
        let err = lc_call(&testkit::TempDir::new("socket-absent").path().join("s.sock"), &["cursor"]).unwrap_err();
        assert!(err.is_cannot_tell(), "{err}");
    }

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

    /// The flusher's bound: a write is due once it is `age` old, whichever write made it so
    /// (sp-4du857), and a restore keeps the older write's age.
    #[test]
    fn staged_writes_are_due_at_the_age_of_the_oldest() {
        let age = FLUSH_AGE;
        let mut s = Staged::default();
        let t0 = Instant::now(); // clock-safe: a base instant; every comparison is arithmetic on it
        assert!(!s.due(t0 + age * 2, age), "nothing staged is never due");
        s.since = Some(t0);
        s.cursor = Some(1);
        assert!(!s.due(t0 + age - Duration::from_millis(1), age));
        assert!(s.due(t0 + age, age));
        let mut newer = Staged { since: Some(t0 + age), applied: vec![("h".into(), "e".into())], ..Staged::default() };
        newer.restore(s);
        assert!(newer.due(t0 + age, age), "the restored write is the oldest");
        assert!(FLUSH_AGE + FLUSH_AGE / 4 < Duration::from_secs(1), "the crash-loss window stays under a second");
    }

    #[test]
    fn a_record_over_the_column_is_refused_naming_its_stream_and_kind_before_any_send() {
        let j = SocketJournal::new("/nonexistent/lc.sock");
        let at = |body: String| Event { id: EventId("e".into()), stream: StreamKey("watch.x".into()), kind: EventKind::new("x.kind", &body), at: 1, cause: None, correlation: CorrelationId("c".into()) };
        for r in [j.record(&at("x".repeat(KIND_BODY_MAX + 1))), j.append(&at("x".repeat(KIND_BODY_MAX + 1))).map(|_| ())] {
            let e = r.unwrap_err();
            assert!(e.is_refused() && e.0.contains("watch.x") && e.0.contains("x.kind"), "{e}");
        }
        let row = ScheduleRow { id: "r".into(), at: 1, stream: StreamKey("watch.x".into()), kind: EventKind::new("x.kind", &"x".repeat(KIND_BODY_MAX + 1)), cause: None, correlation: CorrelationId("c".into()) };
        assert!(j.schedule(&row).unwrap_err().is_refused());
    }

    #[test]
    fn a_group_serializes_its_writes_tagged_by_op_and_omits_none() {
        let ev = Event { id: EventId("e".into()), stream: StreamKey("s".into()), kind: EventKind::new("t", "b"), at: 1, cause: None, correlation: CorrelationId("c".into()) };
        let s = Staged { writes: vec![Staging::Append(ev), Staging::Fired("r".into())], ..Staged::default() };
        let v: serde_json::Value = serde_json::from_str(&enc(&s).unwrap()).unwrap();
        assert_eq!(v["writes"][0]["append"]["id"], "e");
        assert_eq!(v["writes"][1]["fired"], "r");
        let marks: serde_json::Value = serde_json::from_str(&enc(&Staged { cursor: Some(2), ..Staged::default() }).unwrap()).unwrap();
        assert!(marks.get("writes").is_none(), "a marks-only batch is what an older `journal batch` reads: {marks}");
    }
}
