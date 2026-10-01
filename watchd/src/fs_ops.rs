//! Plain filesystem IO on the two files a reader latches onto (`<name>.log`,
//! `<name>.cursor`) and the three watchd keeps for itself (`.pending`, `.restarts`,
//! `.unhealthy`). No trait here — this is deterministic, real-filesystem behaviour, tested
//! directly against `testkit::TempDir` rather than faked.

use crate::cursor;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// `wc -l` — counts newline BYTES, so a final line with no trailing newline is one short.
/// That undercount is the bash's own behaviour (DESIGN.md: ported, not an accident here) and
/// is harmless: the next write always ends in `\n`, so it self-corrects on the next line.
pub fn total_lines(path: &Path) -> u64 {
    let Ok(bytes) = fs::read(path) else { return 0 };
    bytes.iter().filter(|&&b| b == b'\n').count() as u64
}

pub fn read_pos(cursor_file: &Path, total: u64) -> u64 {
    let raw = fs::read_to_string(cursor_file).ok().and_then(|s| cursor::parse_cursor(&s));
    cursor::clamp_pos(raw, total)
}

pub fn write_pos(cursor_file: &Path, pos: u64) -> std::io::Result<()> {
    if let Some(dir) = cursor_file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(cursor_file, format!("{pos}\n"))
}

/// The exact range `(start, end]` (1-indexed, matching `sed -n 'start+1,endp'`) — never a
/// `tail | head`, because the log is being appended to while this runs and the caller's
/// `end` was taken before the read, binding what is shown to what gets marked read.
pub fn read_range(path: &Path, start: u64, end: u64) -> Vec<String> {
    let Ok(f) = fs::File::open(path) else { return Vec::new() };
    let r = BufReader::new(f);
    r.lines()
        .skip(start as usize)
        .take((end.saturating_sub(start)) as usize)
        .map_while(|l| l.ok())
        .collect()
}

pub fn last_line(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()?.lines().last().map(|s| s.to_string())
}

pub fn read_u64_file(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.split_whitespace().next()?.parse().ok()
}

pub fn write_u64_file(path: &Path, v: u64) -> std::io::Result<()> {
    ensure_parent(path)?;
    fs::write(path, format!("{v}\n"))
}

/// `<name>.pending`: `<line> <epoch>` — the position of the oldest actionable unread line,
/// and when this harness first saw it standing there.
pub fn read_pending(path: &Path) -> Option<(u64, i64)> {
    let text = fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let pos: u64 = parts.next()?.parse().ok()?;
    let at: i64 = parts.next()?.parse().ok()?;
    Some((pos, at))
}

pub fn write_pending(path: &Path, pos: u64, at: i64) -> std::io::Result<()> {
    ensure_parent(path)?;
    fs::write(path, format!("{pos} {at}\n"))
}

pub fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use testkit::TempDir;

    #[test]
    fn total_lines_counts_newlines_and_is_zero_for_a_missing_file() {
        let d = TempDir::new("watchd-fs");
        let p = d.join("x.log");
        assert_eq!(total_lines(&p), 0);
        fs::write(&p, "a\nb\nc\n").unwrap();
        assert_eq!(total_lines(&p), 3);
    }

    #[test]
    fn total_lines_undercounts_an_unterminated_final_line() {
        let d = TempDir::new("watchd-fs");
        let p = d.join("x.log");
        fs::write(&p, "a\nb\nc").unwrap();
        assert_eq!(total_lines(&p), 2);
    }

    #[test]
    fn a_missing_cursor_file_reads_as_zero() {
        let d = TempDir::new("watchd-fs");
        assert_eq!(read_pos(&d.join("nope.cursor"), 10), 0);
    }

    #[test]
    fn cursor_round_trips_and_clamps() {
        let d = TempDir::new("watchd-fs");
        let p = d.join("x.cursor");
        write_pos(&p, 5).unwrap();
        assert_eq!(read_pos(&p, 10), 5);
        assert_eq!(read_pos(&p, 3), 3, "a cursor past the (shrunk) total clamps to it");
    }

    #[test]
    fn read_range_is_the_exact_half_open_window() {
        let d = TempDir::new("watchd-fs");
        let p = d.join("x.log");
        fs::write(&p, "1\n2\n3\n4\n5\n").unwrap();
        assert_eq!(read_range(&p, 2, 4), vec!["3".to_string(), "4".to_string()]);
        assert_eq!(read_range(&p, 0, 0), Vec::<String>::new());
        assert_eq!(read_range(&p, 5, 5), Vec::<String>::new());
    }

    #[test]
    fn pending_round_trips() {
        let d = TempDir::new("watchd-fs");
        let p = d.join("x.pending");
        assert_eq!(read_pending(&p), None);
        write_pending(&p, 12, 1000).unwrap();
        assert_eq!(read_pending(&p), Some((12, 1000)));
    }

    #[test]
    fn last_line_of_an_empty_or_missing_file_is_none() {
        let d = TempDir::new("watchd-fs");
        assert_eq!(last_line(&d.join("nope")), None);
        fs::write(d.join("empty"), "").unwrap();
        assert_eq!(last_line(&d.join("empty")), None);
        fs::write(d.join("one"), "only\n").unwrap();
        assert_eq!(last_line(&d.join("one")), Some("only".to_string()));
    }
}
