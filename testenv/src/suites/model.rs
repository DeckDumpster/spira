//! The data `testenv suites` handles (DESIGN-suites.md §3): the population, the gate list,
//! `# covers:` / `# priority:` headers, the last-result record, flake observations, clean-run
//! counters and land state. Pure functions over text; the file
//! reads are thin wrappers so the commands can be tested against a scratch directory.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;


/// Read a file as text (lossy), None when it cannot be read.
pub fn read_text(p: &Path) -> Option<String> {
    fs::read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

// ------------------------------------------------------------------------ population

/// Every `test-*.sh` regular file in `dir`, basenames, sorted. The glob is the definition.
pub fn population(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("test-") && n.ends_with(".sh"))
        .collect();
    v.sort();
    v
}

/// The gate's list is unreadable — which suites the gate runs is UNKNOWN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable;

/// Basenames the gate list names: `#` comments and blank lines dropped, each line trimmed.
pub fn parse_gate_list(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.rsplit('/').next().unwrap_or(l).to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

pub fn gate_list(path: &Path) -> Result<BTreeSet<String>, Unreadable> {
    read_text(path).map(|t| parse_gate_list(&t)).ok_or(Unreadable)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runs {
    Gate,
    Timed,
    Unknown,
}

impl Runs {
    pub fn of(suite: &str, gated: &Result<BTreeSet<String>, Unreadable>) -> Runs {
        match gated {
            Err(_) => Runs::Unknown,
            Ok(g) if g.contains(suite) => Runs::Gate,
            Ok(_) => Runs::Timed,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Runs::Gate => "gate",
            Runs::Timed => "timed",
            Runs::Unknown => "?",
        }
    }
}

// --------------------------------------------------------------------------- headers

/// suite-covers.sh's `suite_covers_of`: the first `# covers:` line plus its continuation
/// lines, blank-joined; empty when there is none (which callers read as "covers
/// everything"). The selector crate's parser (sp-wx2tw).
pub fn covers_of(text: &str) -> String {
    suite_select::header::covers_of(text)
        .map(|v| v.join(" "))
        .unwrap_or_default()
}

/// The first `# priority: N` anywhere in the suite, whitespace removed; `0`–`4` or the default.
pub fn priority_of(text: &str, default: u8) -> u8 {
    let v = text.lines().find_map(|l| {
        let rest = l.strip_prefix('#')?.trim_start_matches(' ');
        Some(rest.strip_prefix("priority:")?.to_string())
    });
    let v: String = v
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    match v.as_bytes() {
        [d @ b'0'..=b'4'] => d - b'0',
        _ => default,
    }
}

// ------------------------------------------------------------------------- records

/// `STATE/<suite>.result`: the leading fields of testenv's result record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastResult {
    pub status: String,
    pub at: u64,
    pub secs: String,
    pub fingerprint: String,
}

impl LastResult {
    /// The first line; status non-empty and epoch all digits, else absent (a truncated
    /// file cannot satisfy this).
    pub fn parse(text: &str) -> Option<LastResult> {
        let line = text.lines().next()?;
        let mut f = line.split_whitespace();
        let status = f.next()?.to_string();
        let at_s = f.next()?;
        if at_s.is_empty() || !at_s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(LastResult {
            status,
            at: at_s.parse().ok()?,
            secs: f.next().unwrap_or("?").to_string(),
            fingerprint: f.next().unwrap_or("-").to_string(),
        })
    }

    pub fn is_red(&self) -> bool {
        matches!(self.status.as_str(), "red" | "timeout" | "red-unconfirmed")
    }
    pub fn is_fault(&self) -> bool {
        matches!(self.status.as_str(), "setup-fault" | "fixture-fault")
    }
    pub fn is_skip(&self) -> bool {
        self.status == "skip"
    }
}

/// A suite name is a basename: no `/`, no leading `.`, not empty.
pub fn valid_suite_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.starts_with('.') && !s.contains('\0')
}

// ------------------------------------------------------------------------------ time

/// `YYYY-MM-DDTHH:MM:SSZ` (or `YYYY-MM-DD`) → epoch; anything else None.
pub fn parse_iso_utc(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t.strip_suffix('Z')?)),
        None => (s, None),
    };
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let m: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let (hh, mm, ss) = match time {
        None => (0, 0, 0),
        Some(t) => {
            let mut f = t.split(':');
            let h: i64 = f.next()?.parse().ok()?;
            let mi: i64 = f.next()?.parse().ok()?;
            let se: i64 = f.next()?.parse().ok()?;
            if f.next().is_some() || h > 23 || mi > 59 || se > 60 {
                return None;
            }
            (h, mi, se)
        }
    };
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let t = days * 86_400 + hh * 3600 + mm * 60 + ss;
    u64::try_from(t).ok()
}

/// `date -u +%Y%m%dT%H%M%SZ` for an epoch.
pub fn compact_stamp(epoch: u64) -> String {
    crate::util::iso_utc(epoch).replace(['-', ':'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tbl73kq)]
    #[test]
    fn gate_list_drops_comments_blanks_and_directories() {
        let g = parse_gate_list("# header\n\n  spira/test-b.sh  # why\ntest-a.sh\n   \n# spira/test-c.sh\n");
        assert_eq!(g.into_iter().collect::<Vec<_>>(), vec!["test-a.sh", "test-b.sh"]);
        assert!(parse_gate_list("").is_empty());
    }

    #[test_id(tqedc3v)]
    #[test]
    fn runs_is_unknown_when_the_gate_list_is_unreadable_never_timed() {
        let g: Result<BTreeSet<String>, Unreadable> = Ok(["test-a.sh".to_string()].into());
        assert_eq!(Runs::of("test-a.sh", &g), Runs::Gate);
        assert_eq!(Runs::of("test-b.sh", &g), Runs::Timed);
        assert_eq!(Runs::of("test-a.sh", &Err(Unreadable)), Runs::Unknown);
        assert_eq!(Runs::Unknown.as_str(), "?");
    }

    #[test_id(td0r2ub)]
    #[test]
    fn covers_folds_continuations_and_stops_at_a_directive() {
        let t = "#!/usr/bin/env bash\n# covers: spira/a.sh spira/b.sh\n#   spira/c.sh UC-x-01\n# \tspira/d.sh\n#\tspira/one-tab.sh\n# tier: T2\n#   spira/late.sh\n";
        assert_eq!(covers_of(t), "spira/a.sh spira/b.sh spira/c.sh UC-x-01 spira/d.sh");
        // a "# word:" directive indented by two spaces is still a continuation (awk: `^# ` is one space)
        assert_eq!(covers_of("# covers: a\n#  host-reason: b\n"), "a host-reason: b");
        assert_eq!(covers_of("# covers: a\n# host-reason: b\n"), "a");
        assert_eq!(covers_of("#covers:x\n"), "x");
        assert_eq!(covers_of("# covers:\n"), "");
        assert_eq!(covers_of("set -e\n"), "");
        // a lone "#" ends the block, as does a line starting "#  #"
        assert_eq!(covers_of("# covers: a\n#\n#   b\n"), "a");
        assert_eq!(covers_of("# covers: a\n#   # b\n"), "a");
    }

    #[test_id(tnr81z4)]
    #[test]
    fn priority_is_a_single_digit_zero_to_four_else_the_default() {
        assert_eq!(priority_of("# covers: x\n# priority: 1\n", 3), 1);
        assert_eq!(priority_of("#priority:   0 \n", 3), 0);
        assert_eq!(priority_of("# priority: 7\n", 3), 3);
        assert_eq!(priority_of("# priority: high\n", 3), 3);
        assert_eq!(priority_of("# priority: 1\n# priority: 2\n", 3), 1);
        assert_eq!(priority_of("", 2), 2);
    }

    #[test_id(tki6kvu)]
    #[test]
    fn last_result_needs_a_status_and_a_numeric_epoch() {
        let r = LastResult::parse("red 1790000000 12 abc123 parallel diff 1\n").unwrap();
        assert_eq!((r.status.as_str(), r.at, r.secs.as_str(), r.fingerprint.as_str()), ("red", 1_790_000_000, "12", "abc123"));
        assert!(r.is_red());
        let short = LastResult::parse("ok 5").unwrap();
        assert_eq!((short.secs.as_str(), short.fingerprint.as_str()), ("?", "-"));
        assert_eq!(LastResult::parse(""), None);
        assert_eq!(LastResult::parse("ok"), None);
        assert_eq!(LastResult::parse("ok 12x 3 -"), None);
        assert!(LastResult::parse("fixture-fault 1 0 -").unwrap().is_fault());
        assert!(LastResult::parse("red-unconfirmed 1 0 -").unwrap().is_red());
        assert!(LastResult::parse("skip 1 0 -").unwrap().is_skip());
    }

    #[test_id(tq43ve0)]
    #[test]
    fn a_suite_name_is_a_basename() {
        assert!(valid_suite_name("test-a.sh"));
        assert!(!valid_suite_name("../x.sh") && !valid_suite_name(".hidden") && !valid_suite_name(""));
    }

    #[test_id(tmdebkx)]
    #[test]
    fn iso_round_trips_and_rejects_other_shapes() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_utc("2026-09-21T14:13:20Z"), Some(1_790_000_000));
        assert_eq!(parse_iso_utc("2000-02-29"), Some(951_782_400));
        assert_eq!(parse_iso_utc("2026-09-21 14:13:20"), None);
        assert_eq!(parse_iso_utc("2026-09-21T14:13:20"), None);
        assert_eq!(parse_iso_utc("yesterday"), None);
        assert_eq!(compact_stamp(1_790_000_000), "20260921T141320Z");
    }

    #[test_id(txz4gav)]
    #[test]
    fn population_is_the_glob() {
        let d = testkit::TempDir::new("suites-pop");
        fs::create_dir_all(d.join("test-dir.sh")).unwrap();
        for f in ["test-b.sh", "test-a.sh", "lib.sh", "test-a.sh.bak", "test-.sh"] {
            fs::write(d.join(f), "").unwrap();
        }
        assert_eq!(population(&d), vec!["test-.sh", "test-a.sh", "test-b.sh"]);
        assert!(population(&d.join("missing")).is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
