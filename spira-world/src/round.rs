//! Round-VM awareness for `world.sh stop`/`status` (sp-2bkpn).
//!
//! On 2026-09-30 a `world.sh stop` interrupted a Concierge round mid-corpus on the round
//! VM after its suites had started: the halt had no idea a round existed, so it neither
//! reported one nor offered to wait. `round-vm status` already answers the question this
//! needs — three lines: `ready: <handle> <addr>|none`, `provisioning: pid <pid>|none`,
//! `outage: <reason>|none` — so this module reads that, never round-vm's own state file
//! directly (the same "ask the tool, don't open its store" discipline every other caller
//! here follows).
//!
//! WHAT THIS DOES NOT CLAIM TO FIX. `provisioning: pid <pid>` names a VM being acquired or
//! actively driven through a corpus by that pid; it is the one signal `round-vm status`
//! exposes that distinguishes "a round is using the pool right now" from "the pool is
//! merely empty." It is not a complete account of round-vm's lease ownership — a round
//! that already finished acquiring and is mid-corpus with no further provisioning underway
//! would not be named by this alone. Closing that gap is round-vm's own reconciliation
//! (`release_owned_by`), out of this wave's four scripts; this module's job is narrower and
//! is exactly what the acceptance criteria ask for: name what `status` can see, and give an
//! operator a way to wait for it.

use std::process::Command;
use std::time::Duration;

/// One `round-vm status` reading, parsed. `None` fields are "none" or absent (`round-vm`
/// not on PATH, or it refused) — the caller decides what "unknown" means for its purpose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundVmStatus {
    pub ready: Option<String>,
    pub provisioning_pid: Option<String>,
    pub outage: Option<String>,
}

fn field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.trim();
        (v != "none" && !v.is_empty()).then(|| v.to_string())
    })
}

/// Parse `round-vm status`'s three lines. Pure, so the in-flight decision below is testable
/// without spawning anything.
pub fn parse(text: &str) -> RoundVmStatus {
    RoundVmStatus {
        ready: field(text, "ready:"),
        provisioning_pid: field(text, "provisioning:").map(|p| p.trim_start_matches("pid ").to_string()),
        outage: field(text, "outage:"),
    }
}

/// A one-line description of the in-flight round, or `None` if `round-vm status` shows
/// nothing provisioning right now.
pub fn in_flight_description(s: &RoundVmStatus) -> Option<String> {
    s.provisioning_pid.as_ref().map(|pid| format!("round-vm: provisioning under pid {pid} (a round is using the VM pool)"))
}

/// Run `round-vm status` and parse it. `None` if the binary is not on PATH or refused —
/// callers must treat that as "cannot tell," never as "no round," matching FAIL CLOSED.
pub fn read() -> Option<RoundVmStatus> {
    // batch-job: round-vm runs for as long as its work does
    let out = Command::new("round-vm").arg("status").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse(&String::from_utf8_lossy(&out.stdout)))
}

/// Wait up to `timeout` for `round-vm status` to show nothing provisioning, polling every
/// `poll` — `world stop --round-drain`'s loop. Returns `true` if it cleared, `false` on
/// timeout. `round-vm` not being on PATH clears immediately (nothing to wait for; FAIL
/// CLOSED belongs to the read, not to a wait with no subject).
///
/// `reader` is [`read`] itself in production — injected (sp-8bhnr, law-probe-a-fixture-
/// not-production) so a test can hand in a deterministic closure instead of this function
/// shelling to the REAL `round-vm` binary. Before this, the one test exercising "cleared"
/// called [`read`] directly: on a box with no `round-vm` on PATH it reads as clear by
/// construction, but on THIS box (round-vm ships in the release's own `bin/`, which is on
/// every dev/gate PATH once installed) it instead drove the real tool and got whatever its
/// actual, un-fixtured state happened to be — exactly the production state this function's
/// own `wait` is supposed to be tested against a FIXTURE of, never against.
pub fn wait_for_clear(
    timeout: Duration,
    poll: Duration,
    mut sleep: impl FnMut(Duration),
    mut now: impl FnMut() -> std::time::Instant,
    mut reader: impl FnMut() -> Option<RoundVmStatus>,
) -> bool {
    let deadline = now() + timeout;
    loop {
        match reader() {
            Some(s) if in_flight_description(&s).is_some() => {
                if now() >= deadline {
                    return false;
                }
                sleep(poll);
            }
            _ => return true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_all_three_fields() {
        let s = parse("ready: h1 10.0.0.1\nprovisioning: pid 4242\noutage: none\n");
        assert_eq!(s.ready, Some("h1 10.0.0.1".to_string()));
        assert_eq!(s.provisioning_pid, Some("4242".to_string()));
        assert_eq!(s.outage, None);
    }

    #[test]
    fn parse_of_all_none_is_all_none() {
        let s = parse("ready: none\nprovisioning: none\noutage: none\n");
        assert_eq!(s, RoundVmStatus::default());
    }

    #[test]
    fn in_flight_description_only_when_provisioning() {
        let idle = parse("ready: none\nprovisioning: none\noutage: none\n");
        assert_eq!(in_flight_description(&idle), None);
        let busy = parse("ready: none\nprovisioning: pid 99\noutage: none\n");
        assert_eq!(
            in_flight_description(&busy),
            Some("round-vm: provisioning under pid 99 (a round is using the VM pool)".to_string())
        );
    }

    /// sp-8bhnr, law-probe-a-fixture-not-production: `reader` stands in for [`read`] so
    /// this never shells to the real `round-vm` binary. Before this, the test called
    /// `read()` directly and relied on `round-vm` being ABSENT from PATH to get a
    /// deterministic `None` — true of a bare dev box, false of this one (and every gate
    /// run), once the release's own `bin/round-vm` is on it: it then drove the real tool
    /// and asserted against whatever un-fixtured state it actually reported.
    #[test]
    fn wait_for_clear_returns_true_immediately_when_the_reader_reports_nothing_in_flight() {
        use std::time::Instant;
        let t0 = Instant::now();
        let mut sleeps = 0;
        let ok = wait_for_clear(Duration::from_secs(5), Duration::from_millis(0), |_| sleeps += 1, move || t0, || None);
        assert!(ok, "a reader with nothing in flight must not hang a drain forever");
        assert_eq!(sleeps, 0, "the first read already clears; no poll should be needed");
    }

    /// The positive control the single-read test above cannot give: a reader that reports
    /// in-flight a few times before clearing must be polled exactly that many times, then
    /// return `true` — never timing out early, never lying about having waited.
    #[test]
    fn wait_for_clear_polls_until_the_reader_clears_then_returns_true() {
        use std::time::Instant;
        let t0 = Instant::now();
        let mut sleeps = 0;
        let mut reads = 0;
        let reader = move || {
            reads += 1;
            (reads <= 3).then(|| parse("ready: none\nprovisioning: pid 1\noutage: none\n"))
        };
        let ok = wait_for_clear(Duration::from_secs(5), Duration::from_millis(0), |_| sleeps += 1, move || t0, reader);
        assert!(ok, "a reader that eventually clears must return true, not time out");
        assert_eq!(sleeps, 3, "one sleep per in-flight read before the clearing read");
    }

    /// The other side of the same contract: a reader that NEVER clears must time out
    /// `false` once the deadline passes, not hang (there is no real round-vm to shell to
    /// in a test, so a reader that always reports in-flight is the fixture for "it never
    /// clears before the deadline").
    #[test]
    fn wait_for_clear_times_out_false_when_the_reader_never_clears() {
        use std::time::{Duration as D, Instant};
        let t0 = Instant::now();
        let mut now_calls = 0;
        let now = move || {
            now_calls += 1;
            // Called once to set the deadline, then once per loop iteration to check it —
            // answer past the deadline from the second call on, so this terminates without
            // a real sleep.
            if now_calls == 1 { t0 } else { t0 + D::from_secs(10) }
        };
        let ok = wait_for_clear(Duration::from_secs(5), Duration::from_millis(0), |_| {}, now, || {
            Some(parse("ready: none\nprovisioning: pid 1\noutage: none\n"))
        });
        assert!(!ok, "a reader that never clears before the deadline must time out, not hang or lie");
    }
}
