//! Lapsed aeons — killed by the liveness lease since the previous sweep. `aeon.sh` (out of
//! this bead's scope) writes one record per lapse under `$SPIRA_RUN/lapsed/`, named
//! `<bead-id>-<YYYYMMDDTHHMMSSZ>`; this reads every record newer than the marker the
//! previous sweep left, so each lapse appears in exactly one sweep.
//!
//! `?` when the directory cannot be read — never 0, never silence
//! (law-absence-needs-a-positive-control): a lapse section that renders empty on a failed
//! read displaces the suspicion that would prompt a look, which is the exact failure this
//! whole program exists to catch.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Count {
    Known(u32),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lapsed {
    pub count: Count,
    pub section: String,
}

fn timestamp_suffix(filename: &str) -> Option<&str> {
    // Last 16 characters: 8 digits, 'T', 6 digits, 'Z' — `YYYYMMDDTHHMMSSZ`.
    if filename.len() < 16 {
        return None;
    }
    let tail = &filename[filename.len() - 16..];
    let bytes = tail.as_bytes();
    let digits_ok = |r: std::ops::Range<usize>| bytes[r].iter().all(|b| b.is_ascii_digit());
    if digits_ok(0..8) && bytes[8] == b'T' && digits_ok(9..15) && bytes[15] == b'Z' {
        Some(tail)
    } else {
        None
    }
}

/// `prev_marker` is the previous sweep's cutoff (empty string = no previous marker, i.e.
/// every record is new). Records are read in filename order, matching `find … | sort`.
pub fn read(dir: &Path, prev_marker: &str) -> Lapsed {
    if !dir.exists() {
        // Never recorded — the directory is created by aeon.sh on the first lapse.
        return Lapsed {
            count: Count::Known(0),
            section: "  none since last sweep".to_string(),
        };
    }
    if !dir.is_dir() {
        return Lapsed {
            count: Count::Unknown,
            section: "  ? (expected a directory at SPIRA_LAPSED_DIR — path exists but is not a directory)".to_string(),
        };
    }
    let mut entries: Vec<(String, std::path::PathBuf)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .map(|s| (s.to_string(), e.path()))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut n = 0u32;
    let mut body = String::new();
    for (name, path) in entries {
        let Some(ts) = timestamp_suffix(&name) else {
            continue;
        };
        if !prev_marker.is_empty() && ts <= prev_marker {
            continue;
        }
        n += 1;
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        body.push_str(&format!("\n  [{name}]\n"));
        for line in content.lines() {
            body.push_str("    ");
            body.push_str(line);
            body.push('\n');
        }
    }
    let section = if n == 0 {
        "  none since last sweep".to_string()
    } else {
        let word = if n == 1 { "lapse" } else { "lapses" };
        format!("  {n} {word} since last sweep:{body}")
    };
    Lapsed {
        count: Count::Known(n),
        section,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_directory_is_zero_not_unknown() {
        let d = testkit::TempDir::new("wt-lapsed-missing");
        let l = read(&d.join("lapsed"), "");
        assert_eq!(l.count, Count::Known(0));
    }

    #[test]
    fn a_file_where_the_directory_should_be_is_unknown() {
        let d = testkit::TempDir::new("wt-lapsed-notdir");
        let p = d.join("lapsed");
        std::fs::write(&p, "oops").unwrap();
        let l = read(&p, "");
        assert_eq!(l.count, Count::Unknown);
    }

    #[test]
    fn records_newer_than_the_marker_are_counted_older_ones_are_not() {
        let d = testkit::TempDir::new("wt-lapsed-records");
        let dir = d.join("lapsed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sp-a-20260901T000000Z"), "OLD outcome=noted\n").unwrap();
        std::fs::write(dir.join("sp-b-20260902T000000Z"), "NEW outcome=recovered\n").unwrap();
        let l = read(&dir, "20260901T120000Z");
        assert_eq!(l.count, Count::Known(1));
        assert!(l.section.contains("sp-b-20260902T000000Z"));
        assert!(!l.section.contains("sp-a-20260901T000000Z"));
    }

    #[test]
    fn no_marker_counts_every_record() {
        let d = testkit::TempDir::new("wt-lapsed-nomarker");
        let dir = d.join("lapsed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sp-a-20260901T000000Z"), "x\n").unwrap();
        std::fs::write(dir.join("sp-b-20260902T000000Z"), "y\n").unwrap();
        let l = read(&dir, "");
        assert_eq!(l.count, Count::Known(2));
    }

    #[test]
    fn a_filename_with_no_valid_timestamp_suffix_is_skipped() {
        let d = testkit::TempDir::new("wt-lapsed-badname");
        let dir = d.join("lapsed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("not-a-lapse-record"), "x\n").unwrap();
        let l = read(&dir, "");
        assert_eq!(l.count, Count::Known(0));
    }
}
