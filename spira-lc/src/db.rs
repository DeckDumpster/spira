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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::wire::Wire;

type Sets = Vec<Vec<Value>>;

/// Every write this process sent, and the transactions they committed in: with group commit
/// (group.rs) the second grows slower than the first. Read by `spira-lc stats`.
static WRITES: AtomicU64 = AtomicU64::new(0);
static COMMITS: AtomicU64 = AtomicU64::new(0);
/// Transactions that carried more than one write.
static GROUPS: AtomicU64 = AtomicU64::new(0);

/// `{"writes": n, "commits": m, "groups": g}` for this process.
pub fn write_stats() -> Value {
    serde_json::json!({
        "writes": WRITES.load(Ordering::Relaxed),
        "commits": COMMITS.load(Ordering::Relaxed),
        "groups": GROUPS.load(Ordering::Relaxed),
    })
}

thread_local! {
    static REQUEST_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// The instant after which the request this thread serves is abandoned (serve.rs sets it):
/// a write still queued for the session then is never started (law-calls-have-deadlines —
/// the receiver stops working on a request its caller has stopped waiting for).
pub fn set_request_deadline(at: Option<Instant>) {
    REQUEST_DEADLINE.with(|d| d.set(at));
}

/// One caller's write, queued for the session; the thread that runs its group fills `result`.
struct Pending {
    script: String,
    deadline: Option<Instant>,
    result: Mutex<Option<Result<Sets, ScriptFailure>>>,
}

impl Pending {
    fn answer(&self, r: Result<Sets, ScriptFailure>) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    }

    fn take(&self) -> Option<Result<Sets, ScriptFailure>> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// The writes waiting for the session. `window` is how long a write that finds no other
/// waiting holds the session open for company: `None` in a one-shot CLI, which never has any.
#[derive(Default)]
struct WriteQueue {
    waiting: Mutex<Vec<Arc<Pending>>>,
    window: Option<Duration>,
}

pub const READ_POOL: usize = 4;

pub struct Conn {
    pub host: String,
    pub port: u16,
    pub user: String,
    password: Mutex<String>,
    password_file: Option<String>,
    pub database: String,
    /// The one live connection, opened on first use and dropped on any failure: after an
    /// error a transaction may still be open, so the next call starts from a fresh session.
    session: Mutex<Option<Wire>>,
    /// Connections for plain reads, so a slow `list` never holds the write session nor the
    /// other readers: a read takes the first free one.
    read_sessions: [Mutex<Option<Wire>>; READ_POOL],
    read_turn: std::sync::atomic::AtomicUsize,
    /// Writes waiting for `session`, committed together (group.rs).
    writes: WriteQueue,
    /// The socket read/write limit for this connection: the query deadline, or
    /// [`ADMIN_IO_TIMEOUT`] for the admin batch verbs.
    pub io_timeout: std::time::Duration,
}

/// A query's server-side deadline: a statement that has not answered in this long is
/// abandoned and reported as [`DEADLINE_MESSAGE`], never as a partial answer.
pub const QUERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10); // batch-job: queries queue behind a loaded Dolt in the round VM
pub const DEADLINE_MESSAGE: &str = "deadline";

/// The admin batch verbs' socket limit (admin-apply-ddl, admin-migrate): one-off install
/// DDL on a fresh Dolt server under load ran past the query deadline ("Resource temporarily
/// unavailable", sp-4o5um). Bounded, and never used for a query.
// batch-job: install-time schema DDL, bounded at 120 s; not a query on any serving path.
pub const ADMIN_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// census's aggregate over the whole fact history, windowed per class: not a point query.
// batch-job: census report, bounded at 30 s.
pub const FACTS_QUERY_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A connect that fails is retried this many times in all: nothing has been sent, so a
/// retry cannot double a write.
const CONNECT_ATTEMPTS: u32 = 3;

/// The server answered and said no (no such database, bad credentials): asking again, after a
/// backoff, gets the same answer, and a fresh store's first DDL connect asks for a database
/// that does not exist yet.
fn refused_for_good(e: &ScriptFailure) -> bool {
    matches!(e, ScriptFailure::CannotTell(m) if m.starts_with("Error 1049 ") || m.starts_with("Error 1045 "))
}

/// Error 1049: the database named at connect does not exist on the server.
fn unknown_database(e: &ScriptFailure) -> bool {
    matches!(e, ScriptFailure::CannotTell(m) if m.contains("1049") || m.contains("database not found"))
}

fn refused_login(e: &ScriptFailure) -> bool {
    matches!(e, ScriptFailure::CannotTell(m) if m.starts_with("Error 1045 "))
}

/// The password now in `file`, when it differs from the one in use.
fn reloaded_password(file: &str, current: &str, read: impl Fn(&str) -> std::io::Result<String>) -> Option<String> {
    let fresh = read(file).ok()?.trim().to_string();
    (!fresh.is_empty() && fresh != current).then_some(fresh)
}

fn connect_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(200u64 << (attempt - 1).min(6))
}

/// The socket limit a verb's connection uses.
pub fn io_timeout_for(verb: &str) -> std::time::Duration {
    match verb {
        "admin-apply-ddl" | "admin-migrate" | "admin-grants" => ADMIN_IO_TIMEOUT,
        "facts-query" => FACTS_QUERY_IO_TIMEOUT,
        _ => QUERY_DEADLINE,
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
    spira_config::vtime::now_epoch() as i64
}

impl Conn {
    pub fn from_env() -> Result<Self, DbError> {
        let host = std::env::var("SPIRA_LC_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port: u16 = std::env::var("SPIRA_LC_PORT")
            .unwrap_or_else(|_| "3307".to_string())
            .parse()
            .map_err(|e| DbError::CannotTell(format!("SPIRA_LC_PORT: {e}")))?;
        let user = std::env::var("SPIRA_LC_USER").unwrap_or_else(|_| "spira_lc".to_string());
        // SPIRA_LC_PASSWORD_FILE is declared config (spira/conf.d): the one source is
        // $SPIRA_TOML (per Ryan 2026-10-05), never this process's own environment. The
        // credential itself still comes from reading that file's contents, same as always.
        let password_file = spira_config::process::cfg("SPIRA_LC_PASSWORD_FILE").ok();
        let password = password_from(password_file.clone(), std::env::var("SPIRA_LC_PASSWORD").ok(), |p| std::fs::read_to_string(p))
            .map_err(DbError::CannotTell)?;
        let database = std::env::var("SPIRA_LC_DB").unwrap_or_else(|_| "spira_lifecycle".to_string());
        Ok(Conn { host, port, user, password: Mutex::new(password), password_file: password_file.filter(|p| !p.is_empty()), database, session: Mutex::new(None), read_sessions: std::array::from_fn(|_| Mutex::new(None)), read_turn: std::sync::atomic::AtomicUsize::new(0), writes: WriteQueue::default(), io_timeout: QUERY_DEADLINE })
    }

    /// A second connection to the same server and database under other credentials — the
    /// database admin that admin-migrate applies a pending migration as (sp-p1z81), while
    /// this one (the lifecycle service user) only probes. Lazy: nothing connects until used.
    pub fn as_user(&self, user: String, password: String) -> Conn {
        Conn {
            host: self.host.clone(),
            port: self.port,
            user,
            password: Mutex::new(password),
            password_file: None,
            database: self.database.clone(),
            session: Mutex::new(None),
            read_sessions: std::array::from_fn(|_| Mutex::new(None)), read_turn: std::sync::atomic::AtomicUsize::new(0),
            writes: WriteQueue::default(),
            io_timeout: self.io_timeout,
        }
    }

    /// A read-only connection to the beads database: `user` logs in with the secret `password_file`
    /// holds. Lazy: nothing connects until the first query.
    pub fn reader(host: &str, port: u16, user: &str, password_file: &str, database: &str) -> Result<Conn, String> {
        let password = password_from(Some(password_file.to_string()), None, |p| std::fs::read_to_string(p))?;
        Ok(Conn {
            host: host.to_string(),
            port,
            user: user.to_string(),
            password: Mutex::new(password),
            password_file: Some(password_file.to_string()).filter(|p| !p.is_empty()),
            database: database.to_string(),
            session: Mutex::new(None),
            read_sessions: std::array::from_fn(|_| Mutex::new(None)),
            read_turn: std::sync::atomic::AtomicUsize::new(0),
            writes: WriteQueue::default(),
            io_timeout: QUERY_DEADLINE,
        })
    }

    /// A connection to a port nothing listens on: whatever answers through it was not read from Dolt.
    #[cfg(test)]
    pub fn unreachable() -> Conn {
        Conn {
            host: "127.0.0.1".into(),
            port: 1,
            user: String::new(),
            password: Mutex::new(String::new()),
            password_file: None,
            database: String::new(),
            session: Mutex::new(None),
            read_sessions: std::array::from_fn(|_| Mutex::new(None)), read_turn: std::sync::atomic::AtomicUsize::new(0),
            writes: WriteQueue::default(),
            io_timeout: std::time::Duration::from_millis(200),
        }
    }

    /// Turns on group commit's window (group.rs); call once, before the connection is shared.
    /// Without it writes still queue, but a lone write never waits for company.
    pub fn enable_group_commit(&mut self) {
        self.writes.window = Some(crate::group::WINDOW);
    }

    fn connect(&self, database: Option<&str>) -> Result<Wire, ScriptFailure> {
        let mut failure = None;
        for attempt in 0..CONNECT_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(connect_backoff(attempt));
            }
            match self.connect_once(database) {
                Ok(w) => return Ok(w),
                Err(e) if refused_for_good(&e) => return Err(e),
                Err(e) => failure = Some(e),
            }
        }
        Err(failure.expect("CONNECT_ATTEMPTS is at least one"))
    }

    /// One login. A long-lived process holds the password it read at startup, so a rotation
    /// would lock it out until restarted: on a refusal the credential file is read again and,
    /// if it now holds a different password, the login is retried with it and it is kept.
    fn connect_once(&self, database: Option<&str>) -> Result<Wire, ScriptFailure> {
        let current = self.password.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut first = Wire::connect_with(&self.host, self.port, &self.user, &current, database, self.io_timeout);
        // The connection serves planning reads, which name their database in the query: a server
        // with no lifecycle database is opened without a default one rather than refused.
        if database.is_some() && first.as_ref().is_err_and(unknown_database) {
            first = Wire::connect_with(&self.host, self.port, &self.user, &current, None, self.io_timeout);
        }
        match (&first, self.password_file.as_deref()) {
            (Err(e), Some(file)) if refused_login(e) => {
                let Some(fresh) = reloaded_password(file, &current, |p| std::fs::read_to_string(p)) else { return first };
                let retry = Wire::connect_with(&self.host, self.port, &self.user, &fresh, database, self.io_timeout);
                if retry.is_ok() {
                    *self.password.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
                }
                retry
            }
            _ => first,
        }
    }

    fn read_slot(&self) -> &Mutex<Option<Wire>> {
        if let Some(free) = self.read_sessions.iter().find(|m| m.try_lock().is_ok()) {
            return free;
        }
        let turn = self.read_turn.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        &self.read_sessions[turn % READ_POOL]
    }

    /// A single read-only query: the rows of its last result set.
    pub fn query(&self, sql: &str) -> Result<Vec<Value>, DbError> {
        match self.run_script_on(self.read_slot(), sql) {
            Ok(sets) => Ok(sets.into_iter().last().unwrap_or_default()),
            Err(ScriptFailure::LostRace) => unreachable!("a plain SELECT never conflicts"),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// Run a write script, returning each result set's rows in script order (statements that
    /// return no rows contribute none). See [`Self::write`].
    fn run_script(&self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        self.write(script)
    }

    /// Every write goes through here (group commit, group.rs): queue it, then take the write
    /// session. Whoever holds the session runs every write waiting at that moment as one
    /// transaction and hands each caller its own answer, so a caller that gets the session
    /// may find its write already done by the holder before it. Writes that arrive while a
    /// commit is in flight form the next group; when serving, a write that finds nothing else
    /// waiting holds the session open for the group window first.
    fn write(&self, script: &str) -> Result<Sets, ScriptFailure> {
        let deadline = REQUEST_DEADLINE.with(|d| d.get());
        let me = Arc::new(Pending { script: script.to_string(), deadline, result: Mutex::new(None) });
        self.writes.waiting.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::clone(&me));
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(answer) = me.take() {
                return answer;
            }
            if let Some(window) = self.writes.window {
                if self.writes.waiting.lock().unwrap_or_else(|e| e.into_inner()).len() == 1 {
                    std::thread::sleep(window);
                }
            }
            let batch: Vec<Arc<Pending>> = {
                let mut waiting = self.writes.waiting.lock().unwrap_or_else(|e| e.into_inner());
                let n = waiting.len().min(crate::group::MAX_GROUP);
                waiting.drain(..n).collect()
            };
            self.run_batch(&mut guard, batch);
        }
    }

    /// Runs `batch` on the held write session: one transaction when every member is a whole
    /// transaction (group.rs), else each alone as before. A member whose request deadline has
    /// passed is never started.
    fn run_batch(&self, guard: &mut MutexGuard<'_, Option<Wire>>, batch: Vec<Arc<Pending>>) {
        let now = Instant::now();
        let (members, expired): (Vec<_>, Vec<_>) = batch.into_iter().partition(|m| m.deadline.is_none_or(|d| d > now));
        for m in expired {
            m.answer(Err(ScriptFailure::CannotTell(format!("{DEADLINE_MESSAGE}: the write was still queued for the session and was never started"))));
        }
        WRITES.fetch_add(members.len() as u64, Ordering::Relaxed);
        let bodies: Option<Vec<&str>> = members.iter().map(|m| crate::group::txn_body(&m.script)).collect();
        let bodies = match bodies {
            Some(b) if b.len() > 1 => b,
            _ => {
                for m in &members {
                    m.answer(self.run_alone(guard, m));
                }
                return;
            }
        };
        let session_started = Instant::now();
        #[allow(clippy::manual_filter)]
        let reusable = guard.take().and_then(|mut w| if w.idle_too_long() && !w.ping() { None } else { Some(w) });
        let mut wire = match reusable.map(Ok).unwrap_or_else(|| self.connect(Some(&self.database))) {
            Ok(w) => w,
            Err(e) => {
                let why = match e {
                    ScriptFailure::CannotTell(m) => m,
                    ScriptFailure::LostRace => "serialization conflict on connect".to_string(),
                };
                for m in &members {
                    m.answer(Err(ScriptFailure::CannotTell(why.clone())));
                }
                return;
            }
        };
        crate::slow::record(session_started.elapsed(), "CONNECT");
        let started = Instant::now();
        let outcome = crate::group::run(&bodies, |sql| wire.exec(sql));
        crate::slow::record(started.elapsed(), &format!("GROUP COMMIT of {}", members.len()));
        match outcome {
            crate::group::Group::Committed(outs) => {
                COMMITS.fetch_add(1, Ordering::Relaxed);
                GROUPS.fetch_add(1, Ordering::Relaxed);
                **guard = Some(wire);
                for (m, out) in members.iter().zip(outs) {
                    m.answer(Ok(out));
                }
            }
            crate::group::Group::Retry { drop_session } => {
                if !drop_session {
                    **guard = Some(wire);
                }
                for m in &members {
                    m.answer(self.run_alone(guard, m));
                }
            }
            crate::group::Group::Unknown(why) => {
                for m in &members {
                    m.answer(Err(ScriptFailure::CannotTell(why.clone())));
                }
            }
        }
    }

    /// One write on its own, exactly as every write ran before group commit.
    fn run_alone(&self, guard: &mut MutexGuard<'_, Option<Wire>>, m: &Pending) -> Result<Sets, ScriptFailure> {
        COMMITS.fetch_add(1, Ordering::Relaxed);
        self.run_locked(guard, &m.script)
    }

    fn run_script_on(&self, session: &Mutex<Option<Wire>>, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        let mut guard = session.lock().unwrap_or_else(|e| e.into_inner());
        self.run_locked(&mut guard, script)
    }

    /// Times only the statements: the wait for the session lock is queueing, not a slow query.
    fn run_locked(&self, guard: &mut MutexGuard<'_, Option<Wire>>, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        let session_started = std::time::Instant::now();
        #[allow(clippy::manual_filter)]
        let reusable = guard.take().and_then(|mut w| if w.idle_too_long() && !w.ping() { None } else { Some(w) });
        let wire = reusable.map(Ok).unwrap_or_else(|| self.connect(Some(&self.database)));
        crate::slow::record(session_started.elapsed(), "CONNECT");
        match wire {
            Ok(mut wire) => {
                let started = std::time::Instant::now();
                let out = wire.exec(script);
                let keep = out.is_ok();
                crate::slow::record(started.elapsed(), script);
                if keep {
                    **guard = Some(wire);
                }
                out
            }
            Err(e) => Err(e),
        }
    }

    /// Append one already-built event INSERT in its own transaction (a fact: no row is mutated).
    pub fn append_event(&self, insert_sql: &str) -> Result<(), DbError> {
        match self.run_script(&format!("START TRANSACTION;\n{insert_sql};\nCOMMIT;\n")) {
            Ok(_) => Ok(()),
            Err(ScriptFailure::LostRace) => Err(DbError::CannotTell("a plain INSERT lost a race — unexpected".into())),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// Append many already-built event INSERTs in one transaction over this one connection:
    /// a machine's fine-grained transitions in a pass cost one commit, not one per transition.
    /// Empty input writes nothing.
    pub fn append_events(&self, insert_sqls: &[String]) -> Result<usize, DbError> {
        let Some(script) = batch_script(insert_sqls) else { return Ok(0) };
        match self.run_script(&script) {
            Ok(_) => Ok(insert_sqls.len()),
            Err(ScriptFailure::LostRace) => Err(DbError::CannotTell("a plain INSERT batch lost a race — unexpected".into())),
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
        // One statement per round trip: the server's own multi-statement splitter refuses a
        // view body with an optimizer hint ("unable to get sub statement").
        let result = wire.and_then(|mut wire| {
            let mut last = Ok(Vec::new());
            for stmt in crate::migrate::split_statements(sql_text) {
                last = wire.exec(&stmt);
                if last.is_err() {
                    break;
                }
            }
            last
        });
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

}

pub(crate) enum ScriptFailure {
    /// COMMIT reported a serialization conflict (SQLSTATE 40001): the whole transaction,
    /// including any event row it tried to insert, was discarded by the server.
    LostRace,
    CannotTell(String),
}

fn batch_script(insert_sqls: &[String]) -> Option<String> {
    if insert_sqls.is_empty() {
        return None;
    }
    let mut script = String::from("START TRANSACTION;\n");
    for sql in insert_sqls {
        script.push_str(sql.trim_end_matches(|c: char| c == ';' || c.is_whitespace()));
        script.push_str(";\n");
    }
    script.push_str("COMMIT;\n");
    Some(script)
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
        assert_eq!(password_from(Some("c".into()), Some("pw".into()), |_| Ok("secret\n".into())), Ok("secret".into()));
    }

    #[test]
    fn an_unreadable_named_file_is_an_error_not_a_blank_password() {
        assert!(password_from(Some("missing".into()), None, no_read).is_err());
    }
}

#[cfg(test)]
mod io_timeout_tests {
    #[test]
    fn a_refusal_from_the_server_is_not_retried_but_a_dead_socket_is() {
        let no_db = ScriptFailure::CannotTell("Error 1049 (42000): Unknown database 'spira_lifecycle'".into());
        let denied = ScriptFailure::CannotTell("Error 1045 (28000): Access denied for user 'spira_lc'".into());
        let refused = ScriptFailure::CannotTell("connecting to 127.0.0.1:1: Connection refused (os error 111)".into());
        assert!(refused_for_good(&no_db) && refused_for_good(&denied));
        assert!(!refused_for_good(&refused), "positive control: a transport failure is still retried");
        assert!(!refused_for_good(&ScriptFailure::LostRace));
    }

    #[test]
    fn a_rotated_credential_file_is_picked_up_and_an_unchanged_one_is_not() {
        let rotated = |_: &str| Ok("new\n".to_string());
        assert_eq!(reloaded_password("f", "old", rotated), Some("new".into()));
        assert_eq!(reloaded_password("f", "new", rotated), None, "same password: retrying cannot help");
        assert_eq!(reloaded_password("f", "old", |_| Ok("\n".into())), None);
        assert_eq!(reloaded_password("f", "old", |_| Err(std::io::ErrorKind::NotFound.into())), None);
        assert!(refused_login(&ScriptFailure::CannotTell("Error 1045 (28000): Access denied".into())));
        assert!(!refused_login(&ScriptFailure::CannotTell("Error 1049 (42000): Unknown database".into())));
    }

    #[test]
    fn connect_backoff_doubles_from_200ms() {
        assert_eq!(super::connect_backoff(1), std::time::Duration::from_millis(200));
        assert_eq!(super::connect_backoff(2), std::time::Duration::from_millis(400));
    }

    use super::*;
    #[test]
    fn only_the_admin_batch_verbs_get_the_long_socket_limit() {
        assert_eq!(io_timeout_for("admin-apply-ddl"), ADMIN_IO_TIMEOUT);
        assert_eq!(io_timeout_for("admin-migrate"), ADMIN_IO_TIMEOUT);
        for v in ["show", "event", "history", "create-bead", "list", "serve", ""] {
            assert_eq!(io_timeout_for(v), QUERY_DEADLINE, "{v} keeps the query deadline");
        }
        assert!(ADMIN_IO_TIMEOUT <= std::time::Duration::from_secs(120), "bounded");
        assert_eq!(io_timeout_for("facts-query"), FACTS_QUERY_IO_TIMEOUT);
        assert_eq!(io_timeout_for("facts"), QUERY_DEADLINE);
    }
}

#[cfg(test)]
mod batch_tests {
    use super::batch_script;

    #[test]
    fn n_inserts_share_one_transaction() {
        let s = batch_script(&["INSERT INTO event VALUES (1);".to_string(), "INSERT INTO event VALUES (2)".to_string()]).unwrap();
        assert_eq!(s, "START TRANSACTION;\nINSERT INTO event VALUES (1);\nINSERT INTO event VALUES (2);\nCOMMIT;\n");
        assert_eq!(s.matches("START TRANSACTION").count(), 1);
        assert_eq!(s.matches("COMMIT").count(), 1);
    }

    #[test]
    fn an_empty_batch_writes_nothing() {
        assert_eq!(batch_script(&[]), None);
    }
}

#[cfg(test)]
mod unknown_database_tests {
    use super::*;

    #[test]
    fn only_a_missing_database_is_retried_without_one() {
        let missing = ScriptFailure::CannotTell("Error 1049 (HY000): database not found: spira_lifecycle".into());
        let denied = ScriptFailure::CannotTell("Error 1045 (28000): Access denied for user".into());
        assert!(unknown_database(&missing));
        assert!(!unknown_database(&denied), "the control: a refused login is not a missing database");
        assert!(!unknown_database(&ScriptFailure::LostRace));
    }
}
