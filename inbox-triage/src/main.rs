//! `inbox-triage` — the concierge's in-session Monitor over `SPIRA_CONCIERGE_INBOX`. See
//! `DESIGN.md`. Replaces `spira/inbox-triage.sh` (sp-48f6g).

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Config {
    inbox: String,
    dedup: u64,
}

/// `SPIRA_CONCIERGE_INBOX`/`SPIRA_CONCIERGE_INBOX_DEDUP` are registered keys
/// (`spira/conf.d`) — the one source of config (per Ryan 2026-10-05), read through
/// `spira_config::process::cfg`/`cfg_parse` (resolved from `$SPIRA_TOML`, a plain file
/// parse — no bash child, so the "exit 97, conf.sh could not be sourced under a bare
/// shell" scar this crate used to carry cannot recur: there is no longer a subprocess for
/// a stripped `PATH` to break). A key that fails to resolve is a refusal naming it, never
/// a default.
fn load_config() -> Result<Config, String> {
    Ok(Config {
        inbox: spira_config::process::cfg("SPIRA_CONCIERGE_INBOX")?,
        dedup: spira_config::process::cfg_parse("SPIRA_CONCIERGE_INBOX_DEDUP")?,
    })
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn main() {
    let cfg = match load_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("inbox-triage: {e}");
            std::process::exit(1);
        }
    };
    let log = Path::new(&cfg.inbox);
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::OpenOptions::new().create(true).append(true).open(log).is_err() {
        eprintln!("inbox-triage: cannot create {}", log.display());
        std::process::exit(1);
    }

    let mut file = File::open(log).unwrap_or_else(|e| {
        eprintln!("inbox-triage: cannot open {}: {e}", log.display());
        std::process::exit(1);
    });
    // `tail -n 0 -F`: start from the current end, never replaying history — this is a live
    // Monitor, not a replay tool.
    let mut offset = file.metadata().map(|m| m.len()).unwrap_or(0);

    let mut dedup = inbox_rules::Dedup::default();
    let stdout = std::io::stdout();
    loop {
        if let Ok((new_offset, lines)) = read_new_lines(&mut file, offset) {
            offset = new_offset;
            let now = now_secs();
            let mut out = stdout.lock();
            for line in lines {
                if let Some(body) = dedup.pass(&line, now, cfg.dedup) {
                    let _ = writeln!(out, "{body}");
                }
            }
            let _ = out.flush();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn read_new_lines(file: &mut File, offset: u64) -> std::io::Result<(u64, Vec<String>)> {
    use std::io::{Seek, SeekFrom};
    let len = file.metadata()?.len();
    if len < offset {
        file.seek(SeekFrom::Start(0))?;
        return Ok((0, Vec::new()));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let mut lines = Vec::new();
    let mut consumed = 0u64;
    let mut rest = &buf[..];
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        lines.push(String::from_utf8_lossy(&rest[..nl]).into_owned());
        consumed += nl as u64 + 1;
        rest = &rest[nl + 1..];
    }
    Ok((offset + consumed, lines))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_new_lines_only_returns_what_is_past_the_offset_and_complete() {
        let d = testkit::TempDir::new("inbox-triage");
        let p = d.join("log");
        std::fs::write(&p, "one\ntwo\n").unwrap();
        let mut f = File::open(&p).unwrap();
        let (off, lines) = read_new_lines(&mut f, 0).unwrap();
        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"three\npart").unwrap();
        let (off2, lines2) = read_new_lines(&mut f, off).unwrap();
        assert_eq!(lines2, vec!["three".to_string()]);
        assert!(off2 > off);
    }
}
