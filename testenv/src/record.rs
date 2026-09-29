//! The per-suite result record (`<suite>.result`) and the failure fingerprint.
//!
//! On-disk format (DESIGN.md §3.1), one line:
//! `<status> <epoch> <secs> <fingerprint> <mode> <producer> <rc>` — except `unreached`,
//! which keeps its historical four-field form `unreached <epoch> 0 -`. Every reader in the
//! harness takes field 1 as the status and field 3 as seconds, so both forms parse.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::OnceLock;

/// What happened to one selected suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Ok,
    Skip,
    Red,
    Timeout,
    QuarantinedRed,
    Disabled,
    SkipReq,
    Unreached,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Skip => "skip",
            Status::Red => "red",
            Status::Timeout => "timeout",
            Status::QuarantinedRed => "quarantined-red",
            Status::Disabled => "disabled",
            Status::SkipReq => "skip-req",
            Status::Unreached => "unreached",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "ok" => Status::Ok,
            "skip" => Status::Skip,
            "red" => Status::Red,
            "timeout" => Status::Timeout,
            "quarantined-red" => Status::QuarantinedRed,
            "disabled" => Status::Disabled,
            "skip-req" => Status::SkipReq,
            "unreached" => Status::Unreached,
            _ => return None,
        })
    }

    /// The suite's script actually ran inside the container (whatever its outcome).
    pub fn executed(self) -> bool {
        matches!(
            self,
            Status::Ok | Status::Skip | Status::Red | Status::Timeout | Status::QuarantinedRed
        )
    }

    /// Counts against the verdict: a non-quarantined red or timeout.
    pub fn blocking(self) -> bool {
        matches!(self, Status::Red | Status::Timeout)
    }
}

/// `--mode`: a green under serial is a weaker claim than a green under parallel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Parallel,
    Serial,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Parallel => "parallel",
            Mode::Serial => "serial",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "parallel" => Some(Mode::Parallel),
            "serial" => Some(Mode::Serial),
            _ => None,
        }
    }
}

/// Who decided which suites to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Producer {
    /// a person or aeon named the suites via `--suites`
    Explicit,
    /// the selector derived them from the branch diff
    Diff,
    /// the diff had an unmapped file; the whole corpus ran as a fallback
    All,
}

impl Producer {
    pub fn as_str(self) -> &'static str {
        match self {
            Producer::Explicit => "explicit",
            Producer::Diff => "diff",
            Producer::All => "all",
        }
    }
    pub fn parse(s: &str) -> Option<Producer> {
        match s.trim() {
            "explicit" => Some(Producer::Explicit),
            "diff" => Some(Producer::Diff),
            "all" => Some(Producer::All),
            _ => None,
        }
    }
}

/// One `<suite>.result` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultRecord {
    pub status: Status,
    pub epoch: u64,
    pub secs: u64,
    pub fingerprint: String,
    pub mode: Option<Mode>,
    pub producer: Option<Producer>,
    /// The suite's exit code; `None` renders `-` (disabled, skip-req, unreached).
    pub rc: Option<i32>,
}

impl ResultRecord {
    pub fn unreached(epoch: u64) -> Self {
        ResultRecord {
            status: Status::Unreached,
            epoch,
            secs: 0,
            fingerprint: "-".into(),
            mode: None,
            producer: None,
            rc: None,
        }
    }

    /// The record for a suite that ran and exited `rc` after `secs`, given its output and
    /// whether it is quarantined. 0 = ok, 77 = skip (automake), 124 = the per-suite timeout.
    #[allow(clippy::too_many_arguments)]
    pub fn from_exit(
        rc: i32,
        secs: u64,
        output: &str,
        suite: &str,
        quarantined: bool,
        mode: Mode,
        producer: Producer,
        epoch: u64,
    ) -> Self {
        let (status, fingerprint) = match rc {
            0 => (Status::Ok, "-".to_string()),
            77 => (Status::Skip, "-".to_string()),
            124 => (
                if quarantined {
                    Status::QuarantinedRed
                } else {
                    Status::Timeout
                },
                format!("timeout:{suite}"),
            ),
            _ => (
                if quarantined {
                    Status::QuarantinedRed
                } else {
                    Status::Red
                },
                fingerprint(rc, output),
            ),
        };
        ResultRecord {
            status,
            epoch,
            secs,
            fingerprint,
            mode: Some(mode),
            producer: Some(producer),
            rc: Some(rc),
        }
    }

    pub fn parse(line: &str) -> Option<Self> {
        let f: Vec<&str> = line.split_whitespace().collect();
        let status = Status::parse(f.first()?)?;
        let num = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        Some(ResultRecord {
            status,
            epoch: num(1),
            secs: num(2),
            fingerprint: f.get(3).unwrap_or(&"-").to_string(),
            mode: f.get(4).and_then(|m| Mode::parse(m)),
            producer: f.get(5).and_then(|p| Producer::parse(p)),
            rc: f.get(6).and_then(|r| r.parse().ok()),
        })
    }
}

impl fmt::Display for ResultRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.status == Status::Unreached {
            return write!(f, "unreached {} 0 -", self.epoch);
        }
        let rc = self.rc.map(|r| r.to_string()).unwrap_or_else(|| "-".into());
        write!(
            f,
            "{} {} {} {} {} {} {}",
            self.status.as_str(),
            self.epoch,
            self.secs,
            self.fingerprint,
            self.mode.map(Mode::as_str).unwrap_or("-"),
            self.producer.map(Producer::as_str).unwrap_or("-"),
            rc
        )
    }
}

/// The per-suite stdout line (`printf '  %-32s ...'`), exactly as callers grep it.
pub fn suite_line(suite: &str, rec: &ResultRecord) -> String {
    let rc = rec.rc.unwrap_or(0);
    let tail = match rec.status {
        Status::Ok => format!("ok      {}s", rec.secs),
        Status::Skip => "SKIPPED".to_string(),
        Status::Disabled => "DISABLED".to_string(),
        Status::SkipReq => format!("SKIP-REQ {}", rec.fingerprint),
        Status::Timeout => format!("TIMEOUT after {}s", rec.secs),
        Status::Red => format!("RED     rc={rc} after {}s", rec.secs),
        Status::QuarantinedRed if rc == 124 => {
            format!("QUARANTINED-RED  TIMEOUT after {}s", rec.secs)
        }
        Status::QuarantinedRed => format!("QUARANTINED-RED  rc={rc} after {}s", rec.secs),
        Status::Unreached => "UNREACHED".to_string(),
    };
    format!("  {suite:<32} {tail}")
}

/// bash `$(cat file)` then `printf '%s\n'`: trailing newlines stripped, exactly one added.
pub fn normalize_output(raw: &str) -> String {
    format!("{}\n", raw.trim_end_matches('\n'))
}

fn norm_rules() -> &'static [(Regex, &'static str); 5] {
    static R: OnceLock<[(Regex, &'static str); 5]> = OnceLock::new();
    R.get_or_init(|| {
        [
            (Regex::new(r"/tmp/[A-Za-z0-9._-]*").unwrap(), "/tmp/X"),
            (
                Regex::new(r"/[A-Za-z0-9._/-]*/sptest_[A-Za-z0-9_]*").unwrap(),
                "/X",
            ),
            (
                Regex::new(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[.0-9]*Z?")
                    .unwrap(),
                "TIMESTAMP",
            ),
            (Regex::new(r"[0-9]{2}:[0-9]{2}:[0-9]{2}").unwrap(), "TIME"),
            (Regex::new(r"[0-9]{3,}").unwrap(), "N"),
        ]
    })
}

/// A short stable digest of a red suite's failure, identical to suites.sh's `_fp` so dedup
/// keys agree across callers (DESIGN.md §3.2).
pub fn fingerprint(rc: i32, output: &str) -> String {
    let out = output.trim_end_matches('\n');
    let lines: Vec<&str> = out.split('\n').collect();
    let fails: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.contains("FAIL"))
        .collect();
    let sig = if fails.is_empty() {
        let start = lines.len().saturating_sub(20);
        lines[start..].join("\n")
    } else {
        fails.join("\n")
    };
    let sig = sig.trim_end_matches('\n');
    let text = format!("rc={rc}\n{sig}\n");
    let mut normalized = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        let mut cur = body.to_string();
        for (re, rep) in norm_rules() {
            cur = re.replace_all(&cur, *rep).into_owned();
        }
        normalized.push_str(&cur);
        normalized.push_str(nl);
    }
    let (crc, len) = cksum(normalized.as_bytes());
    format!("{crc}{len}")
}

/// POSIX `cksum`: CRC-32 (poly 0x04C11DB7, MSB first) over the data then its length.
pub fn cksum(data: &[u8]) -> (u32, usize) {
    fn step(mut c: u32, b: u8) -> u32 {
        c ^= (b as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        c
    }
    let mut crc = data.iter().fold(0u32, |c, &b| step(c, b));
    let mut n = data.len();
    while n > 0 {
        crc = step(crc, (n & 0xff) as u8);
        n >>= 8;
    }
    (!crc, data.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cksum_matches_coreutils() {
        // printf 'hello\n' | cksum -> "3015617425 6"
        assert_eq!(cksum(b"hello\n"), (3015617425, 6));
    }

    // Vectors computed with testenv-batch.sh's own _fp function (bash + sed + cksum).
    #[test]
    fn fingerprint_matches_bash_fp_for_empty_output() {
        assert_eq!(fingerprint(1, ""), "4414460516");
    }

    #[test]
    fn fingerprint_uses_tail_when_no_fail_line() {
        assert_eq!(fingerprint(1, "ok 1 - a\nnot ok 2 - b"), "164262933127");
        let seq: Vec<String> = (1..=30).map(|i| i.to_string()).collect();
        assert_eq!(fingerprint(1, &seq.join("\n")), "109937514765");
    }

    #[test]
    fn fingerprint_normalises_paths_times_and_numbers() {
        let out = "x\nFAIL: thing at /tmp/abc.123/foo 2026-09-28T12:34:56Z\nmore\nFAIL again 12:00:01 count 12345";
        assert_eq!(fingerprint(2, out), "30044042365");
        assert_eq!(
            fingerprint(3, "path /home/u/sptest_abc/x/sptest_def_9 end"),
            "66151181917"
        );
    }

    #[test]
    fn fingerprint_ignores_trailing_newlines() {
        assert_eq!(fingerprint(1, "ok 1 - a\nnot ok 2 - b\n\n"), "164262933127");
    }

    #[test]
    fn record_round_trips_seven_fields() {
        let r = ResultRecord::from_exit(
            3,
            12,
            "FAIL x",
            "test-a.sh",
            false,
            Mode::Parallel,
            Producer::Diff,
            1_790_000_000,
        );
        let line = r.to_string();
        assert!(line.starts_with("red 1790000000 12 "));
        assert!(line.ends_with(" parallel diff 3"));
        assert_eq!(ResultRecord::parse(&line), Some(r));
    }

    #[test]
    fn unreached_keeps_the_four_field_form_and_parses() {
        let r = ResultRecord::unreached(5);
        assert_eq!(r.to_string(), "unreached 5 0 -");
        assert_eq!(
            ResultRecord::parse("unreached 5 0 -").unwrap().status,
            Status::Unreached
        );
    }

    #[test]
    fn exit_codes_map_to_statuses() {
        let m = |rc, q| {
            ResultRecord::from_exit(rc, 1, "", "s.sh", q, Mode::Serial, Producer::Explicit, 0)
        };
        assert_eq!(m(0, false).status, Status::Ok);
        assert_eq!(m(0, false).fingerprint, "-");
        assert_eq!(m(77, false).status, Status::Skip);
        assert_eq!(m(124, false).status, Status::Timeout);
        assert_eq!(m(124, false).fingerprint, "timeout:s.sh");
        assert_eq!(m(124, true).status, Status::QuarantinedRed);
        assert_eq!(m(1, true).status, Status::QuarantinedRed);
        assert_eq!(m(1, false).status, Status::Red);
        assert!(!m(1, true).status.blocking());
        assert!(m(124, false).status.blocking());
    }

    #[test]
    fn disabled_and_skip_req_render_a_dash_rc() {
        let r = ResultRecord {
            status: Status::SkipReq,
            epoch: 9,
            secs: 0,
            fingerprint: "requires:jq".into(),
            mode: Some(Mode::Parallel),
            producer: Some(Producer::Diff),
            rc: None,
        };
        assert_eq!(r.to_string(), "skip-req 9 0 requires:jq parallel diff -");
    }

    #[test]
    fn suite_lines_match_the_grepped_shapes() {
        let mk = |status, rc, secs| ResultRecord {
            status,
            epoch: 0,
            secs,
            fingerprint: "requires:jq".into(),
            mode: None,
            producer: None,
            rc,
        };
        assert_eq!(
            suite_line("test-a.sh", &mk(Status::Ok, Some(0), 7)),
            format!("  {:<32} ok      7s", "test-a.sh")
        );
        assert_eq!(
            suite_line("test-a.sh", &mk(Status::Red, Some(2), 7)),
            format!("  {:<32} RED     rc=2 after 7s", "test-a.sh")
        );
        assert_eq!(
            suite_line("test-a.sh", &mk(Status::Skip, Some(77), 1)),
            format!("  {:<32} SKIPPED", "test-a.sh")
        );
        assert_eq!(
            suite_line("t.sh", &mk(Status::QuarantinedRed, Some(124), 3)),
            format!("  {:<32} QUARANTINED-RED  TIMEOUT after 3s", "t.sh")
        );
        assert_eq!(
            suite_line("t.sh", &mk(Status::SkipReq, None, 0)),
            format!("  {:<32} SKIP-REQ requires:jq", "t.sh")
        );
        let re = Regex::new(r"^\s+test-\S+\.sh\s+(ok|RED|SKIPPED)").unwrap();
        assert!(re.is_match(&suite_line("test-a.sh", &mk(Status::Ok, Some(0), 7))));
        assert!(!re.is_match(&suite_line("test-a.sh", &mk(Status::Unreached, None, 0))));
    }

    #[test]
    fn output_normalisation_matches_bash_capture() {
        assert_eq!(normalize_output("a\nb\n\n\n"), "a\nb\n");
        assert_eq!(normalize_output(""), "\n");
    }
}
