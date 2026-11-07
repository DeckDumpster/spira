//! `inbox-triage append <text>` — record an event, and for an urgent one wake the session
//! once per burst. The timing rule is `inbox_rules::Flush`; this file is the only place
//! that sleeps between its ticks.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const TICKS_PER_SECOND: u32 = 5;

pub struct Push {
    pub wake: String,
    pub pattern: String,
    pub window: u32,
    pub idle: u32,
}

pub fn push_config() -> Result<Push, String> {
    use spira_config::process::{cfg, cfg_parse};
    Ok(Push {
        wake: cfg("SPIRA_CONCIERGE_PUSH_WAKE")?,
        pattern: cfg("SPIRA_CONCIERGE_PUSH_PATTERN")?,
        window: cfg_parse("SPIRA_CONCIERGE_PUSH_WINDOW")?,
        idle: cfg_parse("SPIRA_CONCIERGE_PUSH_IDLE")?,
    })
}

pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")
}

fn spool_path(inbox: &str) -> PathBuf {
    PathBuf::from(format!("{inbox}.push"))
}

fn now_iso() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    spira_world::clock::utc_iso(t as i64)
}

pub fn run(text: &str) -> Result<(), String> {
    let inbox = spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?;
    let line = inbox_rules::line(&now_iso(), text);
    append_line(Path::new(&inbox), &line).map_err(|e| format!("cannot append to {inbox}: {e}"))?;

    // the line is already recorded; a push key that will not parse must not fail the caller
    let Ok(push) = push_config() else { return Ok(()) };
    if push.wake.is_empty() {
        return Ok(());
    }
    let Ok(pattern) = regex::Regex::new(&push.pattern) else { return Ok(()) };
    if !inbox_rules::is_urgent(text, &pattern) {
        return Ok(());
    }
    let spool = spool_path(&inbox);
    append_line(&spool, &line).map_err(|e| format!("cannot spool to {}: {e}", spool.display()))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // batch-job: spawned detached and never waited on
    let _ = Command::new(exe).arg("flush").process_group(0).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    Ok(())
}

fn size(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// The detached flusher: the lock holder drains until the spool is empty, so a line appended
/// during a held wake is never stranded behind a flusher that failed to take the lock.
pub fn flush() -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let inbox = spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?;
    let push = push_config()?;
    let spool = spool_path(&inbox);
    let lock = std::fs::OpenOptions::new().create(true).append(true).open(format!("{inbox}.push.lock")).map_err(|e| e.to_string())?;
    // SAFETY: flock on an fd this process owns.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(());
    }
    while spool.exists() {
        let mut due = inbox_rules::Flush::new(push.window * TICKS_PER_SECOND, push.idle * TICKS_PER_SECOND);
        while !due.tick(size(&spool)) {
            std::thread::sleep(Duration::from_millis(1000 / TICKS_PER_SECOND as u64));
        }
        let batch = PathBuf::from(format!("{}.{}", spool.display(), std::process::id()));
        if std::fs::rename(&spool, &batch).is_err() {
            return Ok(());
        }
        let lines: Vec<String> = std::fs::read_to_string(&batch).unwrap_or_default().lines().map(String::from).collect();
        let _ = std::fs::remove_file(&batch);
        let mut words = push.wake.split_whitespace();
        if let Some(prog) = words.next() {
            // batch-job: the detached flusher holds here while the wake waits for an empty input line
            let _ = Command::new(prog).args(words).arg(inbox_rules::wake_text(&lines)).status();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_creates_the_inbox_and_a_second_call_does_not_truncate() {
        let d = testkit::TempDir::new("inbox-append");
        let inbox = d.join("elsewhere/inbox.log");
        assert!(!inbox.exists(), "the inbox does not exist before the first append");
        append_line(&inbox, "2026-11-06T00:00:00Z hello world").unwrap();
        assert_eq!(std::fs::read_to_string(&inbox).unwrap(), "2026-11-06T00:00:00Z hello world\n");
        append_line(&inbox, "2026-11-06T00:00:01Z second event").unwrap();
        assert_eq!(std::fs::read_to_string(&inbox).unwrap().lines().count(), 2);
    }
}
