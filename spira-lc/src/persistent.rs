//! A persistent `dolt sql` session: one long-lived subprocess, reused across many requests,
//! for the system service (`spira-lc serve`). The one-shot CLI path in `db.rs` pays a fresh
//! TCP + auth handshake and a full `dolt` process start on every call — measured at
//! 150-230ms each, which cannot meet the design's p99 < 50ms transition bench no matter
//! how the SQL itself is written. A live connection amortizes that cost across the
//! service's whole lifetime instead of paying it per request.
//!
//! FRAMING. `dolt sql -r json`, fed a script over stdin with no `-q`, prints one JSON block
//! per statement as it executes them (verified empirically against a real Dolt 2.2.3
//! server: a `SELECT` sent after a delay is answered before the next statement is even
//! sent). This module appends a unique sentinel `SELECT` after every request and reads
//! blocks until that sentinel's own block appears, then returns everything read before it.
//!
//! BOUNDED, NOT BLOCKING FOREVER. Reads happen on a background thread that only ever
//! blocks on the pipe; the request path waits on a channel with a timeout. A session that
//! doesn't answer within the timeout is treated as `CannotTell` and torn down — the next
//! request respawns a fresh one. A hung `dolt` process can therefore never hang a caller.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::db::{DbError, ScriptFailure};

static COUNTER: AtomicU64 = AtomicU64::new(0);

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct PersistentDolt {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
}

impl PersistentDolt {
    pub fn spawn(mut command: Command) -> Result<Self, DbError> {
        let mut child = command
            .args(["sql", "-r", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| DbError::CannotTell(format!("spawning persistent dolt session: {e}")))?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdin = child.stdin.take().expect("piped stdin");

        let (tx, rx) = mpsc::channel();
        let tx_out = tx.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx_out.send(line).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        Ok(PersistentDolt { child, stdin, lines: rx })
    }

    /// Send a script, wait for its answer, and return the raw text of every block dolt
    /// printed before the sentinel — exactly the shape `db::parse_last_json_rows` and
    /// friends already expect from the one-shot path, so callers do not need to know
    /// which path answered them.
    pub fn send(&mut self, script: &str) -> Result<String, ScriptFailure> {
        let marker = format!("spira_lc_sentinel_{}_{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        let framed = format!("{script}\nSELECT '{marker}' AS sentinel;\n");
        self.stdin
            .write_all(framed.as_bytes())
            .and_then(|_| self.stdin.flush())
            .map_err(|e| ScriptFailure::CannotTell(format!("writing to persistent session: {e}")))?;

        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        let mut collected = String::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ScriptFailure::CannotTell("persistent dolt session timed out waiting for a response".into()));
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    if line.contains(&marker) {
                        break;
                    }
                    collected.push_str(&line);
                    collected.push('\n');
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(ScriptFailure::CannotTell("persistent dolt session timed out waiting for a response".into()));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ScriptFailure::CannotTell("persistent dolt session exited".into()));
                }
            }
        }

        if collected.contains("40001") || collected.contains("serialization failure") {
            return Err(ScriptFailure::LostRace);
        }
        // dolt's own error lines are consistently prefixed this way (observed against a
        // real 2.2.3 server); evidence text is never expected to contain it verbatim.
        if collected.contains("error on line ") {
            return Err(ScriptFailure::CannotTell(collected));
        }
        Ok(collected)
    }
}

impl Drop for PersistentDolt {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
