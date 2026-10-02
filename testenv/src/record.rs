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
    /// Cut by `--deadline` (DESIGN.md D7): killed while running, or never started.
    /// Neither executed nor blocking.
    Deferred,
    /// podman's own exec lost the suite's exit status (conmon's exit-file wait gave up
    /// under load, sp-3azqi) — the suite ran, but whether it passed is unknown. Never a
    /// red: a batch with a fault reports VERDICT FAULT, not RED, and names it.
    Fault,
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
            Status::Deferred => "deferred",
            Status::Fault => "fault",
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
            "deferred" => Status::Deferred,
            "fault" => Status::Fault,
            _ => return None,
        })
    }

    /// The suite's script actually ran inside the container (whatever its outcome) — a
    /// fault counts: podman lost the exit status, not the fact that the process ran.
    pub fn executed(self) -> bool {
        matches!(
            self,
            Status::Ok
                | Status::Skip
                | Status::Red
                | Status::Timeout
                | Status::QuarantinedRed
                | Status::Fault
        )
    }

    /// Counts against the verdict: a non-quarantined red or timeout. A fault is never
    /// blocking in this sense — it has its own VERDICT FAULT path (DESIGN.md §4.3,
    /// sp-3azqi), never folded into a suite-defect RED.
    pub fn blocking(self) -> bool {
        matches!(self, Status::Red | Status::Timeout)
    }

    /// podman's own exec failed to collect the exit status, not the suite failing.
    pub fn is_fault(self) -> bool {
        matches!(self, Status::Fault)
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

    /// A suite cut by the deadline: `killed` after `secs` of running (fingerprint
    /// `deadline:<suite>`), or never started (`secs` 0, fingerprint `-`). rc is always `-`.
    pub fn deferred(
        suite: &str,
        killed_after: Option<u64>,
        mode: Mode,
        producer: Producer,
        epoch: u64,
    ) -> Self {
        ResultRecord {
            status: Status::Deferred,
            epoch,
            secs: killed_after.unwrap_or(0),
            fingerprint: match killed_after {
                Some(_) => format!("deadline:{suite}"),
                None => "-".into(),
            },
            mode: Some(mode),
            producer: Some(producer),
            rc: None,
        }
    }

    /// The record for a suite that ran and exited `rc` after `secs`, given its output and
    /// whether it is quarantined. 0 = ok, 77 = skip (automake), 124 = the per-suite timeout.
    /// rc 255 with podman's own exec-wait-timeout signature (sp-3azqi) is a fault —
    /// checked ahead of quarantine, because quarantine excuses a suite's own flakiness,
    /// never a harness problem the suite had nothing to do with.
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
        let (status, fingerprint) = if is_podman_exec_lost(rc, output) {
            (Status::Fault, "fault:podman-exec-lost".to_string())
        } else {
            match rc {
                0 => (Status::Ok, "-".to_string()),
                77 => (Status::Skip, skip_fingerprint(output)),
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
            }
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
        // An undeclared SKIP/SKIP-REQ reclassified by the skip contract (DESIGN.md §3.7):
        // still a plain `red` on disk (gate-diag.sh, round.sh need no new keyword), but the
        // line names the requirement instead of a meaningless rc/secs pair.
        Status::Red if rec.fingerprint.starts_with("skip:") || rec.fingerprint.starts_with("requires:") => {
            format!("RED     undeclared skip — {}", rec.fingerprint)
        }
        Status::Red => format!("RED     rc={rc} after {}s", rec.secs),
        Status::QuarantinedRed
            if rec.fingerprint.starts_with("skip:") || rec.fingerprint.starts_with("requires:") =>
        {
            format!("QUARANTINED-RED  undeclared skip — {}", rec.fingerprint)
        }
        Status::QuarantinedRed if rc == 124 => {
            format!("QUARANTINED-RED  TIMEOUT after {}s", rec.secs)
        }
        Status::QuarantinedRed => format!("QUARANTINED-RED  rc={rc} after {}s", rec.secs),
        Status::Unreached => "UNREACHED".to_string(),
        Status::Deferred if rec.fingerprint.starts_with("deadline:") => {
            format!("DEFERRED deadline after {}s", rec.secs)
        }
        Status::Deferred => "DEFERRED deadline".to_string(),
        Status::Fault => format!(
            "FAULT   podman lost the exit status after {}s (rc={rc}) — not a suite defect",
            rec.secs
        ),
    };
    format!("  {suite:<32} {tail}")
}

/// podman's own exec gave up waiting for conmon's exit file under load (sp-3azqi): the
/// container is fine and the suite may well have finished (its output, if any, is kept
/// verbatim), but podman's CLI never read back an exit status, so `rc` is its own 255, not
/// the suite's. Matched on the exact message `oci_conmon_exec_linux.go`'s `waitForFile`
/// prints, not merely `rc == 255` (a suite could in principle exit 255 on its own) —
/// `runtime.rs`'s `Podman::exec` already tries to recover the real status by reading that
/// same exit file itself before giving up and leaving this signature in `output` at all.
pub fn is_podman_exec_lost(rc: i32, output: &str) -> bool {
    rc == 255
        && output.contains("Error: timed out waiting for file")
        && output.contains("/exit/")
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

/// Path/timestamp/number noise stripped from one line — shared by the red fingerprint
/// (below) and the skip requirement key (DESIGN.md §3.7): a tmp path, a PID-suffixed name or
/// a timestamp must not make two runs of the same skip, or the same failure, look distinct.
pub fn normalize_line(line: &str) -> String {
    let mut cur = line.to_string();
    for (re, rep) in norm_rules() {
        cur = re.replace_all(&cur, *rep).into_owned();
    }
    cur
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
        normalized.push_str(&normalize_line(body));
        normalized.push_str(nl);
    }
    let (crc, len) = cksum(normalized.as_bytes());
    format!("{crc}{len}")
}

/// The fingerprint for a suite that exited 77 (DESIGN.md §3.7): `skip:<requirement>`, where
/// `<requirement>` is testlib.sh's `skip <reason>` text (tap.rs's skip-all form), normalized
/// and space-joined with `_` — the `.result` line is `<status> <epoch> <secs> <fingerprint>
/// <mode> <producer> <rc>` (§3.1), seven fields split on whitespace, and a reason is free
/// text that would otherwise shift every field after it. A suite that exits 77 without the
/// convention gets a fixed, always-undeclarable key, so a caller cannot skip silently by
/// simply omitting the reason.
pub fn skip_fingerprint(output: &str) -> String {
    match crate::tap::skip_all_reason(output) {
        Some(reason) if !reason.trim().is_empty() => {
            let words: Vec<String> = normalize_line(reason.trim())
                .split_whitespace()
                .map(str::to_string)
                .collect();
            format!("skip:{}", words.join("_"))
        }
        _ => "skip:(no_reason_given)".to_string(),
    }
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
            fingerprint(3, "path /var/tmp/u/sptest_abc/x/sptest_def_9 end"),
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

    /// sp-3azqi positive control: seen red first (before this fix, rc=255 fell into the
    /// catch-all `_` arm and read as an ordinary Red, exactly what the VM certification
    /// run misreported for test-strand-reclaim-n.sh and test-skew-refresh.sh). A fake
    /// podman returning rc 255 with its own exec-wait-timeout message — even with the
    /// suite's own fully-passing TAP output ahead of it, the real shape podman left behind
    /// — now yields Fault, never Red, and is never blocking or quarantinable.
    #[test]
    fn podman_exec_wait_timeout_is_a_fault_never_a_red() {
        let evidence = "TAP version 14\n1..4\nok 1 - a\nok 2 - b\nok 3 - c\nok 4 - d\n\n4 passed, 0 failed, 0 skipped\nError: timed out waiting for file /var/lib/containers/storage/overlay-containers/d609b04df0f16dfeda8af8adf28901c5d60de417ee573f9a66bfadf26b14e4f6/userdata/341cfdc06753bbf5d930ce5f9764fe6b5925ed6791e5bccfcb18cc41dc1d50ed/exit/d609b04df0f16dfeda8af8adf28901c5d60de417ee573f9a66bfadf26b14e4f6\n";
        assert!(is_podman_exec_lost(255, evidence));
        let rec = ResultRecord::from_exit(
            255,
            246,
            evidence,
            "test-strand-reclaim-n.sh",
            false,
            Mode::Parallel,
            Producer::Explicit,
            1_790_897_469,
        );
        assert_eq!(rec.status, Status::Fault);
        assert!(!rec.status.blocking());
        assert!(rec.status.executed());
        assert_eq!(rec.fingerprint, "fault:podman-exec-lost");
        // quarantine never excuses (or claims) a harness fault as the suite's own flake
        let quarantined = ResultRecord::from_exit(
            255, 246, evidence, "test-strand-reclaim-n.sh", true, Mode::Parallel,
            Producer::Explicit, 1_790_897_469,
        );
        assert_eq!(quarantined.status, Status::Fault);
        let line = rec.to_string();
        assert!(line.starts_with("fault 1790897469 246 fault:podman-exec-lost"));
        assert!(line.ends_with(" parallel explicit 255"));
        assert_eq!(ResultRecord::parse(&line), Some(rec.clone()));
        assert_eq!(
            suite_line("test-strand-reclaim-n.sh", &rec),
            format!(
                "  {:<32} FAULT   podman lost the exit status after 246s (rc=255) — not a suite defect",
                "test-strand-reclaim-n.sh"
            )
        );
    }

    #[test]
    fn is_podman_exec_lost_needs_both_the_rc_and_the_exact_podman_signature() {
        // rc 255 alone (a suite legitimately exiting 255) is not enough
        assert!(!is_podman_exec_lost(255, "not ok 1 - boom\n"));
        // the message alone, at a different rc, is not enough either
        assert!(!is_podman_exec_lost(
            1,
            "Error: timed out waiting for file /var/.../exit/abc\n"
        ));
        assert!(!is_podman_exec_lost(
            255,
            "Error: timed out waiting for file /no/such/marker/here\n"
        ));
    }

    #[test]
    fn skip_fingerprint_names_the_reason_and_normalizes_it() {
        let rec = ResultRecord::from_exit(
            77,
            0,
            "1..0 # SKIP no dolt on this host\n",
            "s.sh",
            false,
            Mode::Serial,
            Producer::Explicit,
            0,
        );
        assert_eq!(rec.fingerprint, "skip:no_dolt_on_this_host");
        // a path in the reason is normalized exactly like a red's fingerprint
        let rec = ResultRecord::from_exit(
            77,
            0,
            "1..0 # SKIP not found at /tmp/sptest_abc123\n",
            "s.sh",
            false,
            Mode::Serial,
            Producer::Explicit,
            0,
        );
        assert_eq!(rec.fingerprint, "skip:not_found_at_/tmp/X");
    }

    #[test]
    fn skip_without_the_tap_convention_gets_a_fixed_unclaimable_fingerprint() {
        let rec = ResultRecord::from_exit(
            77, 0, "exiting early\n", "s.sh", false, Mode::Serial, Producer::Explicit, 0,
        );
        assert_eq!(rec.fingerprint, "skip:(no_reason_given)");
    }

    #[test]
    fn an_undeclared_skip_reclassified_red_names_the_requirement_in_its_line() {
        let rec = ResultRecord {
            status: Status::Red,
            epoch: 0,
            secs: 3,
            fingerprint: "skip:no widget available".into(),
            mode: Some(Mode::Parallel),
            producer: Some(Producer::Diff),
            rc: Some(77),
        };
        assert_eq!(
            suite_line("test-w.sh", &rec),
            format!("  {:<32} RED     undeclared skip — skip:no widget available", "test-w.sh")
        );
        let rec2 = ResultRecord {
            fingerprint: "requires:claude".into(),
            ..rec.clone()
        };
        assert!(suite_line("test-w.sh", &rec2).contains("undeclared skip — requires:claude"));
        // an ordinary assertion red (numeric fingerprint) keeps the rc/secs line
        let ordinary = ResultRecord { fingerprint: "123456".into(), rc: Some(1), ..rec };
        assert_eq!(
            suite_line("test-w.sh", &ordinary),
            format!("  {:<32} RED     rc=1 after 3s", "test-w.sh")
        );
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
        // an ordinary assertion red: a numeric (cksum) fingerprint, not "skip:"/"requires:"
        assert_eq!(
            suite_line(
                "test-a.sh",
                &ResultRecord {
                    fingerprint: "1234567".into(),
                    ..mk(Status::Red, Some(2), 7)
                }
            ),
            format!("  {:<32} RED     rc=2 after 7s", "test-a.sh")
        );
        assert_eq!(
            suite_line("test-a.sh", &mk(Status::Skip, Some(77), 1)),
            format!("  {:<32} SKIPPED", "test-a.sh")
        );
        assert_eq!(
            suite_line(
                "t.sh",
                &ResultRecord {
                    fingerprint: "1234567".into(),
                    ..mk(Status::QuarantinedRed, Some(124), 3)
                }
            ),
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
    fn deferred_records_are_neither_executed_nor_blocking_and_round_trip() {
        let killed =
            ResultRecord::deferred("test-p.sh", Some(61), Mode::Parallel, Producer::Explicit, 7);
        assert_eq!(
            killed.to_string(),
            "deferred 7 61 deadline:test-p.sh parallel explicit -"
        );
        assert_eq!(
            ResultRecord::parse(&killed.to_string()),
            Some(killed.clone())
        );
        let never = ResultRecord::deferred("test-z.sh", None, Mode::Serial, Producer::Diff, 8);
        assert_eq!(never.to_string(), "deferred 8 0 - serial diff -");
        assert!(!Status::Deferred.executed());
        assert!(!Status::Deferred.blocking());
        assert_eq!(
            suite_line("test-p.sh", &killed),
            format!("  {:<32} DEFERRED deadline after 61s", "test-p.sh")
        );
        assert_eq!(
            suite_line("test-z.sh", &never),
            format!("  {:<32} DEFERRED deadline", "test-z.sh")
        );
        // round.sh's per-suite regex must not count a deferred suite as run
        let re = Regex::new(r"^\s+test-\S+\.sh\s+(ok|RED|SKIPPED)").unwrap();
        assert!(!re.is_match(&suite_line("test-p.sh", &killed)));
    }

    #[test]
    fn output_normalisation_matches_bash_capture() {
        assert_eq!(normalize_output("a\nb\n\n\n"), "a\nb\n");
        assert_eq!(normalize_output(""), "\n");
    }
}
