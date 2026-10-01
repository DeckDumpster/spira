//! The capacity pause (family K, wave4-decomposition.md's "API capacity pause" row) —
//! wave 4.26. lib.sh's `capacity_*` functions ported whole, aeon the owning crate.
//!
//! ONE PROBE OWNER. Before this bead, `capacity_paused` (lib.sh) was reached identically
//! by aeon, sentinel and archivist — each shelled into the same bash function, and each
//! could therefore run `capacity_probe_maybe`/delete the pause file on its own. Three
//! processes probing is three times the cost for the same answer
//! (wave4-decomposition.md (c)3: "if both callers' ports each probe, the cost doubles").
//! [`check_and_probe`] is now the only function that ever calls [`probe`] or clears a
//! stale pause file, and only aeon's own call sites (`run.rs`, `sweep.rs`, `escape.rs`)
//! pass it a real [`ProbeCfg`]. Every other reader — sentinel's summon gate, cockpit-
//! collect's probe, archivist's sweep, `capacity.sh` — calls [`pause_state`] directly: a
//! plain read that never mutates the file and never spends a probe.
//!
//! FAIL CLOSED, NEVER OPEN. The bash original's `capacity_pause_until` printed "0" (the
//! window is open) for BOTH "no pause file exists" and "the pause file exists but could
//! not be read or parsed" — two different facts collapsed onto the one answer a caller
//! cannot tell apart. An operator mid-`chmod` on `$SPIRA_RUN`, or a half-written pause
//! file, would have read as "open" and summoned straight into an exhausted account
//! (wave4-decomposition.md (c)3's own worry, in so many words). [`PauseState::Unknown`]
//! keeps the two apart: every caller below treats it as paused, never as open.
//!
//! [`reset_at`] keeps the same discipline on the other side of the same contract: a
//! session that died to a capacity refusal with a missing or unparseable `resetsAt` is
//! still a refusal (`Some(0)`, not `None` — `None` means "not a refusal at all", which
//! would let the bead be charged an attempt it should never pay). [`pause_set`] then
//! treats that `0` the same way the bash version's `${at:-0}` guard did: not "no limit",
//! but "fall back to `SPIRA_CAPACITY_BACKOFF`" — paused regardless.

use std::io::{ErrorKind, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::conf::Conf;
use crate::ledger;
use crate::ports::Exec;
use crate::util::iso_utc;

// =========================================================================================
// THE PAUSE FILE — the contract every reader shares.
// =========================================================================================

/// `capacity_pause_until` + `capacity_pause_why`'s honest answer, read together (one file,
/// one parse). See the module doc for why `Unknown` is its own case rather than folded
/// into `Open`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PauseState {
    /// No pause file at all — the common, steady state once the window reopens.
    Open,
    /// A pause recorded to run until `until` (epoch), for `why`.
    Paused { until: i64, why: String },
    /// The file exists but could not be read or parsed. NEVER treat this as `Open`.
    Unknown,
}

impl PauseState {
    /// Seconds remaining — `Some` only once an epoch is both known and still ahead of
    /// `now`. `Unknown` and a `Paused` epoch already in the past both answer `None` here;
    /// callers that must fail closed on `Unknown` match on the enum directly instead (see
    /// [`check_and_probe`]/[`Paused`]).
    pub fn left(&self, now: i64) -> Option<i64> {
        match self {
            PauseState::Paused { until, .. } if *until > now => Some(until - now),
            _ => None,
        }
    }
}

/// `capacity_pause_until`/`capacity_pause_why`, as one read. The file is one line:
/// `<epoch> <iso-stamp> <why...>`; `why` may itself contain spaces, so only the first two
/// fields are stripped (bash's own `awk '{$1="";$2="";...}'`).
pub fn pause_state(pause_file: &Path) -> PauseState {
    let text = match std::fs::read_to_string(pause_file) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return PauseState::Open,
        Err(_) => return PauseState::Unknown,
    };
    let first = text.lines().next().unwrap_or("");
    let Some(at) = first.split_whitespace().next().and_then(|s| s.parse::<i64>().ok()) else {
        return PauseState::Unknown;
    };
    let why = first.splitn(3, ' ').nth(2).unwrap_or("").trim().to_string();
    PauseState::Paused { until: at, why }
}

/// `capacity_pause_why` alone (capacity.sh's `status`): `None` when there is no pause or
/// no recorded reason.
pub fn pause_why(pause_file: &Path) -> Option<String> {
    match pause_state(pause_file) {
        PauseState::Paused { why, .. } if !why.is_empty() => Some(why),
        _ => None,
    }
}

// =========================================================================================
// WRITING THE PAUSE — announced once, extended, never shortened.
// =========================================================================================

/// `capacity_pause_set <epoch> <reason>`. An existing pause is only ever EXTENDED, so a
/// second session dying into the same outage cannot pull the reopening forward to its own
/// — older — reading of `resetsAt`. Available to every caller (aeon, archivist, and
/// `capacity.sh pause`), unlike [`check_and_probe`]: recording evidence is not probing.
///
/// Returns the `CAPACITY: ...` line the caller should log, or `None` when the guard above
/// left the file untouched (bash's own early `return 0`, silent).
pub fn pause_set(pause_file: &Path, ledger_file: &Path, now: i64, backoff: i64, at: i64, why: &str) -> Option<String> {
    let at = if at > now { at } else { now + backoff };
    let cur = match pause_state(pause_file) {
        PauseState::Paused { until, .. } => until,
        // Open, or Unknown — a pause this process cannot verify is written over in good
        // faith; the alternative (refusing to write because the old one cannot be read)
        // would leave an account outage unrecorded.
        _ => 0,
    };
    if cur >= at {
        return None;
    }
    if let Some(parent) = pause_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(pause_file, format!("{at} {} {why}\n", iso_utc(at)));
    if let Some(parent) = ledger_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(ledger_file) {
        let _ = writeln!(f, "{} CAPACITY paused until {} {why}", iso_utc(now), iso_utc(at));
    }
    Some(format!(
        "CAPACITY: the account is out until {}Z ({}s) — summoning is paused, {why} returned unchanged",
        &iso_utc(at)[11..16],
        at - now
    ))
}

// =========================================================================================
// THE OWNER'S CHECK — the only place a probe runs or a stale file is cleared.
// =========================================================================================

/// What [`check_and_probe`] decided — independent of how it got there, so the decision
/// itself is covered by unit tests without a real pause file or a real agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paused {
    Open,
    Paused(i64),
    /// The pause file could not be read or parsed. A caller renders this as `?`, never as
    /// a number, and treats it exactly like `Paused` for any decision that gates
    /// summoning (fail closed — see the module doc).
    Unknown,
}

pub struct Verdict {
    pub state: Paused,
    /// Lines `capacity_paused` would have printed via lib.sh's `log` — plain text; the
    /// caller's own `log()` adds the timestamp (matching every other call site in this
    /// crate).
    pub log: Vec<String>,
    /// The pause file should be removed: the window reopened on its own, or a probe
    /// lifted it early. Harmless to skip (every reader already compares `until > now`
    /// itself — see [`PauseState::left`]); kept for the same tidiness bash's version had.
    pub clear_file: bool,
}

/// What a probe needs: the agent binary, its model, and the shared interval-gate file
/// that keeps repeated calls to this same function (one per summon attempt) from paying
/// for a probe more than once per `interval_secs`.
pub struct ProbeCfg<'a> {
    pub exec: &'a dyn Exec,
    pub agent_bin: &'a str,
    pub model: &'a str,
    pub timeout_secs: u64,
    pub interval_secs: i64,
    pub probe_last_file: &'a Path,
}

/// `capacity_paused`, run only from the process that owns the probe (aeon). `probe` is
/// `Some` on every real aeon call site; a caller that passes `None` gets the file's plain
/// answer with no probing at all — never wrong, just never early.
pub fn paused(pause_file: &Path, now: i64, probe_window: i64, probe: Option<&ProbeCfg>) -> Verdict {
    match pause_state(pause_file) {
        PauseState::Unknown => Verdict {
            state: Paused::Unknown,
            log: vec!["CAPACITY: the pause file could not be read or parsed — treating the account as paused (failing closed, never as open)".into()],
            clear_file: false,
        },
        PauseState::Open => Verdict { state: Paused::Open, log: vec![], clear_file: false },
        PauseState::Paused { until, .. } => {
            let left = until - now;
            if left <= 0 {
                return Verdict { state: Paused::Open, log: vec!["CAPACITY: the window has reopened — summoning resumes".into()], clear_file: true };
            }
            if left > probe_window {
                if let Some(p) = probe {
                    if probe_maybe(p, now) {
                        return Verdict {
                            state: Paused::Open,
                            log: vec![format!("CAPACITY: probe served — pause lifted early (horizon was {left}s out)")],
                            clear_file: true,
                        };
                    }
                }
            }
            Verdict { state: Paused::Paused(left), log: vec![], clear_file: false }
        }
    }
}

/// `Conf`-driven convenience for aeon's own call sites: builds the `ProbeCfg` from the
/// resolved config (law-a-binary-resolves-the-config-it-reads: every knob below comes
/// from `conf`, never straight from the environment) and runs [`paused`].
pub fn check_and_probe(conf: &Conf, exec: &dyn Exec, now: i64) -> Verdict {
    let agent = conf.agent();
    let model = conf.capacity_probe_model();
    let probe_last = conf.capacity_probe_last();
    let probe_cfg = ProbeCfg {
        exec,
        agent_bin: &agent,
        model: &model,
        timeout_secs: conf.capacity_probe_timeout(),
        interval_secs: conf.capacity_probe_interval(),
        probe_last_file: &probe_last,
    };
    paused(&conf.capacity_pause(), now, conf.capacity_probe_window(), Some(&probe_cfg))
}

/// `capacity_probe_maybe`: the timestamp is written BEFORE the probe runs, so a process
/// killed mid-probe still respects the interval on the next call instead of looping
/// (law-bound-the-rare-path — the bash version's own comment, carried over unchanged).
fn probe_maybe(p: &ProbeCfg, now: i64) -> bool {
    let last = std::fs::read_to_string(p.probe_last_file)
        .ok()
        .and_then(|t| t.split_whitespace().next().and_then(|s| s.parse::<i64>().ok()))
        .unwrap_or(0);
    if now - last < p.interval_secs {
        return false;
    }
    if let Some(parent) = p.probe_last_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p.probe_last_file, format!("{now} {}\n", iso_utc(now)));
    probe(p.exec, p.agent_bin, p.model, p.timeout_secs)
}

/// `capacity_probe`: `timeout <secs> <agent> -p --model <model>` — the external
/// `timeout(1)` wrapper, exactly as the bash original invoked it, rather than a
/// Rust-level per-call timeout plumbed through `Exec` (which every other call in this
/// crate leaves unbounded, `ports::RealExec{timeout: None}`). A timeout or any non-zero
/// exit is a refusal, never evidence the account is open.
fn probe(exec: &dyn Exec, agent_bin: &str, model: &str, timeout_secs: u64) -> bool {
    let out = exec.exec(
        "timeout",
        &[timeout_secs.to_string(), agent_bin.to_string(), "-p".to_string(), "--model".to_string(), model.to_string()],
        Some(b"ok".to_vec()),
        None,
    );
    out.code == 0
}

// =========================================================================================
// capacity_reset_at — the OTHER side of the contract: did THIS session die to a refusal.
// =========================================================================================

/// `capacity_reset_at <logfile>`: `Some(epoch)` (`Some(0)` if the refusal carried no
/// `resetsAt` at all) if the LAST attempt's segment ended in the account refusing the
/// session; `None` for anything else — "the log does not exist", "the log is
/// unparseable" and "the session failed for its own reasons" ALIKE, on purpose: reading a
/// genuine failure as an outage would stop a bead ever being poisoned (lib.sh's own
/// comment, carried over unchanged — that is the property CHECK 4 holds).
pub fn reset_at(logfile: &Path, mark: &str) -> Option<i64> {
    let meta = std::fs::metadata(logfile).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let seg = ledger::attempt_trace(logfile, 0, mark);
    let text = String::from_utf8_lossy(&seg);

    const LIMIT_TEXT: [&str; 3] = ["hit your session limit", "usage limit", "rate limit"];
    let (mut reset, mut hit) = (0i64, false);
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(d) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if !d.is_object() {
            continue;
        }
        match d.get("type").and_then(|t| t.as_str()) {
            Some("rate_limit_event") => {
                let info = d.get("rate_limit_info");
                if info.and_then(|i| i.get("status")).and_then(|s| s.as_str()) == Some("rejected") {
                    hit = true;
                    if let Some(r) = info.and_then(|i| i.get("resetsAt")).and_then(|v| v.as_i64()) {
                        reset = reset.max(r);
                    }
                }
            }
            Some("result") if d.get("is_error").and_then(|b| b.as_bool()) == Some(true) => {
                let text_field = d.get("result").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                if LIMIT_TEXT.iter().any(|t| text_field.contains(t)) {
                    hit = true;
                }
            }
            _ => {}
        }
    }
    hit.then_some(reset)
}

// =========================================================================================
// THE WITHDRAWAL LEDGER — which refusal has already been paid back (capacity.sh).
// =========================================================================================

/// `capacity_log_fingerprint`: content, not `stat` — a size/mtime check would read an
/// unchanged log as new after any copy/restore/re-sync, spending a withdrawal that was
/// never earned. sha2 replaces bash's `sha256sum`/`cksum` fallback: both write and read
/// happen in this one crate now, so only internal consistency matters, never the
/// algorithm's name.
pub fn log_fingerprint(path: &Path) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    if data.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(&data);
    Some(format!("{:x}", h.finalize()))
}

/// `capacity_withdrawn_fp <id>`: the fingerprint already paid back, or `None`.
pub fn withdrawn_fp(dir: &Path, id: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(id)).ok().and_then(|t| t.lines().next().and_then(|l| l.split_whitespace().next()).map(str::to_string))
}

/// `capacity_withdrawn_mark <id> <fp> <attempt>`: written whole, one line per bead.
pub fn withdrawn_mark(dir: &Path, id: &str, fp: &str, attempt: i64, now: i64) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(id), format!("{fp} {attempt} {}\n", iso_utc(now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::sync::Mutex;

    fn dir() -> testkit::TempDir {
        testkit::TempDir::new("aeon-capacity")
    }

    // ---- pause_state: fail closed, never open --------------------------------------------

    #[test]
    fn no_file_is_open() {
        let d = dir();
        assert_eq!(pause_state(&d.join("capacity-pause")), PauseState::Open);
    }

    #[test]
    fn a_parseable_file_is_paused_with_its_reason() {
        let d = dir();
        let p = d.join("capacity-pause");
        std::fs::write(&p, "1750000000 2025-06-15T16:40:00Z archivist/sess-1 with spaces\n").unwrap();
        assert_eq!(pause_state(&p), PauseState::Paused { until: 1_750_000_000, why: "archivist/sess-1 with spaces".into() });
        assert_eq!(pause_why(&p), Some("archivist/sess-1 with spaces".into()));
    }

    #[test]
    fn unreadable_or_corrupt_is_unknown_never_open() {
        let d = dir();
        let p = d.join("capacity-pause");
        std::fs::write(&p, "not-a-number why\n").unwrap();
        assert_eq!(pause_state(&p), PauseState::Unknown, "corrupt content must not read as 0/open");

        // A directory where a file was expected: read_to_string fails with something other
        // than NotFound.
        let as_dir = d.join("is-a-dir");
        std::fs::create_dir_all(&as_dir).unwrap();
        assert_eq!(pause_state(&as_dir), PauseState::Unknown);
    }

    #[test]
    fn empty_file_is_unknown() {
        let d = dir();
        let p = d.join("capacity-pause");
        std::fs::write(&p, "").unwrap();
        assert_eq!(pause_state(&p), PauseState::Unknown);
    }

    // ---- pause_set: extend-only, backoff on an unparseable/missing reset ------------------

    #[test]
    fn pause_set_extends_but_never_shortens() {
        let d = dir();
        let pause = d.join("capacity-pause");
        let ledger = d.join("aeon-ledger.log");
        let now = 1_000_000i64;
        let line = pause_set(&pause, &ledger, now, 900, now + 1000, "first").unwrap();
        assert!(line.contains("1000s"), "{line}");
        assert_eq!(pause_state(&pause), PauseState::Paused { until: now + 1000, why: "first".into() });

        // A SHORTER reopening is refused — the existing, later epoch wins.
        assert!(pause_set(&pause, &ledger, now, 900, now + 500, "second").is_none());
        assert_eq!(pause_state(&pause), PauseState::Paused { until: now + 1000, why: "first".into() });

        // A LATER one extends.
        let line2 = pause_set(&pause, &ledger, now, 900, now + 2000, "third").unwrap();
        assert!(line2.contains("2000s"));
        assert_eq!(pause_state(&pause), PauseState::Paused { until: now + 2000, why: "third".into() });

        let ledger_text = std::fs::read_to_string(&ledger).unwrap();
        assert_eq!(ledger_text.lines().count(), 2, "the refused write appends nothing: {ledger_text}");
        assert!(ledger_text.contains("CAPACITY paused until"));
    }

    #[test]
    fn pause_set_fails_closed_on_an_unparseable_or_missing_reset_time() {
        // reset_at's `Some(0)` ("hit, but no known resetsAt") must still pause — the
        // bash original's `${at:-0}` guard, never treated as "no limit".
        let d = dir();
        let pause = d.join("capacity-pause");
        let ledger = d.join("aeon-ledger.log");
        let now = 1_000_000i64;
        let line = pause_set(&pause, &ledger, now, 900, 0, "unparseable reset").unwrap();
        assert!(line.contains("900s"), "falls back to the backoff, never 'no limit': {line}");
        assert_eq!(pause_state(&pause), PauseState::Paused { until: now + 900, why: "unparseable reset".into() });
    }

    // ---- reset_at: the LAST attempt only, and "hit but no resetsAt" is Some(0) -----------

    const MARK: &str = "=== spira attempt";

    #[test]
    fn missing_or_empty_log_is_none() {
        let d = dir();
        assert_eq!(reset_at(&d.join("missing.log"), MARK), None);
        let empty = d.join("empty.log");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(reset_at(&empty, MARK), None);
    }

    #[test]
    fn a_genuine_failure_is_none_not_a_refusal() {
        let d = dir();
        let p = d.join("a.log");
        std::fs::write(&p, "{\"type\":\"result\",\"is_error\":true,\"result\":\"some other error\"}\n").unwrap();
        assert_eq!(reset_at(&p, MARK), None);
    }

    #[test]
    fn a_rejected_rate_limit_event_with_resets_at_is_some_of_the_epoch() {
        let d = dir();
        let p = d.join("a.log");
        std::fs::write(&p, r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1750003600}}"#.to_string() + "\n").unwrap();
        assert_eq!(reset_at(&p, MARK), Some(1_750_003_600));
    }

    #[test]
    fn a_refusal_with_no_resets_at_is_some_zero_never_none() {
        let d = dir();
        let p = d.join("a.log");
        std::fs::write(&p, "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"rejected\"}}\n").unwrap();
        assert_eq!(reset_at(&p, MARK), Some(0), "a refusal with an unparseable/missing resetsAt is still a refusal");
    }

    #[test]
    fn the_terminal_result_text_is_the_fallback_for_a_refusal_without_a_rate_limit_event() {
        let d = dir();
        let p = d.join("a.log");
        std::fs::write(&p, "{\"type\":\"result\",\"is_error\":true,\"result\":\"You have hit your session limit, try later\"}\n").unwrap();
        assert_eq!(reset_at(&p, MARK), Some(0));
    }

    #[test]
    fn only_the_last_attempt_segment_is_read() {
        let d = dir();
        let p = d.join("a.log");
        std::fs::write(
            &p,
            format!(
                "{MARK} 1 aeon=a\n{{\"type\":\"rate_limit_event\",\"rate_limit_info\":{{\"status\":\"rejected\",\"resetsAt\":111}}}}\n{MARK} 2 aeon=b\n{{\"type\":\"result\",\"is_error\":false}}\n"
            ),
        )
        .unwrap();
        assert_eq!(reset_at(&p, MARK), None, "attempt 1's refusal must not leak into attempt 2's verdict");
    }

    #[test]
    fn a_429_in_a_token_count_is_not_a_bare_http_status() {
        // Regression for the substring trap lib.sh's own header warns about: a bare grep
        // for "429"/"503" matches token counts, never an HTTP status.
        let d = dir();
        let p = d.join("a.log");
        std::fs::write(&p, "{\"type\":\"result\",\"is_error\":false,\"cache_read_input_tokens\":142902}\n").unwrap();
        assert_eq!(reset_at(&p, MARK), None);
    }

    // ---- the owner's check: the four cases of test-capacity-probe.sh (retired, wave 4.26) -

    struct FakeExec(Mutex<Vec<String>>, i32);
    impl Exec for FakeExec {
        fn exec(&self, prog: &str, _args: &[String], _stdin: Option<Vec<u8>>, _cwd: Option<&Path>) -> Out {
            self.0.lock().unwrap().push(prog.to_string());
            if self.1 == 0 { Out::ok("") } else { Out::fail(self.1, "") }
        }
    }

    fn probe_cfg<'a>(exec: &'a FakeExec, probe_last: &'a Path) -> ProbeCfg<'a> {
        ProbeCfg { exec, agent_bin: "claude", model: "m", timeout_secs: 30, interval_secs: 0, probe_last_file: probe_last }
    }

    #[test]
    fn case0_positive_control_no_pause_probe_not_called() {
        let d = dir();
        let exec = FakeExec(Mutex::new(vec![]), 0);
        let probe_last = d.join("probe-last");
        let v = paused(&d.join("capacity-pause"), 1000, 300, Some(&probe_cfg(&exec, &probe_last)));
        assert_eq!(v.state, Paused::Open);
        assert!(exec.0.lock().unwrap().is_empty(), "probe must not run when nothing is paused");
    }

    #[test]
    fn case1_probe_served_lifts_the_pause_early() {
        let d = dir();
        let pause = d.join("capacity-pause");
        let now = 1_000_000i64;
        std::fs::write(&pause, format!("{} iso probe-test\n", now + 86_400)).unwrap(); // 24h out
        let exec = FakeExec(Mutex::new(vec![]), 0); // serves
        let probe_last = d.join("probe-last");
        let v = paused(&pause, now, 300, Some(&probe_cfg(&exec, &probe_last))); // window=5min
        assert_eq!(v.state, Paused::Open);
        assert!(v.clear_file);
        assert!(!exec.0.lock().unwrap().is_empty(), "probe WAS called");
    }

    #[test]
    fn case2_probe_refused_keeps_the_pause() {
        let d = dir();
        let pause = d.join("capacity-pause");
        let now = 1_000_000i64;
        std::fs::write(&pause, format!("{} iso probe-test\n", now + 86_400)).unwrap();
        let exec = FakeExec(Mutex::new(vec![]), 1); // refuses
        let probe_last = d.join("probe-last");
        let v = paused(&pause, now, 300, Some(&probe_cfg(&exec, &probe_last)));
        assert_eq!(v.state, Paused::Paused(86_400));
        assert!(!v.clear_file);
        assert!(!exec.0.lock().unwrap().is_empty(), "probe WAS called");
    }

    #[test]
    fn case3_close_horizon_never_probes() {
        let d = dir();
        let pause = d.join("capacity-pause");
        let now = 1_000_000i64;
        std::fs::write(&pause, format!("{} iso probe-test\n", now + 120)).unwrap(); // 2 min
        let exec = FakeExec(Mutex::new(vec![]), 0);
        let probe_last = d.join("probe-last");
        let v = paused(&pause, now, 300, Some(&probe_cfg(&exec, &probe_last))); // window=5min
        assert_eq!(v.state, Paused::Paused(120));
        assert!(exec.0.lock().unwrap().is_empty(), "probe NOT called — horizon is within the window");
    }

    #[test]
    fn a_reopened_window_clears_the_file_and_logs_resumption() {
        let d = dir();
        let pause = d.join("capacity-pause");
        std::fs::write(&pause, "500 iso why\n").unwrap();
        let v = paused(&pause, 1000, 300, None);
        assert_eq!(v.state, Paused::Open);
        assert!(v.clear_file);
        assert!(v.log.iter().any(|l| l.contains("window has reopened")));
    }

    #[test]
    fn unreadable_pause_file_gates_as_paused_not_open_even_without_a_probe() {
        let d = dir();
        let pause = d.join("capacity-pause");
        std::fs::write(&pause, "garbage\n").unwrap();
        let v = paused(&pause, 1000, 300, None);
        assert_eq!(v.state, Paused::Unknown, "fail closed: never Open on an unreadable file");
    }

    #[test]
    fn the_probe_interval_gates_repeated_probing() {
        let d = dir();
        let pause = d.join("capacity-pause");
        let now = 1_000_000i64;
        std::fs::write(&pause, format!("{} iso why\n", now + 86_400)).unwrap();
        let probe_last = d.join("probe-last");
        std::fs::write(&probe_last, format!("{now}\n")).unwrap(); // just probed
        let exec = FakeExec(Mutex::new(vec![]), 0);
        let cfg = ProbeCfg { exec: &exec, agent_bin: "claude", model: "m", timeout_secs: 30, interval_secs: 3600, probe_last_file: &probe_last };
        let v = paused(&pause, now + 10, 300, Some(&cfg)); // only 10s later, interval is 1h
        assert_eq!(v.state, Paused::Paused(86_390));
        assert!(exec.0.lock().unwrap().is_empty(), "within the interval — no second probe");
    }

    // ---- fingerprint / withdrawal ledger --------------------------------------------------

    #[test]
    fn fingerprint_is_stable_and_empty_file_has_none() {
        let d = dir();
        let p = d.join("a.log");
        std::fs::write(&p, "same content\n").unwrap();
        let fp1 = log_fingerprint(&p).unwrap();
        let fp2 = log_fingerprint(&p).unwrap();
        assert_eq!(fp1, fp2);
        std::fs::write(&p, "").unwrap();
        assert_eq!(log_fingerprint(&p), None);
    }

    #[test]
    fn withdrawn_mark_and_fp_round_trip() {
        let d = dir();
        let wd = d.join("capacity-withdrawn");
        assert_eq!(withdrawn_fp(&wd, "sp-x"), None);
        withdrawn_mark(&wd, "sp-x", "abc123", 2, 1_000_000).unwrap();
        assert_eq!(withdrawn_fp(&wd, "sp-x"), Some("abc123".into()));
    }
}
