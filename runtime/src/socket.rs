//! The journal through spira-lc's socket: one request per call to the one door onto
//! `spira_lifecycle`, the idempotent marks and cursor riding the next request ([`Staged`]). No process is launched; an idle runtime holds no database connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::event::*;
use crate::journal::{Journal, Offset, Poison, ScheduleRow};
use crate::{Result, RuntimeError};

/// spira-lc's exit code for a request the journal looked at and refused.
const REFUSED: i64 = 3;

/// law-calls-have-deadlines: the service stops at 3 s, a client waits 5 s.
const DEADLINE: Duration = Duration::from_secs(5);

/// Widths spira-lc refuses past (spira-lc/src/journal.rs, migrations/0030-runtime-journal.sql).
/// A mark is staged only inside them, so a staged write is never the one a batch refuses.
const ID_MAX: usize = 255;
const HANDLER_MAX: usize = 128;

/// The idempotent writes waiting for the next request (sp-rkb6wl): `applied` marks and the
/// cursor. They ride whatever request goes next, in its one transaction and before its own op,
/// so every read sees them, and a turn costs one store round-trip, not one per consuming handler.
/// Until then a crash loses only marks and a cursor advance, and a replay re-derives both.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    #[serde(default)]
    pub applied: Vec<(String, String)>,
    #[serde(default)]
    pub cursor: Option<Offset>,
}

impl Staged {
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty() && self.cursor.is_none()
    }

    /// Put back writes a failed request carried, ahead of any staged since: a newer cursor wins.
    fn restore(&mut self, older: Staged) {
        let newer = std::mem::replace(&mut self.applied, older.applied);
        self.applied.extend(newer);
        self.cursor = self.cursor.or(older.cursor);
    }
}

pub struct SocketJournal {
    path: PathBuf,
    staged: Mutex<Staged>,
    /// False once spira-lc answered that it has no `journal batch` (a service older than this
    /// client): staged writes then go one request each, as they did before.
    batch: AtomicBool,
}

impl SocketJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SocketJournal { path: path.into(), staged: Mutex::default(), batch: AtomicBool::new(true) }
    }

    fn staged(&self) -> MutexGuard<'_, Staged> {
        self.staged.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, args: &[&str]) -> Result<String> {
        let mut argv = vec!["journal"];
        argv.extend_from_slice(args);
        lc_call(&self.path, &argv)
    }

    /// One request: `args` (none for a bare flush), carrying the staged writes. Every staged
    /// write is idempotent, so a request that failed puts them back to ride the next.
    fn call(&self, args: &[&str]) -> Result<String> {
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
        if self.batch.load(Ordering::Relaxed) {
            let writes = enc(staged)?;
            let mut argv = vec!["batch", writes.as_str()];
            argv.extend_from_slice(args);
            match self.send(&argv) {
                Err(e) if e.is_refused() && e.0.contains("unknown op \"batch\"") => self.batch.store(false, Ordering::Relaxed),
                r => return r,
            }
        }
        for (h, id) in &staged.applied {
            self.send(&["mark-applied", h, id])?;
        }
        if let Some(c) = staged.cursor {
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
    let down = |why: &dyn std::fmt::Display| RuntimeError::new(format!("cannot tell: spira-lc is not answering on {}: {why}", path.display()));
    let stream = connect(path).map_err(|e| down(&e))?;
    stream.set_read_timeout(Some(deadline)).and_then(|_| stream.set_write_timeout(Some(deadline))).map_err(|e| down(&e))?;
    let request = serde_json::to_string(argv).map_err(|e| RuntimeError::new(e.to_string()))?;
    writeln!(&stream, "{request}").map_err(|e| down(&e))?;

    let lost = |why: &str| RuntimeError::new(format!("cannot tell: spira-lc took the journal request but its reply was lost ({why})"));
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
        _ => Err(RuntimeError::new(body)),
    }
}

/// `lc_call`'s exit code for a reply: 0, `REFUSED`, or 1 for a fault.
fn exit_code(r: &Result<String>) -> i64 {
    match r {
        Ok(_) => 0,
        Err(e) if e.is_refused() => REFUSED,
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
        self.number(&["append", &enc(ev)?])
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
        let fits = |s: &str, max: usize| !s.is_empty() && !s.contains('\0') && s.chars().count() <= max;
        if !(fits(handler, HANDLER_MAX) && fits(&id.0, ID_MAX)) {
            return self.call(&["mark-applied", handler, &id.0]).map(|_| ());
        }
        self.staged().applied.push((handler.into(), id.0.clone()));
        Ok(())
    }

    fn cursor(&self) -> Result<Offset> {
        self.number(&["cursor"])
    }

    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.staged().cursor = Some(next);
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        self.call(&[]).map(|_| ())
    }

    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.call(&["schedule", &enc(row)?]).map(|_| ())
    }

    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.json(&["due"])
    }

    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.call(&["mark-fired", row_id]).map(|_| ())
    }

    fn poison(&self, p: &Poison) -> Result<()> {
        self.call(&["poison", &enc(p)?]).map(|_| ())
    }

    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.json(&["poisoned", &stream.0])
    }
}
