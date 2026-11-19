//! The beads database's reader, and the one-shot administrator's connection `admin-drop-lifecycle`
//! uses. Both speak the MySQL protocol to Dolt's `sql-server` themselves (`wire.rs`) over reused
//! connections; forking the `dolt` CLI per query cost a process start on every call. The
//! lifecycle machine itself never touches this: it lives in the control store.

use std::sync::Mutex;

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
    /// The one live connection, opened on first use and dropped on any failure: after an
    /// error a transaction may still be open, so the next call starts from a fresh session.
    session: Mutex<Option<Wire>>,
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
            session: Mutex::new(None),
            io_timeout: QUERY_DEADLINE,
        })
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
            session: Mutex::new(None),
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
            session: Mutex::new(None),
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
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        #[allow(clippy::manual_filter)]
        let reusable = guard.take().and_then(|mut w| if w.idle_too_long() && !w.ping() { None } else { Some(w) });
        let wire = reusable.map(Ok).unwrap_or_else(|| self.connect());
        crate::slow::record(std::time::Duration::ZERO, "CONNECT");
        match wire {
            Ok(mut wire) => {
                let started = std::time::Instant::now();
                let out = wire.exec(script);
                crate::slow::record(started.elapsed(), script);
                if out.is_ok() {
                    *guard = Some(wire);
                }
                out
            }
            Err(e) => Err(e),
        }
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
