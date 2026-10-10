//! `watchd tail <name>` — replay from the cursor, then stream; the Monitor command
//! (`cmd_tail`). ONE reader per watcher is the contract: the lock is taken BEFORE the cursor
//! is read, because reading the position and then both streaming is the duplicate-delivery
//! bug itself.
//!
//! DESIGN.md "Decisions": this is one process reading the file directly and writing the
//! cursor as it goes — no background `tail -F | awk` pipeline, no pidfile, no
//! mawk/gawk-fflush workaround, because there is no second process here to buffer output
//! or to need killing on the way out. TERM/INT/HUP are handled in this process's own poll
//! loop and simply stop it; dropping the lock file handle on exit releases the flock, which
//! is the whole mechanism — never a pid file, which gets a crash, a kill and a departed
//! session all wrong.

use crate::filter::Filter;
use crate::fs_ops;
use crate::iso8601;
use crate::lock;
use crate::manifest::{Kind, Row};
use crate::paths;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
    for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe {
            libc::signal(s, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
}

pub struct TailArgs<'a> {
    pub name: &'a str,
    pub all: bool,
    pub takeover: bool,
    pub from_start: bool,
}

/// One pass over whatever is newly available in `file` starting at `byte_offset`: complete
/// lines are matched against `filter` (`None` shows everything), shown lines go to `out`,
/// and the cursor — a LINE NUMBER, not a byte count — advances for every line examined,
/// shown or not, because a filtered line has been considered and rejected, not missed.
/// Returns the new byte offset and the new line number.
pub fn poll_once(file: &mut File, byte_offset: u64, start_line: u64, filter: Option<&Filter>, out: &mut dyn Write) -> std::io::Result<(u64, u64)> {
    use std::io::{Seek, SeekFrom};
    let len = file.metadata()?.len();
    if len < byte_offset {
        // Truncated (or rotated in place) — the same case `tail -F` handles by re-reading
        // from the start.
        file.seek(SeekFrom::Start(0))?;
        return Ok((0, start_line));
    }
    file.seek(SeekFrom::Start(byte_offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let mut consumed = 0u64;
    let mut line = start_line;
    let mut rest = &buf[..];
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        let raw = &rest[..nl];
        let text = String::from_utf8_lossy(raw);
        line += 1;
        if filter.map(|f| f.matches(&text)).unwrap_or(true) {
            writeln!(out, "{text}")?;
        }
        consumed += nl as u64 + 1;
        rest = &rest[nl + 1..];
    }
    out.flush()?;
    Ok((byte_offset + consumed, line))
}

/// Skips `n` complete lines from the start of `file`, returning the byte offset just past
/// them — used once, to resume from the cursor before streaming begins.
fn byte_offset_of_line(file: &mut File, n: u64) -> std::io::Result<u64> {
    use std::io::Seek;
    file.rewind()?;
    if n == 0 {
        return Ok(0);
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let mut seen = 0u64;
    let mut offset = 0u64;
    for (i, &b) in buf.iter().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == n {
                offset = i as u64 + 1;
                break;
            }
        }
    }
    Ok(offset)
}

pub fn cmd_tail(rows: &[Row], run: &str, conf_file: &str, actionable: &str, now: i64, args: &TailArgs) -> i32 {
    let Some(row) = rows.iter().find(|r| r.name == args.name) else {
        eprintln!("watchd: no watcher named '{}' in the manifest", args.name);
        return 2;
    };
    if row.kind == Kind::Off {
        eprintln!("watchd: '{}' is optional and {} is not set in {conf_file} — there is nothing to tail", row.name, row.target);
        return 2;
    }
    let filter: Option<Filter> = if args.all || args.from_start {
        None
    } else {
        match Filter::compile(actionable) {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("watchd: {e}");
                return 2;
            }
        }
    };

    let lf = paths::logfile(run, &row.name, row.kind, &row.target).expect("not off");
    let cf = paths::cursorfile(run, &row.name);
    let _ = std::fs::create_dir_all(paths::watchd_dir(run));

    let lockf = paths::tail_lockfile(run, &row.name);
    let mut held = match lock::try_lock(&lockf) {
        Ok(Ok(f)) => f,
        Ok(Err(())) => {
            let holder = lock::read_holder_pid(&lockf);
            if !args.takeover {
                eprintln!("watchd: '{}' is already being tailed by pid {holder} — not attaching a second reader.", row.name);
                eprintln!("  Its events are still being delivered to whoever holds it; a second tail would deliver");
                eprintln!("  every line twice and corrupt the shared cursor. If this session should have the stream");
                eprintln!("  instead, re-run with --takeover.");
                return 3;
            }
            if let Ok(pid) = holder.parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            match lock::wait_for_lock(&lockf, Duration::from_secs(10)) {
                Ok(Some(f)) => f,
                _ => {
                    eprintln!("watchd: '{}' is still held by pid {holder} ten seconds after TERM — refusing rather than running beside it", row.name);
                    return 3;
                }
            }
        }
        Err(e) => {
            eprintln!("watchd: cannot open {}: {e}", lockf.display());
            return 1;
        }
    };
    let _ = lock::record_holder(&mut held);

    let total = fs_ops::total_lines(&lf);
    let cursor_pos = fs_ops::read_pos(&cf, total);
    let stamp = iso8601::format_utc(now);
    let start_line = if args.from_start {
        eprintln!("watchd: '{}' resuming from line 0 of {total} — from the start, cursor stays at {cursor_pos} ({stamp})", row.name);
        0
    } else {
        eprintln!("watchd: '{}' resuming from line {cursor_pos} of {total} ({stamp})", row.name);
        cursor_pos
    };
    let write_cursor = !args.from_start;

    if !lf.exists() {
        eprintln!("watchd: {} does not exist yet — waiting for it", lf.display());
    }
    let Some(mut file) = wait_for_file(&lf) else { return 0 };
    let mut byte_offset = byte_offset_of_line(&mut file, start_line).unwrap_or(0);
    let mut line = start_line;
    let stdout = std::io::stdout();
    loop {
        if STOP.load(Ordering::SeqCst) {
            return 0;
        }
        if let Ok((new_off, new_line)) = poll_once(&mut file, byte_offset, line, filter.as_ref(), &mut stdout.lock()) {
            if new_line != line && write_cursor {
                let _ = fs_ops::write_pos(&cf, new_line);
            }
            byte_offset = new_off;
            line = new_line;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_for_file(path: &Path) -> Option<File> {
    loop {
        if let Ok(f) = File::open(path) {
            return Some(f);
        }
        if STOP.load(Ordering::SeqCst) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::TempDir;

    #[test]
    fn poll_once_shows_every_complete_line_and_leaves_a_partial_one_for_next_time() {
        let d = TempDir::new("watchd-tail");
        let p = d.join("x.log");
        std::fs::write(&p, "a\nb\npartial").unwrap();
        let mut f = File::open(&p).unwrap();
        let mut out = Vec::new();
        let (off, line) = poll_once(&mut f, 0, 0, None, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\nb\n");
        assert_eq!(line, 2);
        assert_eq!(off, 4); // "a\nb\n".len()

        std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"\n").unwrap();
        let mut out2 = Vec::new();
        let (_off2, line2) = poll_once(&mut f, off, line, None, &mut out2).unwrap();
        assert_eq!(String::from_utf8(out2).unwrap(), "partial\n");
        assert_eq!(line2, 3);
    }

    #[test]
    fn the_cursor_advances_for_a_filtered_out_line_too() {
        let d = TempDir::new("watchd-tail");
        let p = d.join("x.log");
        std::fs::write(&p, "plain progress\nFAIL something\n").unwrap();
        let mut f = File::open(&p).unwrap();
        let filt = Filter::compile(crate::filter::DEFAULT).unwrap();
        let mut out = Vec::new();
        let (_off, line) = poll_once(&mut f, 0, 0, Some(&filt), &mut out).unwrap();
        assert_eq!(line, 2, "both lines were considered, so the line count is 2 even though only one showed");
        assert_eq!(String::from_utf8(out).unwrap(), "FAIL something\n");
    }

    #[test]
    fn all_mode_shows_everything() {
        let d = TempDir::new("watchd-tail");
        let p = d.join("x.log");
        std::fs::write(&p, "plain progress\n").unwrap();
        let mut f = File::open(&p).unwrap();
        let mut out = Vec::new();
        poll_once(&mut f, 0, 0, None, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "plain progress\n");
    }

    #[test]
    fn a_truncated_file_is_re_read_from_the_start() {
        let d = TempDir::new("watchd-tail");
        let p = d.join("x.log");
        std::fs::write(&p, "aaaaaaaaaa\n").unwrap();
        let mut f = File::open(&p).unwrap();
        let mut out = Vec::new();
        let (off, _line) = poll_once(&mut f, 0, 0, None, &mut out).unwrap();
        assert_eq!(off, 11);
        std::fs::write(&p, "short\n").unwrap();
        let mut out2 = Vec::new();
        let (off2, _line2) = poll_once(&mut f, off, 1, None, &mut out2).unwrap();
        assert_eq!(off2, 0, "truncation resets the offset so the next poll re-reads from 0");
    }

    #[test]
    fn byte_offset_of_line_resumes_past_exactly_n_lines() {
        let d = TempDir::new("watchd-tail");
        let p = d.join("x.log");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        let mut f = File::open(&p).unwrap();
        assert_eq!(byte_offset_of_line(&mut f, 0).unwrap(), 0);
        assert_eq!(byte_offset_of_line(&mut f, 1).unwrap(), 4);
        assert_eq!(byte_offset_of_line(&mut f, 2).unwrap(), 8);
    }

    #[test]
    fn an_off_row_is_refused() {
        let rows = vec![Row { name: "view".into(), kind: Kind::Off, target: "@SPIRA_VIEW@".into(), health: String::new() }];
        let rc = cmd_tail(&rows, "/tmp", "spira.conf", crate::filter::DEFAULT, 0, &TailArgs { name: "view", all: false, takeover: false, from_start: false });
        assert_eq!(rc, 2);
    }

    #[test]
    fn an_unknown_name_is_refused() {
        let rows: Vec<Row> = vec![];
        let rc = cmd_tail(&rows, "/tmp", "spira.conf", crate::filter::DEFAULT, 0, &TailArgs { name: "nope", all: false, takeover: false, from_start: false });
        assert_eq!(rc, 2);
    }
}
