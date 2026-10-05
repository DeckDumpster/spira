//! The DB layer: everything in this file is I/O, on purpose, so nothing in the `lifecycle`
//! crate has to be. It speaks the MySQL protocol to Dolt's `sql-server` itself (`wire.rs`)
//! over one reused connection; forking the `dolt` CLI per query cost a process start on
//! every lifecycle call.
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

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::wire::Wire;

pub struct Conn {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    /// The one live connection, opened on first use and dropped on any failure: after an
    /// error a transaction may still be open, so the next call starts from a fresh session.
    session: Mutex<Option<Wire>>,
    /// The socket read/write limit for this connection: the 5 s query cap, or
    /// [`ADMIN_IO_TIMEOUT`] for the admin batch verbs.
    pub io_timeout: std::time::Duration,
}

/// The admin batch verbs' socket limit (admin-apply-ddl, admin-migrate): one-off install
/// DDL on a fresh Dolt server under load ran past the 5 s query cap ("Resource temporarily
/// unavailable", sp-4o5um). Bounded, and never used for a query.
// batch-job: install-time schema DDL, bounded at 120 s; not a query on any serving path.
pub const ADMIN_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The socket limit a verb's connection uses.
pub fn io_timeout_for(verb: &str) -> std::time::Duration {
    match verb {
        "admin-apply-ddl" | "admin-migrate" => ADMIN_IO_TIMEOUT,
        _ => std::time::Duration::from_secs(5),
    }
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
        let host = std::env::var("SPIRA_LC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port: u16 = std::env::var("SPIRA_LC_PORT")
            .unwrap_or_else(|_| "3307".to_string())
            .parse()
            .map_err(|e| DbError::CannotTell(format!("SPIRA_LC_PORT: {e}")))?;
        let user = std::env::var("SPIRA_LC_USER").unwrap_or_else(|_| "spira_lc".to_string());
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let password = password_from(Some(spira_config::resolve::lc_password_file(&env)), std::env::var("SPIRA_LC_PASSWORD").ok(), |p| std::fs::read_to_string(p))
            .map_err(DbError::CannotTell)?;
        let database = std::env::var("SPIRA_LC_DB").unwrap_or_else(|_| "spira_lifecycle".to_string());
        Ok(Conn { host, port, user, password, database, session: Mutex::new(None), io_timeout: std::time::Duration::from_secs(5) })
    }

    fn connect(&self, database: Option<&str>) -> Result<Wire, ScriptFailure> {
        Wire::connect_with(&self.host, self.port, &self.user, &self.password, database, self.io_timeout)
    }

    /// A single read-only query: the rows of its last result set.
    pub fn query(&self, sql: &str) -> Result<Vec<Value>, DbError> {
        match self.run_script(sql) {
            Ok(sets) => Ok(sets.into_iter().last().unwrap_or_default()),
            Err(ScriptFailure::LostRace) => unreachable!("a plain SELECT never conflicts"),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// Run a script, returning each result set's rows in script order (statements that
    /// return no rows contribute none). Reuses this `Conn`'s session, pinging first when it
    /// has sat idle so a connection the server closed is replaced instead of failing a write.
    fn run_script(&self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        let started = std::time::Instant::now();
        let result = self.run_script_timed(script);
        crate::slow::record(started.elapsed(), script);
        result
    }

    fn run_script_timed(&self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let reusable = guard.take().and_then(|mut w| if w.idle_too_long() && !w.ping() { None } else { Some(w) });
        let mut wire = match reusable {
            Some(w) => w,
            None => self.connect(Some(&self.database))?,
        };
        let out = wire.exec(script);
        if out.is_ok() {
            *guard = Some(wire);
        }
        out
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
            Ok(sets) => {
                let rows = sets.last().cloned().unwrap_or_default();
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

    /// The migration classifier's own write: insert a fresh row plus its event, in one
    /// transaction, but only if no row for this key exists yet. There is no prior row to
    /// compare-and-swap against — the classifier assigns an initial state from legacy
    /// evidence rather than applying a transition — so `INSERT IGNORE` (refused by the
    /// primary key alone) plays the role `WHERE version = ?` plays in
    /// [`Self::cas_update_and_log`], and this is exactly what makes re-running the
    /// classifier a no-op: a bead already classified is already a row here.
    pub fn insert_if_absent_and_log(
        &self,
        table: &str,
        insert_columns: &str,
        insert_values: &str,
        ev: &EventRecord,
        to_state: &str,
    ) -> Result<bool, DbError> {
        let script = format!(
            "START TRANSACTION;\n\
             INSERT IGNORE INTO {table} ({insert_columns}) VALUES ({insert_values});\n\
             INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at)\n\
             SELECT {machine}, {lc_key}, {event}, {expect}, {from_state},\n\
                    IF(ROW_COUNT() = 1, {to_state}, {from_state}),\n\
                    IF(ROW_COUNT() = 1, 1, 0),\n\
                    IF(ROW_COUNT() = 1, NULL, {already_reason}),\n\
                    {evidence}, {actor}, {at};\n\
             SELECT applied FROM event WHERE seq = LAST_INSERT_ID();\n\
             COMMIT;\n",
            table = table,
            insert_columns = insert_columns,
            insert_values = insert_values,
            machine = sql_str(&ev.machine),
            lc_key = sql_str(&ev.key),
            event = sql_str(&ev.event),
            expect = sql_str(&ev.expect),
            from_state = sql_str(&ev.from_state),
            to_state = sql_str(to_state),
            already_reason = sql_str("AlreadyClassified"),
            evidence = sql_json(&ev.evidence),
            actor = sql_str(&ev.actor),
            at = ev.at,
        );

        match self.run_script(&script) {
            Ok(sets) => {
                let rows = sets.last().cloned().unwrap_or_default();
                let applied = rows.first().and_then(|r| r.get("applied")).and_then(|v| v.as_str()).map(|s| s != "0").unwrap_or(false);
                Ok(applied)
            }
            Err(ScriptFailure::LostRace) => {
                self.insert_refusal_event(ev)?;
                Ok(false)
            }
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// DDL runs with `SPIRA_LC_DB` selected when that database exists, so a file that names
    /// no database (`lifecycle/migrations/*.sql`) applies as shipped (sp-vf9iu: 0002-since.sql
    /// failed "no database selected"). On a fresh server the database does not exist yet and
    /// selecting it fails the handshake, so it falls back to no default database: `schema.sql`
    /// itself opens with `CREATE DATABASE IF NOT EXISTS spira_lifecycle; USE spira_lifecycle;`.
    pub fn apply_ddl(&self, sql_text: &str) -> Result<(), DbError> {
        let wire = self.connect(Some(&self.database)).or_else(|_| self.connect(None));
        let result = wire.and_then(|mut wire| wire.exec(sql_text));
        match result {
            Ok(_) => Ok(()),
            Err(ScriptFailure::LostRace) => Err(DbError::CannotTell("DDL reported a serialization conflict".into())),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// One or more statements that do not themselves need CAS bookkeeping — a plain INSERT,
    /// or a script this caller has already made idempotent (`... WHERE NOT EXISTS (...)`).
    /// A lost race here is a bug, not a real condition, because nothing that calls this
    /// competes with another writer on the same key by construction (row creation, keyed by
    /// an id nothing else mints).
    pub fn run_plain(&self, script: &str) -> Result<(), DbError> {
        match self.run_script(script) {
            Ok(_) => Ok(()),
            Err(ScriptFailure::LostRace) => Err(DbError::CannotTell("a plain insert unexpectedly lost a race".into())),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// The cross-machine cascade (design §3.3): every row a batch-landing/settling/
    /// abandoning touches, CAS'd and logged in one transaction. Each step is independent —
    /// no step's SQL depends on another's outcome — because Dolt's own commit-time
    /// serialization check (see this module's doc) already makes the whole script
    /// all-or-nothing against a *concurrent* cascade on the same rows: two racing cascades
    /// touch the same batch row, so one's COMMIT fails with 40001 and every row it touched,
    /// event inserts included, rolls back together. What this does not catch is one row
    /// among many going stale for an unrelated reason (its own version already moved before
    /// this cascade was built) — that row's step simply logs `applied=false` like any single
    /// CAS does, and the periodic consistency read (design Intent 5) is what notices it.
    /// `preamble` runs first, in the same transaction, before any step — the constructor
    /// inserts (a fresh batch row, its `batch_member` rows) that a cascade like `cut` needs
    /// alongside its CAS steps, with no separate round trip and no separate transaction.
    pub fn cascade(&self, preamble: &str, steps: &[CascadeStep]) -> Result<Vec<bool>, DbError> {
        let mut script = String::from("START TRANSACTION;\n");
        script.push_str(preamble);
        for step in steps {
            script.push_str(&format!(
                "UPDATE {table} SET {set_clause} WHERE {key_column} = {key_q} AND version = {old_version};\n\
                 INSERT INTO event (machine, lc_key, event, expect, from_state, to_state, applied, refusal, evidence, actor, at)\n\
                 SELECT {machine}, {lc_key}, {event}, {expect}, {from_state},\n\
                        IF(ROW_COUNT() = 1, {to_state}, {from_state}),\n\
                        IF(ROW_COUNT() = 1, 1, 0),\n\
                        IF(ROW_COUNT() = 1, NULL, {stale_reason}),\n\
                        {evidence}, {actor}, {at};\n\
                 SELECT applied FROM event WHERE seq = LAST_INSERT_ID();\n",
                table = step.table,
                key_column = step.key_column,
                key_q = sql_str(&step.key),
                old_version = step.old_version,
                set_clause = step.set_clause,
                machine = sql_str(&step.event.machine),
                lc_key = sql_str(&step.event.key),
                event = sql_str(&step.event.event),
                expect = sql_str(&step.event.expect),
                from_state = sql_str(&step.event.from_state),
                to_state = sql_str(&step.applied_to_state),
                stale_reason = sql_str("StaleVersion"),
                evidence = sql_json(&step.event.evidence),
                actor = sql_str(&step.event.actor),
                at = step.event.at,
            ));
        }
        script.push_str("COMMIT;\n");

        match self.run_script(&script) {
            Ok(blocks) => {
                Ok(steps
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        blocks
                            .get(i)
                            .and_then(|r| r.first())
                            .and_then(|r| r.get("applied"))
                            .and_then(|v| v.as_str())
                            .map(|s| s != "0")
                            .unwrap_or(false)
                    })
                    .collect())
            }
            Err(ScriptFailure::LostRace) => {
                for step in steps {
                    self.insert_refusal_event(&step.event)?;
                }
                Ok(vec![false; steps.len()])
            }
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }
}

/// One row's CAS-update-and-log inside a `cascade`. Shaped like `cas_update_and_log`'s own
/// arguments because it is the same primitive, just assembled N times into one script
/// instead of one script each — see `cascade`'s doc for why that is enough.
pub struct CascadeStep {
    pub table: &'static str,
    pub key_column: &'static str,
    pub key: String,
    pub old_version: u64,
    pub set_clause: String,
    pub applied_to_state: String,
    pub event: EventRecord,
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

/// The password a connection uses: the file named by SPIRA_LC_PASSWORD_FILE when that is set
/// AND non-empty, else SPIRA_LC_PASSWORD. conf.sh exports SPIRA_LC_PASSWORD_FILE empty when no
/// credential file exists (sp-9c2o5), and reading a file named "" must not shadow the password.
pub(crate) fn password_from(
    file: Option<String>,
    password: Option<String>,
    read: impl Fn(&str) -> std::io::Result<String>,
) -> Result<String, String> {
    match file.filter(|p| !p.is_empty()) {
        Some(path) => Ok(read(&path).map_err(|e| format!("reading {path}: {e}"))?.trim().to_string()),
        None => Ok(password.unwrap_or_default()),
    }
}

#[cfg(test)]
mod password_tests {
    use super::password_from;

    fn no_read(_: &str) -> std::io::Result<String> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "read must not be called"))
    }

    #[test]
    fn an_empty_password_file_falls_back_to_the_password() {
        assert_eq!(password_from(Some(String::new()), Some("pw".into()), no_read), Ok("pw".into()));
    }

    #[test]
    fn a_named_password_file_wins_and_is_trimmed() {
        assert_eq!(password_from(Some("/c".into()), Some("pw".into()), |_| Ok("secret\n".into())), Ok("secret".into()));
    }

    #[test]
    fn an_unreadable_named_file_is_an_error_not_a_blank_password() {
        assert!(password_from(Some("/missing".into()), None, no_read).is_err());
    }
}

#[cfg(test)]
mod io_timeout_tests {
    use super::*;
    #[test]
    fn only_the_admin_batch_verbs_get_the_long_socket_limit() {
        assert_eq!(io_timeout_for("admin-apply-ddl"), ADMIN_IO_TIMEOUT);
        assert_eq!(io_timeout_for("admin-migrate"), ADMIN_IO_TIMEOUT);
        for v in ["show", "event", "history", "create-bead", "list", "serve", ""] {
            assert_eq!(io_timeout_for(v), std::time::Duration::from_secs(5), "{v} keeps the 5 s query cap");
        }
        assert!(ADMIN_IO_TIMEOUT <= std::time::Duration::from_secs(120), "bounded");
    }
}
