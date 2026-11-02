//! A health probe — one bounded shell assertion (`_wd_probe`). The verdict is the exit
//! code: anything but zero is DEGRADED, because not-found, killed, timed out and crashed all
//! mean the same thing here — nothing PROVED the watcher can still see
//! (law-absence-needs-a-positive-control). An empty command is a third fact, "no assertion
//! was made", never a pass.
//!
//! DESIGN.md "Decisions": the bound is enforced by this process directly (spawn, poll for
//! exit with a deadline, kill past it) rather than by depending on an external `timeout`
//! binary — so there is no "not on PATH" fallback to unbounded execution to warn about.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// No assertion was made — the row carries no health command. Rendered `-`.
    NotAsserted,
    Ok,
    Degraded(String),
}

/// `command` empty means no assertion was configured. Otherwise `bash -c "$command"`,
/// stdout discarded, stderr captured (never passed through — it must not be able to write a
/// line into a table something else parses by column).
pub fn probe(command: &str, timeout: Duration) -> Health {
    if command.is_empty() {
        return Health::NotAsserted;
    }
    let envs = match spira_config::release_env::child_path_env_for_process() {
        Ok(e) => e,
        Err(e) => return Health::Degraded(e),
    };
    let mut child = match Command::new("bash").envs(envs)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Health::Degraded(format!("could not run the probe: {e} (exit -1)")),
    };

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stderr);
                }
                if status.success() {
                    return Health::Ok;
                }
                let first_line = stderr.lines().next().unwrap_or("").to_string();
                let code = status.code().unwrap_or(-1);
                let why = if first_line.is_empty() { "the probe said nothing".to_string() } else { first_line };
                return Health::Degraded(truncate(&format!("{why} (exit {code})")));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Health::Degraded(truncate(&format!(
                        "the probe was still running (timed out after {}s)",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Health::Degraded(format!("could not wait on the probe: {e} (exit -1)")),
        }
    }
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= 200 {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(197).collect();
        out.push_str("...");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_command_is_not_asserted() {
        assert_eq!(probe("", Duration::from_secs(5)), Health::NotAsserted);
    }
}
