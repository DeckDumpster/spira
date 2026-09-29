//! The outward-facing records: exit codes and the one log line per call.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Exit codes — the contract `batcher-cut::io::rebase_stale` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exit {
    /// Nothing to do, or rebased (clean/mechanical) and re-certified.
    Ok = 0,
    /// A real content conflict; bead reopened with the hunks quoted.
    Conflict = 1,
    /// Rebased but red at the new tip; branch restored, bead reopened with the gate output.
    GateRed = 2,
    /// Not attempted (busy, missing, lock, scratch failure) — the caller's fallback runs.
    NotAttempted = 3,
}

impl Exit {
    pub fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Current,
    Busy,
    Error,
    Conflict,
    GateRed,
    Clean,
    Mechanical,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Outcome::Current => "current",
            Outcome::Busy => "busy",
            Outcome::Error => "error",
            Outcome::Conflict => "conflict",
            Outcome::GateRed => "gate-red",
            Outcome::Clean => "clean",
            Outcome::Mechanical => "mechanical",
        })
    }
}

/// One line of `SPIRA_REBASE_STALE_LOG`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub ts: String,
    pub id: String,
    pub repo: String,
    pub outcome: Outcome,
    pub reason: String,
}

impl fmt::Display for LogRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // One line, always: a reason carrying a newline would split the record.
        let reason = self.reason.replace('\n', " ");
        write!(
            f,
            "REBASE_STALE {} id={} repo={} outcome={} reason={}",
            self.ts, self.id, self.repo, self.outcome, reason
        )
    }
}

impl LogRecord {
    pub fn now(id: &str, repo: &str, outcome: Outcome, reason: &str) -> LogRecord {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        LogRecord {
            ts: utc_stamp(secs),
            id: id.into(),
            repo: repo.into(),
            outcome,
            reason: reason.into(),
        }
    }

    /// Appends the line; failure to log never changes the outcome (as the bash's `|| true`).
    pub fn append(&self, log: &Path) {
        if let Some(d) = log.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = writeln!(f, "{self}");
        }
    }
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` for a unix time.
pub fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // civil_from_days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_matches_date_u() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(1_790_000_000), "2026-09-21T14:13:20Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn log_line_shape_is_the_bash_shape() {
        let r = LogRecord {
            ts: "2026-09-29T01:02:03Z".into(),
            id: "sp-a".into(),
            repo: "spira".into(),
            outcome: Outcome::GateRed,
            reason: "tip=abc\nmore".into(),
        };
        assert_eq!(r.to_string(), "REBASE_STALE 2026-09-29T01:02:03Z id=sp-a repo=spira outcome=gate-red reason=tip=abc more");
        assert_eq!(Exit::NotAttempted.code(), 3);
        assert_eq!(
            serde_json::to_string(&Outcome::GateRed).unwrap(),
            "\"gate-red\""
        );
    }
}
