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

#[cfg(test)]
mod tests {
    use super::key_of_path;

    #[test]
    fn a_spira_path_names_the_registry_key_the_scheduler_pins() {
        assert_eq!(key_of_path("spira.max_dwarves").as_deref(), Some("SPIRA_MAX_DWARVES"));
        assert_eq!(key_of_path("spira.cockpit_sessions").as_deref(), Some("COCKPIT_SESSIONS"));
        assert_eq!((key_of_path("personas.builder.model"), key_of_path("spira."), key_of_path("spira.a.b")), (None, None, None));
    }
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
        Some("unchanged") => format!("scheduler already holds {held:?} for {key}"),
        Some("restart") => format!("written; the scheduler reads {key} only when it starts and holds {held:?}; restart it to take {wanted:?}"),
        Some("pending") => {
            for _ in 0..POLLS {
                if call(sock, &["scheduler", "running", key]).is_ok_and(|r| r == wanted) {
                    return format!("scheduler re-pinned {key}");
                }
                std::thread::sleep(POLL_EVERY);
            }
            let held_now = call(sock, &["scheduler", "running", key]).unwrap_or(held);
            let waits = call(sock, &["scheduler", "status"]).ok().and_then(|d| serde_json::from_str::<serde_json::Value>(&d).ok()).and_then(|d| d["repin"]["waits"].as_str().map(str::to_string)).unwrap_or_else(|| "a turn".into());
            format!("written; the scheduler still holds {held_now:?} for {key} and takes {wanted:?} at its next safe point ({waits})")
        }
        _ => format!("written; the scheduler's answer about {key} was not understood: {reply}"),
    }
}

/// `get --running <key>`: the value the scheduler holds for `key`.
pub fn running(sock: &str, key: &str) -> Result<String, String> {
    call(Path::new(sock), &["scheduler", "running", key])
}
