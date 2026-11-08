//! `inbox-triage append <text>` — record an event, and for an urgent one wake the session
//! once per burst. The timing rule is `inbox_rules::Flush`; this file is the only place
//! that sleeps between its ticks. The append-and-push leg itself is `inbox_rules::record`, shared
//! with every other writer of the inbox (sp-nasmjd).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use inbox_rules::{record, spool_path};

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

fn now_iso() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    spira_world::clock::utc_iso(t as i64)
}

pub fn run(text: &str) -> Result<(), String> {
    let inbox = spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?;
    let line = inbox_rules::line(&now_iso(), text);
    // the line is recorded whatever the push keys say; a push key that will not parse must not fail the caller
    let (wake, pattern) = match push_config() {
        Ok(p) => (p.wake, p.pattern),
        Err(e) => {
            eprintln!("inbox-triage append: push disabled: {e}");
            (String::new(), String::new())
        }
    };
    record(Path::new(&inbox), &line, &wake, &pattern, || std::env::current_exe().map_err(|e| e.to_string())).map(|_| ())
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
    let spool = spool_path(Path::new(&inbox));
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
