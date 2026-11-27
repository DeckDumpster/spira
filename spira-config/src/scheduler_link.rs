//! `spira-config set|unset` telling the running scheduler which key changed, and
//! `get --running` asking what it holds: its control socket, the same `scheduler` verbs
//! `world scheduler-status` speaks.

use std::io::{BufRead, ErrorKind, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(5);
const POLLS: u32 = 20;
const POLL_EVERY: Duration = Duration::from_millis(100);

/// Why a request did not return a body: the scheduler refused it, no scheduler was reached
/// (socket absent or refusing), or one was reached or may be and said nothing usable.
#[derive(Debug)]
pub enum Fault {
    Refused(String),
    Absent(String),
    Silent(String),
}

/// One request on the control socket: a JSON argv line out, `{"exit_code","len"}` and the body back.
pub fn call(sock: &Path, argv: &[&str]) -> Result<String, String> {
    call_within(sock, argv, DEADLINE).map_err(|e| match e {
        Fault::Refused(body) => body,
        Fault::Absent(why) | Fault::Silent(why) => format!("the scheduler is not answering on {}: {why}", sock.display()),
    })
}

/// `call` with its read and write `deadline`.
fn call_within(sock: &Path, argv: &[&str], deadline: Duration) -> Result<String, Fault> {
    let silent = |e: &dyn std::fmt::Display| Fault::Silent(e.to_string());
    let stream = UnixStream::connect(sock).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => Fault::Absent(e.to_string()),
        _ => silent(&e),
    })?;
    stream.set_read_timeout(Some(deadline)).and_then(|()| stream.set_write_timeout(Some(deadline))).map_err(|e| silent(&e))?;
    writeln!(&stream, "{}", serde_json::to_string(argv).map_err(|e| Fault::Refused(e.to_string()))?).map_err(|e| silent(&e))?;
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    reader.read_line(&mut head).map_err(|e| silent(&e))?;
    let head: serde_json::Value = serde_json::from_str(head.trim()).map_err(|_| silent(&"malformed reply"))?;
    let (Some(code), Some(len)) = (head["exit_code"].as_i64(), head["len"].as_u64()) else { return Err(silent(&"malformed reply")) };
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).map_err(|e| silent(&e))?;
    let body = String::from_utf8_lossy(&body).into_owned();
    if code == 0 { Ok(body) } else { Err(Fault::Refused(body)) }
}

/// The registry key a dotted `spira.<field>` path names; `None` for a path outside `[spira]`.
pub fn key_of_path(path: &str) -> Option<String> {
    let field = path.strip_prefix("spira.")?;
    if field.is_empty() || field.contains('.') {
        return None;
    }
    let upper = field.to_ascii_uppercase();
    Some(if field.starts_with("cockpit_") { upper } else { format!("SPIRA_{upper}") })
}

/// The line `set` or `unset` prints for `key` after its write: whether the scheduler took the
/// new value or still holds the old one, and what it waits for. `sock` is the control socket,
/// or why it could not be found.
pub fn told(sock: Result<String, String>, key: &str) -> (String, bool) {
    told_within(sock, key, DEADLINE)
}

/// `told` with `deadline` for the scheduler's reply. The flag is false only when a scheduler
/// accepted the connection and did not answer: it still holds its old value, so the new one is
/// not in force. Every other outcome, no scheduler running included, leaves nothing to retry.
pub fn told_within(sock: Result<String, String>, key: &str, deadline: Duration) -> (String, bool) {
    let sock = match sock {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) => return (format!("written; no scheduler control socket is configured, so {key} reaches a scheduler when it starts"), true),
        Err(e) => return (format!("written; cannot tell whether a scheduler holds the old value of {key}: {e}"), true),
    };
    let sock = Path::new(&sock);
    let reply = match call_within(sock, &["scheduler", "repin", key], deadline) {
        Ok(r) => r,
        Err(Fault::Refused(refused)) => return (format!("written; {refused}; it reads {key} when it starts"), true),
        Err(Fault::Absent(e)) => return (format!("written; the scheduler is not running on {} ({e}); it reads {key} when it starts", sock.display()), true),
        Err(Fault::Silent(e)) => {
            let secs = if deadline.as_secs() > 0 { format!("{}s", deadline.as_secs()) } else { format!("{}ms", deadline.as_millis()) };
            return (format!("written; the scheduler on {} did not answer within {secs} ({e}) and still holds its old {key}; run the same set again, or check with spira-config get --running {key}", sock.display()), false);
        }
    };
    let v: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
    let (held, wanted) = (v["held"].as_str().unwrap_or("").to_string(), v["wanted"].as_str().unwrap_or("").to_string());
    let line = match v["state"].as_str() {
        Some("unchanged") => format!("in force: the scheduler holds {held} for {key}"),
        Some("restart") => format!("written; the scheduler reads {key} only when it starts and holds {held}; restart it to take {wanted}"),
        Some("pending") => {
            for _ in 0..POLLS {
                if call(sock, &["scheduler", "running", key]).is_ok_and(|r| r == wanted) {
                    return (format!("scheduler re-pinned {key}"), true);
                }
                std::thread::sleep(POLL_EVERY);
            }
            let held_now = call(sock, &["scheduler", "running", key]).unwrap_or(held);
            let waits = call(sock, &["scheduler", "status"]).ok().and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok()).and_then(|d| d["repin"]["waits"].as_str().map(str::to_string)).unwrap_or_else(|| "a turn".into());
            format!("written; the scheduler still holds {held_now} for {key} and takes {wanted} at its next safe point ({waits})")
        }
        _ => format!("written; the scheduler's answer about {key} was not understood: {reply}"),
    };
    (line, true)
}

/// `get --running <key>`: the value the scheduler holds for `key`.
pub fn running(sock: &str, key: &str) -> Result<String, String> {
    call(Path::new(sock), &["scheduler", "running", key])
}

/// One scheduler-pinned limit as three readers see it: the config file, the value the running
/// scheduler holds, and any re-pin it has yet to take.
#[derive(Debug, PartialEq, Eq)]
pub enum Limit {
    Held { value: String, pending: Option<Pending>, file_differs: Option<String> },
    Unanswered { file: Option<String>, why: String },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Pending {
    pub config: String,
    pub waits: String,
}

impl Limit {
    /// `running` is the reply of `scheduler running <key>`; `status` the parsed `scheduler status`.
    pub fn from_parts(key: &str, file: Option<&str>, running: Result<String, String>, status: Option<&serde_json::Value>) -> Limit {
        let held = match running {
            Ok(v) => v.trim().to_string(),
            Err(why) => return Limit::Unanswered { file: file.map(str::to_string), why },
        };
        let pending = status
            .and_then(|s| s.pointer(&format!("/repin/pending/{key}/wanted"))) // not a path: a JSON pointer
            .and_then(serde_json::Value::as_str)
            .map(|config| Pending {
                config: config.to_string(),
                waits: status.and_then(|s| s.pointer("/repin/waits")).and_then(serde_json::Value::as_str).filter(|w| !w.is_empty()).unwrap_or("its next safe point").to_string(),
            });
        let file_differs = file.filter(|f| *f != held).filter(|_| pending.is_none()).map(str::to_string);
        Limit::Held { value: held, pending, file_differs }
    }

    pub fn read(key: &str, file: Option<&str>) -> Limit {
        let sock = match crate::process::cfg("SPIRA_RUNTIME_SOCKET") {
            Ok(s) if !s.trim().is_empty() => s,
            Ok(_) => return Limit::Unanswered { file: file.map(str::to_string), why: "no scheduler control socket is configured".into() },
            Err(e) => return Limit::Unanswered { file: file.map(str::to_string), why: e },
        };
        let sock = Path::new(sock.trim());
        let running = call(sock, &["scheduler", "running", key]);
        let status = running.is_ok().then(|| call(sock, &["scheduler", "status"]).ok().and_then(|d| serde_json::from_str(&d).ok())).flatten();
        Limit::from_parts(key, file, running, status.as_ref())
    }

    /// The number to count with: the held value, else the file's when the scheduler is silent.
    pub fn shown(&self) -> Option<&str> {
        match self {
            Limit::Held { value, .. } => Some(value),
            Limit::Unanswered { file, .. } => file.as_deref(),
        }
    }

    /// The number the scheduler enforces; `None` when it did not answer.
    pub fn held(&self) -> Option<&str> {
        match self {
            Limit::Held { value, .. } => Some(value),
            Limit::Unanswered { .. } => None,
        }
    }

    pub fn pending(&self) -> Option<&Pending> {
        match self {
            Limit::Held { pending, .. } => pending.as_ref(),
            Limit::Unanswered { .. } => None,
        }
    }

    /// The text after the label: the held value, then what a differing config waits for; or the
    /// file value labelled as the file's when the scheduler is silent.
    pub fn render(&self) -> String {
        match self {
            Limit::Held { value, pending: None, file_differs: None } => value.clone(),
            Limit::Held { value, pending: None, file_differs: Some(f) } => format!("{value} held; config file says {f}, no re-pin pending"),
            Limit::Held { value, pending: Some(p), .. } => format!("{value} held; config says {}, taken when {}", p.config, p.waits),
            Limit::Unanswered { file: Some(f), why } => format!("{f} per the config file, NOT confirmed held ({why})"),
            Limit::Unanswered { file: None, why } => format!("? ({why})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{key_of_path, told_within, Limit, Pending};
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::time::Duration;

    #[test_id(tf6t0j3)]
    #[test]
    fn a_scheduler_that_accepts_and_never_replies_is_reported_as_still_holding_the_old_value() {
        let dir = testkit::TempDir::new("told-silent");
        let sock = dir.path().join("c.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let held = std::thread::spawn(move || {
            let conn = listener.accept();
            let _ = released.recv();
            conn
        });
        let (line, taken) = told_within(Ok(sock.display().to_string()), "SPIRA_MAX_DWARVES", Duration::from_millis(50));
        assert!(!taken && line.contains("did not answer within") && line.contains("still holds"), "{line}");
        release.send(()).unwrap();
        held.join().unwrap().unwrap();
    }

    #[test_id(tfxt7we)]
    #[test]
    fn a_written_value_the_scheduler_already_holds_is_in_force_and_a_differing_one_asks_for_a_restart() {
        let dir = testkit::TempDir::new("told-held");
        let sock = dir.path().join("c.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let replies = std::thread::spawn(move || {
            for body in [json!({"state": "unchanged", "held": "5", "wanted": "5"}), json!({"state": "restart", "held": "5", "wanted": "6"})] {
                let (mut conn, _) = listener.accept().unwrap();
                BufReader::new(&conn).read_line(&mut String::new()).unwrap();
                let body = body.to_string();
                write!(conn, "{}\n{body}", json!({"exit_code": 0, "len": body.len()})).unwrap();
            }
        });
        let tell = || told_within(Ok(sock.display().to_string()), "SPIRA_MAX_DWARVES", Duration::from_secs(5));
        assert_eq!(tell(), ("in force: the scheduler holds 5 for SPIRA_MAX_DWARVES".to_string(), true));
        let (line, taken) = tell();
        assert!(taken && line.ends_with("restart it to take 6"), "{line}");
        replies.join().unwrap();
    }

    #[test_id(tyx56jb)]
    #[test]
    fn a_socket_that_cannot_be_opened_for_want_of_permission_is_not_called_not_running() {
        let dir = testkit::TempDir::new("told-denied");
        let sock = dir.path().join("c.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        std::fs::set_permissions(&sock, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
        let (line, _) = told_within(Ok(sock.display().to_string()), "SPIRA_MAX_DWARVES", Duration::from_millis(50));
        assert!(!line.contains("is not running"), "{line}");
    }

    #[test_id(txif1oh)]
    #[test]
    fn no_listener_on_the_socket_is_a_scheduler_not_running_and_taken() {
        let dir = testkit::TempDir::new("told-absent");
        let sock = dir.path().join("nobody.sock");
        let (line, taken) = told_within(Ok(sock.display().to_string()), "SPIRA_MAX_DWARVES", Duration::from_millis(50));
        assert!(taken && line.contains("the scheduler is not running on") && line.contains("it reads SPIRA_MAX_DWARVES when it starts"), "{line}");
    }

    fn status(key: &str, wanted: &str) -> serde_json::Value {
        json!({"repin": {"pending": {key: {"held": "7", "wanted": wanted}}, "waits": "the batcher to finish the round it is driving"}})
    }

    #[test_id(t84vsa7)]
    #[test]
    fn a_limit_the_scheduler_holds_as_the_file_says_renders_bare() {
        let l = Limit::from_parts("SPIRA_MAX_DWARVES", Some("8"), Ok("8".into()), Some(&json!({"repin": {"pending": {}}})));
        assert_eq!((l.held(), l.pending(), l.render().as_str()), (Some("8"), None, "8"));
    }

    #[test_id(tkaabxx)]
    #[test]
    fn a_limit_held_below_the_file_shows_both_and_what_the_change_waits_for() {
        let l = Limit::from_parts("SPIRA_MAX_DWARVES", Some("9"), Ok("7".into()), Some(&status("SPIRA_MAX_DWARVES", "9")));
        assert_eq!(l.held(), Some("7"));
        assert_eq!(l.pending(), Some(&Pending { config: "9".into(), waits: "the batcher to finish the round it is driving".into() }));
        assert_eq!(l.render(), "7 held; config says 9, taken when the batcher to finish the round it is driving");
    }

    #[test_id(tilb17t)]
    #[test]
    fn a_file_that_differs_with_no_recorded_re_pin_is_not_given_a_wait() {
        let l = Limit::from_parts("SPIRA_MAX_DWARVES", Some("9"), Ok("7".into()), Some(&json!({"repin": {"pending": {}}})));
        assert_eq!((l.pending(), l.render().as_str()), (None, "7 held; config file says 9, no re-pin pending"));
    }

    #[test_id(t3mpt5h)]
    #[test]
    fn a_silent_scheduler_shows_the_file_value_labelled_never_as_held() {
        let l = Limit::from_parts("SPIRA_MAX_DWARVES", Some("9"), Err("the scheduler is not answering on /s: refused".into()), None);
        assert_eq!(l.held(), None);
        assert!(l.render().starts_with("9 per the config file, NOT confirmed held"), "{}", l.render());
    }

    #[test_id(t4ht5lk)]
    #[test]
    fn a_spira_path_names_the_registry_key_the_scheduler_pins() {
        assert_eq!(key_of_path("spira.max_dwarves").as_deref(), Some("SPIRA_MAX_DWARVES"));
        assert_eq!(key_of_path("spira.cockpit_sessions").as_deref(), Some("COCKPIT_SESSIONS"));
        assert_eq!((key_of_path("personas.builder.model"), key_of_path("spira."), key_of_path("spira.a.b")), (None, None, None));
    }
}
