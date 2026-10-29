//! The journal through spira-lc's socket: one request per call to the one door onto
//! `spira_lifecycle`. No process is launched; an idle runtime holds no database connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::event::*;
use crate::journal::{Journal, Offset, Poison, ScheduleRow};
use crate::{Result, RuntimeError};

/// law-calls-have-deadlines: the service stops at 3 s, a client waits 5 s.
const DEADLINE: Duration = Duration::from_secs(5);

pub struct SocketJournal {
    path: PathBuf,
}

impl SocketJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SocketJournal { path: path.into() }
    }

    fn call(&self, args: &[&str]) -> Result<String> {
        let mut argv = vec!["journal"];
        argv.extend_from_slice(args);
        lc_call(&self.path, &argv)
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<T> {
        serde_json::from_str(&self.call(args)?).map_err(|e| RuntimeError::new(format!("journal reply: {e}")))
    }

    fn number(&self, args: &[&str]) -> Result<u64> {
        self.call(args)?.trim().parse().map_err(|_| RuntimeError::new("journal reply is not a number"))
    }
}

/// One request to spira-lc's socket: the argv it would take on its command line.
pub fn lc_call(path: &std::path::Path, argv: &[&str]) -> Result<String> {
    let down = |why: &dyn std::fmt::Display| RuntimeError::new(format!("cannot tell: spira-lc is not answering on {}: {why}", path.display()));
    let stream = UnixStream::connect(path).map_err(|e| down(&e))?;
    stream.set_read_timeout(Some(DEADLINE)).and_then(|_| stream.set_write_timeout(Some(DEADLINE))).map_err(|e| down(&e))?;
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
    if code != 0 {
        return Err(RuntimeError::new(body));
    }
    Ok(body)
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

    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        Ok(self.number(&["applied", handler, &id.0])? == 1)
    }

    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.call(&["mark-applied", handler, &id.0]).map(|_| ())
    }

    fn cursor(&self) -> Result<Offset> {
        self.number(&["cursor"])
    }

    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.call(&["set-cursor", &next.to_string()]).map(|_| ())
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
