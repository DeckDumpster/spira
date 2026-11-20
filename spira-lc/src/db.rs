//! The beads database's reader, and the one-shot administrator's connection `admin-drop-lifecycle`
//! uses. Both speak the MySQL protocol to Dolt's `sql-server` themselves (`wire.rs`) over reused
//! connections; forking the `dolt` CLI per query cost a process start on every call. The
//! lifecycle machine itself never touches this: it lives in the control store.

use std::sync::{Condvar, Mutex};

use serde_json::Value;

use crate::wire::Wire;

/// A query's server-side deadline: a statement that has not answered in this long is
/// abandoned and reported as [`DEADLINE_MESSAGE`], never as a partial answer.
pub const QUERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10); // batch-job: queries queue behind a loaded Dolt in the round VM
pub const DEADLINE_MESSAGE: &str = "deadline";

/// The administrator's socket limit: dropping a database on a Dolt under load outlasts a query.
// batch-job: one-off admin statement, bounded at 120 s; not a query on any serving path.
pub const ADMIN_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The socket limit of a collection, which answers only when the whole live store is rewritten.
pub const GC_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4 * 3600); // batch-job: a gc rewrites the whole live store

/// How many sessions a reader connection holds open at most: a read waits for another only
/// when all of them are busy.
pub const READER_SESSIONS: usize = 4;

/// A connect that fails is retried this many times in all: nothing has been sent, so a
/// retry cannot double a write.
const CONNECT_ATTEMPTS: u32 = 3;

/// The server answered and said no (no such database, bad credentials): asking again, after a
/// backoff, gets the same answer.
fn refused_for_good(e: &ScriptFailure) -> bool {
    matches!(e, ScriptFailure::CannotTell(m) if m.starts_with("Error 1049 ") || m.starts_with("Error 1045 "))
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

// The String is surfaced only through the derived `Debug` impl (every caller prints the
// error with `{e:?}`), which rustc's dead-code analysis does not count as a read.
#[derive(Debug)]
#[allow(dead_code)]
pub enum DbError {
    /// The server is unreachable, or its output could not be parsed. The caller cannot tell
    /// whether a statement applied — exit code 2.
    CannotTell(String),
}

pub fn now_epoch() -> i64 {
    spira_config::vtime::now_epoch() as i64
}

pub struct Conn {
    pub host: String,
    pub port: u16,
    pub user: String,
    password: Mutex<String>,
    password_file: Option<String>,
    pub database: String,
    /// Live sessions, opened on demand and dropped on any failure: after an error a
    /// transaction may still be open, so the next call starts from a fresh session.
    sessions: Sessions<Wire>,
    /// The socket read/write limit for this connection.
    pub io_timeout: std::time::Duration,
}

impl Conn {
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
            sessions: Sessions::new(READER_SESSIONS),
            io_timeout: QUERY_DEADLINE,
        })
    }

    /// A read-write connection to the beads database, logging in as the writer.
    pub fn writer(host: &str, port: u16, user: &str, password_file: &str, database: &str) -> Result<Conn, String> {
        Conn::reader(host, port, user, password_file, database)
    }

    /// Runs `f` with this connection's one session held, so a transaction's statements cannot
    /// interleave with another caller's. A session that failed on anything but a lost race is dropped.
    pub fn transaction<T>(&self, f: impl FnOnce(&mut dyn crate::content_write::Tx) -> T) -> Result<T, String> {
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let reusable = guard.take().and_then(|mut w| if w.idle_too_long() && !w.ping() { None } else { Some(w) });
        let wire = match reusable {
            Some(w) => w,
            None => self.connect().map_err(|e| match e {
                ScriptFailure::CannotTell(m) => m,
                ScriptFailure::LostRace => "a connect lost a race".into(),
            })?,
        };
        let mut tx = WireTx { wire, healthy: true };
        let out = f(&mut tx);
        if tx.healthy {
            *guard = Some(tx.wire);
        }
        Ok(out)
    }

    /// The administrator, with no default database: what a statement that names its own database runs as.
    pub fn administrator(host: &str, port: u16, user: &str, password: String) -> Conn {
        Conn {
            host: host.to_string(),
            port,
            user: user.to_string(),
            password: Mutex::new(password),
            password_file: None,
            database: String::new(),
            sessions: Sessions::new(1),
            io_timeout: ADMIN_IO_TIMEOUT,
        }
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
            sessions: Sessions::new(1),
            io_timeout: std::time::Duration::from_millis(200),
        }
    }

    fn database(&self) -> Option<&str> {
        Some(self.database.as_str()).filter(|d| !d.is_empty())
    }

    fn connect(&self) -> Result<Wire, ScriptFailure> {
        let mut failure = None;
        for attempt in 0..CONNECT_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(connect_backoff(attempt));
            }
            match self.connect_once() {
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
    fn connect_once(&self) -> Result<Wire, ScriptFailure> {
        let current = self.password.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let first = Wire::connect_with(&self.host, self.port, &self.user, &current, self.database(), self.io_timeout);
        match (&first, self.password_file.as_deref()) {
            (Err(e), Some(file)) if refused_login(e) => {
                let Some(fresh) = reloaded_password(file, &current, |p| std::fs::read_to_string(p)) else { return first };
                let retry = Wire::connect_with(&self.host, self.port, &self.user, &fresh, self.database(), self.io_timeout);
                if retry.is_ok() {
                    *self.password.lock().unwrap_or_else(|e| e.into_inner()) = fresh;
                }
                retry
            }
            _ => first,
        }
    }

    /// A single query: the rows of its last result set.
    pub fn query(&self, sql: &str) -> Result<Vec<Value>, DbError> {
        match self.run(sql) {
            Ok(sets) => Ok(sets.into_iter().last().unwrap_or_default()),
            Err(ScriptFailure::LostRace) => unreachable!("a plain SELECT never conflicts"),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    /// One statement that returns no rows.
    pub fn execute(&self, sql: &str) -> Result<(), DbError> {
        match self.run(sql) {
            Ok(_) => Ok(()),
            Err(ScriptFailure::LostRace) => Err(DbError::CannotTell("a statement lost a race".into())),
            Err(ScriptFailure::CannotTell(e)) => Err(DbError::CannotTell(e)),
        }
    }

    fn run(&self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        self.sessions.run(|| self.connect(), script)
    }
}

/// What a pooled session does: `Wire` over the socket, a double in tests.
pub(crate) trait Session {
    fn exec(&mut self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure>;
    fn idle_too_long(&self) -> bool;
    fn ping(&mut self) -> bool;
}

impl Session for Wire {
    fn exec(&mut self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        Wire::exec(self, script)
    }
    fn idle_too_long(&self) -> bool {
        Wire::idle_too_long(self)
    }
    fn ping(&mut self) -> bool {
        Wire::ping(self)
    }
}

struct Pool<S> {
    idle: Vec<S>,
    /// Sessions idle or checked out; a failed one stops counting.
    open: usize,
}

pub(crate) struct Sessions<S> {
    cap: usize,
    pool: Mutex<Pool<S>>,
    released: Condvar,
}

impl<S: Session> Sessions<S> {
    pub(crate) fn new(cap: usize) -> Self {
        Sessions { cap, pool: Mutex::new(Pool { idle: Vec::new(), open: 0 }), released: Condvar::new() }
    }

    /// Runs `script` on an idle session, a fresh one while under the cap, or the next one released.
    /// The pool lock is never held across a query or a connect.
    pub(crate) fn run(&self, open: impl Fn() -> Result<S, ScriptFailure>, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        let idle = loop {
            if let Some(s) = pool.idle.pop() {
                break Some(s);
            }
            if pool.open < self.cap {
                pool.open += 1;
                break None;
            }
            pool = self.released.wait(pool).unwrap_or_else(|e| e.into_inner());
        };
        drop(pool);
        let mut slot = Slot { pool: self, session: idle };
        #[allow(clippy::manual_filter)]
        let ready = slot.session.take().and_then(|mut s| if s.idle_too_long() && !s.ping() { None } else { Some(s) });
        let mut session = match ready {
            Some(s) => s,
            None => {
                let opened = open();
                crate::slow::record(std::time::Duration::ZERO, "CONNECT");
                opened?
            }
        };
        let started = std::time::Instant::now();
        let out = session.exec(script);
        crate::slow::record(started.elapsed(), script);
        if out.is_ok() {
            slot.session = Some(session);
        }
        out
    }
}

/// A reserved place in the pool. Dropping it, on unwind too, returns the session if one is
/// still held, else frees the place, and wakes a waiting read.
struct Slot<'a, S: Session> {
    pool: &'a Sessions<S>,
    session: Option<S>,
}

impl<S: Session> Drop for Slot<'_, S> {
    fn drop(&mut self) {
        let mut pool = self.pool.pool.lock().unwrap_or_else(|e| e.into_inner());
        match self.session.take() {
            Some(s) => pool.idle.push(s),
            None => pool.open -= 1,
        }
        drop(pool);
        self.pool.released.notify_one();
    }
}

struct WireTx {
    wire: Wire,
    healthy: bool,
}

impl crate::content_write::Tx for WireTx {
    fn send(&mut self, sql: &str) -> Result<(), crate::content_write::Fail> {
        let started = std::time::Instant::now();
        let out = self.wire.exec(sql);
        crate::slow::record(started.elapsed(), sql);
        out.map(|_| ()).map_err(|e| match e {
            ScriptFailure::LostRace => crate::content_write::Fail::Conflict,
            ScriptFailure::CannotTell(m) => {
                self.healthy = false;
                crate::content_write::Fail::Other(m)
            }
        })
    }
}

pub(crate) enum ScriptFailure {
    /// COMMIT reported a serialization conflict (SQLSTATE 40001).
    LostRace,
    CannotTell(String),
}

/// The password a connection logs in with: the contents of `file` when that names a file and is
/// non-empty, else `password`. A credential file exported empty must not shadow the password.
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
mod connect_tests {
    use super::*;

    #[test]
    fn a_refusal_from_the_server_is_not_retried_but_a_dead_socket_is() {
        assert!(refused_for_good(&ScriptFailure::CannotTell("Error 1045 (28000): Access denied".into())));
        assert!(refused_for_good(&ScriptFailure::CannotTell("Error 1049 (42000): Unknown database".into())));
        assert!(!refused_for_good(&ScriptFailure::CannotTell("Connection refused".into())));
    }

    #[test]
    fn a_rotated_credential_file_is_picked_up_and_an_unchanged_one_is_not() {
        let read = |_: &str| Ok("new\n".to_string());
        assert_eq!(reloaded_password("f", "old", read).as_deref(), Some("new"));
        assert_eq!(reloaded_password("f", "new", read), None);
        assert_eq!(reloaded_password("f", "old", |_: &str| Ok("  \n".to_string())), None);
    }

    #[test]
    fn connect_backoff_doubles_from_200ms() {
        assert_eq!(connect_backoff(1), std::time::Duration::from_millis(200));
        assert_eq!(connect_backoff(2), std::time::Duration::from_millis(400));
    }
}

#[cfg(test)]
mod session_pool_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Arc;
    use std::time::Duration;

    /// A session whose `block` query waits for a release; any other query answers; `boom` fails.
    struct Double {
        entered: Sender<()>,
        release: Arc<(Mutex<usize>, Condvar)>,
    }

    impl Session for Double {
        fn exec(&mut self, script: &str) -> Result<Vec<Vec<Value>>, ScriptFailure> {
            match script {
                "block" => {
                    self.entered.send(()).unwrap();
                    let (m, c) = &*self.release;
                    let mut n = m.lock().unwrap();
                    while *n == 0 {
                        n = c.wait(n).unwrap();
                    }
                    *n -= 1;
                    Ok(vec![])
                }
                "panic" => panic!("session died"),
                "boom" => Err(ScriptFailure::CannotTell("boom".into())),
                _ => Ok(vec![vec![Value::from(1)]]),
            }
        }
        fn idle_too_long(&self) -> bool {
            false
        }
        fn ping(&mut self) -> bool {
            true
        }
    }

    struct Rig {
        pool: Arc<Sessions<Double>>,
        opens: Arc<AtomicUsize>,
        entered_tx: Sender<()>,
        entered: Receiver<()>,
        release: Arc<(Mutex<usize>, Condvar)>,
    }

    impl Rig {
        fn new(cap: usize) -> Rig {
            let (entered_tx, entered) = channel();
            Rig {
                pool: Arc::new(Sessions::new(cap)),
                opens: Arc::new(AtomicUsize::new(0)),
                entered_tx,
                entered,
                release: Arc::new((Mutex::new(0), Condvar::new())),
            }
        }
        fn read(&self, script: &'static str) -> Receiver<()> {
            let (done_tx, done) = channel();
            let (pool, opens, tx, release) = (self.pool.clone(), self.opens.clone(), self.entered_tx.clone(), self.release.clone());
            std::thread::spawn(move || {
                let _ = pool.run(
                    || {
                        opens.fetch_add(1, Ordering::SeqCst);
                        Ok(Double { entered: tx.clone(), release: release.clone() })
                    },
                    script,
                );
                let _ = done_tx.send(());
            });
            done
        }
        fn release_one(&self) {
            *self.release.0.lock().unwrap() += 1;
            self.release.1.notify_all();
        }
    }

    const BOUND: Duration = Duration::from_secs(1);

    #[test]
    fn a_held_read_does_not_block_another() {
        let rig = Rig::new(READER_SESSIONS);
        let first = rig.read("block");
        rig.entered.recv_timeout(BOUND).unwrap();
        let second = rig.read("fast");
        second.recv_timeout(BOUND).expect("read 2 answered while read 1 was held");
        rig.release_one();
        first.recv_timeout(BOUND).unwrap();
    }

    #[test]
    fn the_cap_holds_and_a_read_over_it_waits_for_a_release() {
        let rig = Rig::new(READER_SESSIONS);
        let held: Vec<_> = (0..READER_SESSIONS).map(|_| rig.read("block")).collect();
        for _ in 0..READER_SESSIONS {
            rig.entered.recv_timeout(BOUND).unwrap();
        }
        let fifth = rig.read("fast");
        assert_eq!(rig.opens.load(Ordering::SeqCst), READER_SESSIONS);
        rig.release_one();
        fifth.recv_timeout(BOUND).expect("5th read answered after a release");
        assert_eq!(rig.opens.load(Ordering::SeqCst), READER_SESSIONS);
        for _ in 1..READER_SESSIONS {
            rig.release_one();
        }
        for h in held {
            h.recv_timeout(BOUND).unwrap();
        }
    }

    #[test]
    fn a_panicking_read_gives_its_place_back() {
        let rig = Rig::new(1);
        let _ = rig.read("panic").recv_timeout(BOUND);
        rig.read("fast").recv_timeout(BOUND).expect("the next read answered");
    }

    #[test]
    fn the_reader_holds_the_reader_cap_and_the_administrator_one() {
        let admin = Conn::administrator("h", 1, "u", String::new());
        assert_eq!(admin.sessions.cap, 1);
        let dir = testkit::TempDir::new("spira-lc-pool-pw");
        let file = dir.path().join("pw");
        std::fs::write(&file, "pw").unwrap();
        let r = Conn::reader("h", 1, "u", file.to_str().unwrap(), "d").unwrap();
        assert_eq!(r.sessions.cap, READER_SESSIONS);
    }

    #[test]
    fn a_failed_session_is_not_reused() {
        let rig = Rig::new(READER_SESSIONS);
        rig.read("fast").recv_timeout(BOUND).unwrap();
        assert_eq!(rig.opens.load(Ordering::SeqCst), 1);
        rig.read("boom").recv_timeout(BOUND).unwrap();
        assert_eq!(rig.opens.load(Ordering::SeqCst), 1, "boom reused the idle session");
        rig.read("fast").recv_timeout(BOUND).unwrap();
        assert_eq!(rig.opens.load(Ordering::SeqCst), 2);
    }
}
