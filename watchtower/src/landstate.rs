//! Reads `$SPIRA_RUN/landstate/<id>` records: one file per branch, first line
//! `STATUS TIP AT REASON...` (space-separated; `TIP`/`AT`/`REASON` may be empty). This is
//! the same format `land_mark` writes and every bash reader parsed with
//! `read -r st tip at reason < file` — whitespace-split, not a stable schema with a parser
//! of its own, so this mirrors that exactly rather than introducing one.

use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawRecord {
    pub id: String,
    pub status: String,
    pub tip: String,
    pub at: String,
    pub reason: String,
}

impl RawRecord {
    /// `AT` parsed as an integer, matching bash's `case "$at" in ''|*[!0-9]*) ;; esac` —
    /// anything not purely digits is "unreadable", not zero.
    pub fn at_secs(&self) -> Option<i64> {
        if self.at.is_empty() || !self.at.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        self.at.parse().ok()
    }
}

fn parse_line(line: &str) -> (String, String, String, String) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let get = |i: usize| words.get(i).copied().unwrap_or("").to_string();
    // The 4th field must absorb the remainder of the line (trailing words), matching
    // `read`'s last-variable behaviour, not just the 4th whitespace-split token.
    let reason = if words.len() > 3 {
        // Find where the 4th word starts in the original line and take the rest verbatim.
        let mut idx = 0usize;
        let mut seen = 0usize;
        let bytes = line.as_bytes();
        let mut in_word = false;
        for (i, b) in bytes.iter().enumerate() {
            let ws = (*b as char).is_whitespace();
            if !ws && !in_word {
                seen += 1;
                in_word = true;
                if seen == 4 {
                    idx = i;
                    break;
                }
            } else if ws {
                in_word = false;
            }
        }
        line[idx..].trim_end().to_string()
    } else {
        get(3)
    };
    (get(0), get(1), get(2), reason)
}

/// Reads every regular file directly under `dir` (no recursion, matching
/// `find "$dir" -maxdepth 1 -type f`). An unreadable directory yields an empty list — the
/// same "nothing found" the bash's `[ -d "$dir" ]` guard produced; callers that must
/// distinguish "no directory" from "directory, no matches" check `dir.is_dir()` themselves.
pub fn read_dir(dir: &Path) -> Vec<RawRecord> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let id = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let first_line = match std::fs::read_to_string(&path) {
            Ok(t) => t.lines().next().unwrap_or("").to_string(),
            Err(_) => continue,
        };
        let (status, tip, at, reason) = parse_line(&first_line);
        out.push(RawRecord {
            id,
            status,
            tip,
            at,
            reason,
        });
    }
    out
}

/// The most recent `LANDED` record's `(id, at)` — the one number that says whether the
/// pipeline works. `None` means either the directory is unreadable or no `LANDED` record
/// with a numeric `AT` exists; callers render that as `?`, never as "nothing landed yet".
pub fn last_landed(dir: &Path) -> Option<(String, i64)> {
    if !dir.is_dir() {
        return None;
    }
    let mut best: Option<(String, i64)> = None;
    for rec in read_dir(dir) {
        if rec.status != "LANDED" {
            continue;
        }
        if let Some(at) = rec.at_secs() {
            if best.as_ref().map(|(_, b)| at > *b).unwrap_or(true) {
                best = Some((rec.id.clone(), at));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_file_and_splits_four_fields() {
        let d = testkit::TempDir::new("wt-landstate");
        std::fs::write(d.join("sp-a"), "CERTIFIED deadbeef 1700000000 pr-open:spira\n").unwrap();
        std::fs::write(d.join("sp-b"), "LANDED\n").unwrap();
        let mut recs = read_dir(&d);
        recs.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].id, "sp-a");
        assert_eq!(recs[0].status, "CERTIFIED");
        assert_eq!(recs[0].tip, "deadbeef");
        assert_eq!(recs[0].at, "1700000000");
        assert_eq!(recs[0].reason, "pr-open:spira");
        assert_eq!(recs[1].status, "LANDED");
        assert_eq!(recs[1].tip, "");
    }

    #[test]
    fn missing_directory_reads_as_empty_not_an_error() {
        let recs = read_dir(Path::new("/does/not/exist/landstate"));
        assert!(recs.is_empty());
    }

    #[test]
    fn last_landed_picks_the_newest_landed_record() {
        let d = testkit::TempDir::new("wt-landstate-last");
        std::fs::write(d.join("sp-a"), "LANDED deadbeef 1700000000 x\n").unwrap();
        std::fs::write(d.join("sp-b"), "LANDED deadbeef 1700003000 x\n").unwrap();
        std::fs::write(d.join("sp-c"), "CERTIFIED deadbeef 1700009999 x\n").unwrap();
        assert_eq!(last_landed(&d), Some(("sp-b".to_string(), 1700003000)));
    }

    #[test]
    fn at_secs_rejects_anything_not_purely_digits() {
        let mut r = RawRecord {
            at: "1700000000".into(),
            ..Default::default()
        };
        assert_eq!(r.at_secs(), Some(1700000000));
        r.at = "".into();
        assert_eq!(r.at_secs(), None);
        r.at = "12x".into();
        assert_eq!(r.at_secs(), None);
    }
}
