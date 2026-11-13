//! `spira-config set|unset` telling the running scheduler which key changed, and
//! `get --running` asking what it holds: its control socket, the same `scheduler` verbs
//! `world scheduler-status` speaks.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(5);
const POLLS: u32 = 20;
const POLL_EVERY: Duration = Duration::from_millis(100);

/// One request on the control socket: a JSON argv line out, `{"exit_code","len"}` and the body back.
pub fn call(sock: &Path, argv: &[&str]) -> Result<String, String> {
    let down = |why: &dyn std::fmt::Display| format!("the scheduler is not answering on {}: {why}", sock.display());
    let stream = UnixStream::connect(sock).map_err(|e| down(&e))?;
    stream.set_read_timeout(Some(DEADLINE)).and_then(|()| stream.set_write_timeout(Some(DEADLINE))).map_err(|e| down(&e))?;
    writeln!(&stream, "{}", serde_json::to_string(argv).map_err(|e| e.to_string())?).map_err(|e| down(&e))?;
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    reader.read_line(&mut head).map_err(|e| down(&e))?;
    let head: serde_json::Value = serde_json::from_str(head.trim()).map_err(|_| down(&"malformed reply"))?;
    let (Some(code), Some(len)) = (head["exit_code"].as_i64(), head["len"].as_u64()) else { return Err(down(&"malformed reply")) };
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).map_err(|e| down(&e))?;
    let body = String::from_utf8_lossy(&body).into_owned();
    if code == 0 { Ok(body) } else { Err(body) }
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
pub fn told(sock: Result<String, String>, key: &str) -> String {
    let sock = match sock {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) => return format!("written; no scheduler control socket is configured, so {key} reaches a scheduler when it starts"),
        Err(e) => return format!("written; cannot tell whether a scheduler holds the old value of {key}: {e}"),
    };
    let sock = Path::new(&sock);
    let reply = match call(sock, &["scheduler", "repin", key]) {
        Ok(r) => r,
        Err(e) => return format!("written; {e}; it reads {key} when it starts"),
    };
    let v: serde_json::Value = serde_json::from_str(&reply).unwrap_or_default();
    let (held, wanted) = (v["held"].as_str().unwrap_or("").to_string(), v["wanted"].as_str().unwrap_or("").to_string());
    match v["state"].as_str() {
        Some("unchanged") => format!("scheduler already holds {held} for {key}"),
        Some("restart") => format!("written; the scheduler reads {key} only when it starts and holds {held}; restart it to take {wanted}"),
        Some("pending") => {
            for _ in 0..POLLS {
                if call(sock, &["scheduler", "running", key]).is_ok_and(|r| r == wanted) {
                    return format!("scheduler re-pinned {key}");
                }
                std::thread::sleep(POLL_EVERY);
            }
            let held_now = call(sock, &["scheduler", "running", key]).unwrap_or(held);
            let waits = call(sock, &["scheduler", "status"]).ok().and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok()).and_then(|d| d["repin"]["waits"].as_str().map(str::to_string)).unwrap_or_else(|| "a turn".into());
            format!("written; the scheduler still holds {held_now} for {key} and takes {wanted} at its next safe point ({waits})")
        }
        _ => format!("written; the scheduler's answer about {key} was not understood: {reply}"),
    }
}

/// `get --running <key>`: the value the scheduler holds for `key`.
pub fn running(sock: &str, key: &str) -> Result<String, String> {
    call(Path::new(sock), &["scheduler", "running", key])
}

/// One scheduler-pinned limit as three readers see it: the config file, the value the running
/// scheduler holds, and any re-pin it has yet to take.
#[derive(Debug, PartialEq, Eq)]
pub enum Limit {
    Held { value: String, pending: Option<Pending> },
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
        let recorded = status.and_then(|s| s.pointer(&format!("/repin/pending/{key}/wanted"))).and_then(serde_json::Value::as_str);
        let differs = file.filter(|f| *f != held);
        let pending = match (recorded, differs) {
            (Some(wanted), _) => Some(wanted),
            (None, f) => f,
        }
        .filter(|w| *w != held)
        .map(|config| Pending {
            config: config.to_string(),
            waits: status.and_then(|s| s.pointer("/repin/waits")).and_then(serde_json::Value::as_str).filter(|w| !w.is_empty()).unwrap_or("the scheduler's next start").to_string(),
        });
        Limit::Held { value: held, pending }
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
            Limit::Held { value, pending: None } => value.clone(),
            Limit::Held { value, pending: Some(p) } => format!("{value} held; config says {}, taken when {}", p.config, p.waits),
            Limit::Unanswered { file: Some(f), why } => format!("{f} per the config file, NOT confirmed held ({why})"),
            Limit::Unanswered { file: None, why } => format!("? ({why})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{key_of_path, Limit, Pending};
    use serde_json::json;

    fn status(key: &str, wanted: &str) -> serde_json::Value {
        json!({"repin": {"pending": {key: {"held": "7", "wanted": wanted}}, "waits": "the batcher to finish the round it is driving"}})
    }

    #[test]
    fn a_limit_the_scheduler_holds_as_the_file_says_renders_bare() {
        let l = Limit::from_parts("SPIRA_MAX_DWARVES", Some("8"), Ok("8".into()), Some(&json!({"repin": {"pending": {}}})));
        assert_eq!((l.held(), l.pending(), l.render().as_str()), (Some("8"), None, "8"));
    }

    #[test]
    fn a_limit_held_below_the_file_shows_both_and_what_the_change_waits_for() {
        let l = Limit::from_parts("SPIRA_MAX_DWARVES", Some("9"), Ok("7".into()), Some(&status("SPIRA_MAX_DWARVES", "9")));
        assert_eq!(l.held(), Some("7"));
        assert_eq!(l.pending(), Some(&Pending { config: "9".into(), waits: "the batcher to finish the round it is driving".into() }));
        assert_eq!(l.render(), "7 held; config says 9, taken when the batcher to finish the round it is driving");
    }

    #[test]
    fn a_silent_scheduler_shows_the_file_value_labelled_never_as_held() {
        let l = Limit::from_parts("SPIRA_MAX_DWARVES", Some("9"), Err("the scheduler is not answering on /s: refused".into()), None);
        assert_eq!(l.held(), None);
        assert!(l.render().starts_with("9 per the config file, NOT confirmed held"), "{}", l.render());
    }

    #[test]
    fn a_spira_path_names_the_registry_key_the_scheduler_pins() {
        assert_eq!(key_of_path("spira.max_dwarves").as_deref(), Some("SPIRA_MAX_DWARVES"));
        assert_eq!(key_of_path("spira.cockpit_sessions").as_deref(), Some("COCKPIT_SESSIONS"));
        assert_eq!((key_of_path("personas.builder.model"), key_of_path("spira."), key_of_path("spira.a.b")), (None, None, None));
    }
}
