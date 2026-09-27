//! The DB layer: everything in this file is I/O, on purpose, so nothing in the `lifecycle`
//! crate has to be. It shells out to the `dolt` binary rather than embedding a MySQL driver
//! (`law-prefer-the-real-dependency`): Dolt is already the harness's SQL engine, its CLI
//! already speaks the wire protocol to a remote `sql-server`, and there is no in-process
//! driver anywhere else in this workspace to be consistent with.
//!
//! CONCURRENCY, AS MEASURED (2026-09-26, against a throwaway `dolt sql-server` 2.2.3):
//! two overlapping transactions racing an `UPDATE ... WHERE version = ?` do not both see
//! zero rows affected — Dolt runs snapshot isolation and one COMMIT succeeds while the
//! other fails at commit time with `Error 1213 (40001): serialization failure`, rolling
//! back everything in that transaction, including any event row it meant to insert. So a
//! lost race is detected two different ways, and both are handled:
//!
//!   - the loser's UPDATE silently matches zero rows (its own read was already stale before
//!     it started) — caught in-transaction via `IF(ROW_COUNT() = 1, ...)` on the event insert;
//!   - the loser's UPDATE matched when it ran, but the other side committed first — caught
//!     as a serialization failure at COMMIT, which discards the whole transaction, so the
//!     refusal is recorded in a second, plain (non-conflicting) INSERT.
//!
//! Either way exactly one transition is ever recorded as applied for a given (key, version).

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::persistent::PersistentDolt;

pub struct Conn {
    pub dolt_bin: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub data_dir: String,
    /// `Some` only for a `Conn` built with `persistent_session()` — the `serve` daemon's
    /// own long-lived connection. `None` is the one-shot CLI path: correct, but paying a
    /// fresh `dolt` process start and handshake on every call (measured 150-230ms each,
    /// which is why the p99 < 50ms bench runs through the persistent path instead).
    session: Option<Mutex<Option<PersistentDolt>>>,
}

// The String is surfaced only through the derived `Debug` impl (every caller prints the
// error with `{e:?}`), which rustc's dead-code analysis does not count as a read.
#[derive(Debug)]
#[allow(dead_code)]
pub enum DbError {
    /// The dolt binary is missing, the server is unreachable, or its output could not be
    /// parsed. The caller cannot tell whether the transition applied — exit code 2.
    CannotTell(String),
}

pub fn now_epoch() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

impl Conn {
    pub fn from_env() -> Result<Self, DbError> {
        let dolt_bin = std::env::var("SPIRA_LC_DOLT_BIN").unwrap_or_else(|_| "dolt".to_string());
        let host = std::env::var("SPIRA_LC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port: u16 = std::env::var("SPIRA_LC_PORT")
            .unwrap_or_else(|_| "3307".to_string())
            .parse()
            .map_err(|e| DbError::CannotTell(format!("SPIRA_LC_PORT: {e}")))?;
        let user = std::env::var("SPIRA_LC_USER").unwrap_or_else(|_| "spira_lc".to_string());
        let password = match std::env::var("SPIRA_LC_PASSWORD_FILE") {
            Ok(path) => std::fs::read_to_string(&path)
                .map_err(|e| DbError::CannotTell(format!("reading {path}: {e}")))?
                .trim()
                .to_string(),
            Err(_) => std::env::var("SPIRA_LC_PASSWORD").unwrap_or_default(),
        };
        let database = std::env::var("SPIRA_LC_DB").unwrap_or_else(|_| "spira_lifecycle".to_string());
        // dolt's remote client mode still resolves a local data-dir for bookkeeping even
        // when --host/--port point elsewhere; an unset or vanished cwd makes it fail with
        // "failed to load database names" before it ever opens the connection.
        let data_dir = std::env::var("SPIRA_LC_DATA_DIR").unwrap_or_else(|_| "/tmp".to_string());
        Ok(Conn { dolt_bin, host, port, user, password, database, data_dir, session: None })
    }

    /// The same connection parameters, but backed by one persistent `dolt` session reused
    /// across every call instead of a fresh process per call. Only `serve` should hold one
    /// of these — a one-shot CLI invocation gets no benefit from a connection that dies
    /// with the process anyway.
    pub fn persistent_session() -> Result<Self, DbError> {
        let mut conn = Self::from_env()?;
        conn.session = Some(Mutex::new(None));
        Ok(conn)
    }

    fn command(&self) -> Command {
        let mut cmd = self.command_without_db();
        cmd.args(["--use-db", &self.database]);
        cmd
    }

    /// Without `--use-db`: DDL that creates the database itself (schema.sql opens with
    /// `CREATE DATABASE IF NOT EXISTS`) must connect before that database exists.
    fn command_without_db(&self) -> Command {
        let mut cmd = Command::new(&self.dolt_bin);
        cmd.args([
            "--data-dir",
            &self.data_dir,
            "--host",
            &self.host,
            "--port",
            &self.port.to_string(),
            "-u",
            &self.user,
            "-p",
            &self.password,
            "--no-tls",
        ]);
        cmd
    }

    /// A single read-only query. Returns the parsed `rows` array from `-r json`.
    pub fn query(&self, sql: &str) -> Result<Vec<serde_json::Value>, DbError> {
        let sql = if sql.trim_end().ends_with(';') { sql.to_string() } else { format!("{sql};") };
        let text = match self.run_script(&sql) {
            Ok(text) => text,
            Err(ScriptFailure::LostRace) => unreachable!("a plain SELECT never conflicts"),
            Err(ScriptFailure::CannotTell(e)) => return Err(DbError::CannotTell(e)),
        };
        parse_last_json_rows(&text).ok_or_else(|| DbError::CannotTell(format!("could not parse query output: {text}")))
    }

    /// Run a script (piped to stdin) with `-r json`, returning stdout on success. The
    /// caller inspects the JSON blocks it needs (e.g. the trailing SELECT) itself. Goes
    /// through the persistent session when this `Conn` has one; otherwise a fresh process.
    fn run_script(&self, script: &str) -> Result<String, ScriptFailure> {
        let Some(session) = &self.session else {
            return self.run_script_on(self.command(), script);
        };
        let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let dolt = PersistentDolt::spawn(self.command()).map_err(|e| match e {
                DbError::CannotTell(m) => ScriptFailure::CannotTell(m),
            })?;
            *guard = Some(dolt);
        }
        match guard.as_mut().unwrap().send(script) {
            Ok(text) => Ok(text),
            Err(ScriptFailure::LostRace) => Err(ScriptFailure::LostRace),
            Err(e @ ScriptFailure::CannotTell(_)) => {
                // The session's state is unknown after an unexpected failure; drop it so
                // the next call spawns a fresh one instead of reusing something broken.
                *guard = None;
                Err(e)
            }
        }
    }

    fn run_script_on(&self, mut command: Command, script: &str) -> Result<String, ScriptFailure> {
        let mut child = command
            .args(["sql", "-r", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ScriptFailure::CannotTell(format!("spawning {}: {e}", self.dolt_bin)))?;
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(script.as_bytes())
            .map_err(|e| ScriptFailure::CannotTell(format!("writing script: {e}")))?;
        let out = child.wait_with_output().map_err(|e| ScriptFailure::CannotTell(format!("waiting on {}: {e}", self.dolt_bin)))?;
        let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else if combined.contains("40001") || combined.contains("serialization failure") {
            Err(ScriptFailure::LostRace)
        } else {
            Err(ScriptFailure::CannotTell(combined))
        }
    }

    /// Insert one event row in its own transaction. Used both for logical refusals (no row
    /// mutation attempted at all) and for the fallback recording of a race lost at COMMIT.
    pub fn insert_refusal_event(&self, ev: &EventRecord) -> Result<(), DbError> {
        let script = format!(
            "START TRANSACTION;\nINSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at) VALUES ({}, {}, {}, {}, {}, {}, 0, {}, {}, {}, {});\nCOMMIT;\n",
            sql_str(&ev.machine),
            sql_str(&ev.key),
            sql_str(&ev.event),
            sql_str(&ev.expect),
            sql_str(&ev.from_state),
            sql_str(&ev.from_state),
            sql_str(ev.refusal.as_deref().unwrap_or("Refused")),
            sql_json(&ev.evidence),
            sql_str(&ev.actor),
            ev.at,
        );
        match self.run_script(&script) {
            Ok(_) => Ok(()),
            Err(ScriptFailure::LostRace) => Err(DbError::CannotTell("refusal insert itself lost a race — unexpected, inserts do not conflict".into())),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// The CAS + event insert for one row's transition, in one transaction. `set_clause` and
    /// `new_version` are the caller's pure `apply()` outcome, translated to SQL; `table` and
    /// `key_column` name the row being updated. Returns whether the DB confirms the
    /// transition actually applied (it may not, even though the caller's local `apply()`
    /// said it should, if another writer won the race first).
    #[allow(clippy::too_many_arguments)]
    pub fn cas_update_and_log(
        &self,
        table: &str,
        key_column: &str,
        key: &str,
        old_version: u64,
        set_clause: &str,
        ev: &EventRecord,
        applied_to_state: &str,
    ) -> Result<bool, DbError> {
        let script = format!(
            "START TRANSACTION;\n\
             UPDATE {table} SET {set_clause} WHERE {key_column} = {key_q} AND version = {old_version};\n\
             INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at)\n\
             SELECT {machine}, {lc_key}, {event}, {expect}, {from_state},\n\
                    IF(ROW_COUNT() = 1, {to_state}, {from_state}),\n\
                    IF(ROW_COUNT() = 1, 1, 0),\n\
                    IF(ROW_COUNT() = 1, NULL, {stale_reason}),\n\
                    {evidence}, {actor}, {at};\n\
             SELECT applied FROM event WHERE seq = LAST_INSERT_ID();\n\
             COMMIT;\n",
            table = table,
            key_column = key_column,
            key_q = sql_str(key),
            old_version = old_version,
            set_clause = set_clause,
            machine = sql_str(&ev.machine),
            lc_key = sql_str(&ev.key),
            event = sql_str(&ev.event),
            expect = sql_str(&ev.expect),
            from_state = sql_str(&ev.from_state),
            to_state = sql_str(applied_to_state),
            stale_reason = sql_str("StaleVersion"),
            evidence = sql_json(&ev.evidence),
            actor = sql_str(&ev.actor),
            at = ev.at,
        );

        match self.run_script(&script) {
            Ok(stdout) => {
                let rows = parse_last_json_rows(&stdout)
                    .ok_or_else(|| DbError::CannotTell(format!("could not parse transition output: {stdout}")))?;
                let applied = rows.first().and_then(|r| r.get("applied")).and_then(|v| v.as_str()).map(|s| s != "0").unwrap_or(false);
                Ok(applied)
            }
            Err(ScriptFailure::LostRace) => {
                // The whole transaction rolled back, event insert included. Record the
                // refusal separately: we know for certain, now, that it did not apply.
                self.insert_refusal_event(ev)?;
                Ok(false)
            }
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// DDL runs without `--use-db`, because `schema.sql` itself opens with `CREATE DATABASE
    /// IF NOT EXISTS spira_lifecycle; USE spira_lifecycle;` — the database need not exist
    /// yet when this is called, which is exactly the state it's called in on a fresh server.
    pub fn apply_ddl(&self, sql_text: &str) -> Result<(), DbError> {
        match self.run_script_on(self.command_without_db(), sql_text) {
            Ok(_) => Ok(()),
            Err(ScriptFailure::LostRace) => Err(DbError::CannotTell("DDL reported a serialization conflict".into())),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }
}

pub(crate) enum ScriptFailure {
    /// COMMIT reported a serialization conflict (SQLSTATE 40001): the whole transaction,
    /// including any event row it tried to insert, was discarded by the server.
    LostRace,
    CannotTell(String),
}

pub struct EventRecord {
    pub machine: String,
    pub key: String,
    pub event: String,
    pub expect: String,
    pub from_state: String,
    pub refusal: Option<String>,
    pub evidence: serde_json::Value,
    pub actor: String,
    pub at: i64,
}

fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn sql_json(v: &serde_json::Value) -> String {
    sql_str(&v.to_string())
}

/// `dolt sql -r json` prints one `{...}` block per statement, separated by blank lines.
/// The block this crate ever needs is the last non-empty one that carries a `rows` array.
fn parse_last_json_rows(text: &str) -> Option<Vec<serde_json::Value>> {
    let mut last_rows: Option<Vec<serde_json::Value>> = None;
    for block in text.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(block) {
            if let Some(rows) = v.get("rows").and_then(|r| r.as_array()) {
                last_rows = Some(rows.clone());
            }
        }
    }
    last_rows.or_else(|| if text.trim().is_empty() { None } else { Some(Vec::new()) })
}
