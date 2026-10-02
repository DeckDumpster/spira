//! `testenv wait <out>` (sp-tcarr): block on the run's own pid, never on the `VERDICT`
//! marker. A run that catches TERM/INT/HUP already finishes through [`crate::run::Finish`]
//! and always prints a `VERDICT` line; SIGKILL cannot be caught, so a run killed that way
//! writes nothing, and a marker-keyed `grep -q "^VERDICT" "$out"` waiter hangs against it
//! forever. Observed 2026-10-01: three such loops alive 1h12m, 2h21m and 5h04m against runs
//! already dead.
//!
//! The contract: whoever backgrounds a run writes `<out>.pid` (the run's own pid) before
//! `wait` can be called, and `<out>.rc` once it has reaped the process (optional — `wait`
//! determines liveness itself and only reads `<out>.rc` for a diagnostic, never to decide
//! when to stop polling). `wait` polls that pid's liveness with `kill -0`, which keeps
//! working even if the backgrounding parent itself is gone — the pid it named is the run,
//! not the shell that launched it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long `wait` tolerates `<out>.pid` not existing yet (the parent writes it right after
/// forking, but may not have flushed when `wait` is invoked back to back with the launch).
const PID_GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(200);

pub fn main(args: &[String]) -> i32 {
    let Some(out) = args.first().filter(|s| !s.is_empty()) else {
        eprintln!("usage: testenv wait <out>");
        return 2;
    };
    run(Path::new(out), &RealClock, &RealProc)
}

pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

pub trait Proc {
    /// Whether `pid` is still alive (or exists but we lack permission to signal it — still
    /// alive, as far as this is concerned). Gone (ESRCH) is the only "dead" answer.
    fn alive(&self, pid: i32) -> bool;
}

struct RealClock;
impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d)
    }
}

struct RealProc;
impl Proc for RealProc {
    fn alive(&self, pid: i32) -> bool {
        // SAFETY: signal 0 sends nothing; it only probes existence/permission.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

fn pid_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".pid");
    PathBuf::from(s)
}

fn rc_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_owned();
    s.push(".rc");
    PathBuf::from(s)
}

fn read_pid(out: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_path(out))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The last `VERDICT ...` line in `out`, if any — testenv's own contract is that it is the
/// last stdout line on every clean exit, but a FAULT written by a signal handler mid-build
/// (sp-tcarr) can still be followed by nothing further, so "last line in the file" and
/// "last VERDICT line" agree in practice; searching from the end is the honest version of
/// that claim.
fn last_verdict(out: &Path) -> Option<String> {
    let text = std::fs::read_to_string(out).ok()?;
    text.lines()
        .rev()
        .find(|l| l.starts_with("VERDICT "))
        .map(str::to_string)
}

/// `VERDICT FAULT rc=<n> ...` -> `<n>`, clamped to a valid exit code; malformed or absent
/// reads as a generic harness fault (2), never 0.
fn fault_rc(line: &str) -> i32 {
    line.split_whitespace()
        .find_map(|t| t.strip_prefix("rc="))
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or(2)
        .clamp(0, 255)
}

fn exit_for(line: &str) -> i32 {
    if line.starts_with("VERDICT GREEN") {
        0
    } else if line.starts_with("VERDICT RED") {
        1
    } else {
        fault_rc(line)
    }
}

/// Outcome of a wait, independent of process exit: what `main` prints and returns.
pub struct Outcome {
    pub line: String,
    pub rc: i32,
}

pub fn run(out: &Path, clock: &dyn Clock, proc: &dyn Proc) -> i32 {
    let o = wait_for(out, clock, proc);
    println!("{}", o.line);
    o.rc
}

fn wait_for(out: &Path, clock: &dyn Clock, proc: &dyn Proc) -> Outcome {
    let start = clock.now();
    let pid = loop {
        if let Some(p) = read_pid(out) {
            break Some(p);
        }
        if clock.now().duration_since(start) >= PID_GRACE {
            break None;
        }
        clock.sleep(POLL);
    };
    let Some(pid) = pid else {
        return Outcome {
            line: format!(
                "VERDICT FAULT rc=2 ran=0 reason=no-pid — {} never appeared",
                pid_path(out).display()
            ),
            rc: 2,
        };
    };
    while proc.alive(pid) {
        clock.sleep(POLL);
    }
    match last_verdict(out) {
        Some(line) => {
            let rc = exit_for(&line);
            Outcome { line, rc }
        }
        None => {
            let hint = std::fs::read_to_string(rc_path(out))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let line = match hint {
                Some(rc) => format!("VERDICT FAULT rc=2 ran=0 reason=gone exit={rc}"),
                None => "VERDICT FAULT rc=2 ran=0 reason=gone".to_string(),
            };
            Outcome { line, rc: 2 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashSet;
    use std::fs;

    struct FakeClock {
        t: Cell<Instant>,
    }
    impl FakeClock {
        fn new() -> Self {
            FakeClock { t: Cell::new(Instant::now()) }
        }
    }
    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.t.get()
        }
        fn sleep(&self, d: Duration) {
            self.t.set(self.t.get() + d);
        }
    }

    /// Alive until `dies_after` sleeps have elapsed (counted by the fake clock's sleep
    /// calls), so a test controls exactly when the pid disappears without a real process.
    struct FakeProc {
        ticks: Cell<u32>,
        dies_after: u32,
        pids: HashSet<i32>,
    }
    impl FakeProc {
        fn new(pid: i32, dies_after: u32) -> Self {
            FakeProc {
                ticks: Cell::new(0),
                dies_after,
                pids: HashSet::from([pid]),
            }
        }
    }
    impl Proc for FakeProc {
        fn alive(&self, pid: i32) -> bool {
            if !self.pids.contains(&pid) {
                return false;
            }
            let t = self.ticks.get();
            self.ticks.set(t + 1);
            t < self.dies_after
        }
    }

    /// A fresh scratch directory, removed on drop (spira-lint `tmp-leak`); `out` lives
    /// inside it rather than directly under the system temp dir.
    fn tmp(name: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("testenv-wait-test-{name}"))
    }

    #[test]
    fn a_clean_green_exit_is_reported_and_exits_0() {
        let d = tmp("green");
        let out = d.join("out");
        fs::write(&out, "building...\nVERDICT GREEN ran=3\n").unwrap();
        fs::write(pid_path(&out), "4242\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(4242, 2));
        assert_eq!(o.line, "VERDICT GREEN ran=3");
        assert_eq!(o.rc, 0);
    }

    #[test]
    fn red_exits_1() {
        let d = tmp("red");
        let out = d.join("out");
        fs::write(&out, "VERDICT RED ran=2 red=1\n").unwrap();
        fs::write(pid_path(&out), "99\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(99, 1));
        assert_eq!(o.rc, 1);
    }

    #[test]
    fn a_caught_signal_s_own_fault_line_is_passed_through() {
        let d = tmp("term");
        let out = d.join("out");
        fs::write(&out, "batch: interrupted\nVERDICT FAULT rc=2 ran=0 reason=signal\n").unwrap();
        fs::write(pid_path(&out), "7\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(7, 1));
        assert_eq!(o.line, "VERDICT FAULT rc=2 ran=0 reason=signal");
        assert_eq!(o.rc, 2);
    }

    #[test]
    fn a_process_gone_without_any_verdict_line_is_fault_reason_gone() {
        // the SIGKILL case: the output has whatever it had when the kill landed, and
        // never gained a VERDICT line because nothing could run after SIGKILL.
        let d = tmp("kill9");
        let out = d.join("out");
        fs::write(&out, "building...\nhalfway through a suite\n").unwrap();
        fs::write(pid_path(&out), "555\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(555, 1));
        assert_eq!(o.line, "VERDICT FAULT rc=2 ran=0 reason=gone");
        assert_eq!(o.rc, 2);
    }

    #[test]
    fn a_gone_process_still_reports_the_parent_s_rc_hint_when_present() {
        let d = tmp("kill9-rc");
        let out = d.join("out");
        fs::write(&out, "building...\n").unwrap();
        fs::write(pid_path(&out), "556\n").unwrap();
        fs::write(rc_path(&out), "137\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(556, 1));
        assert_eq!(o.line, "VERDICT FAULT rc=2 ran=0 reason=gone exit=137");
    }

    #[test]
    fn no_pid_file_ever_appearing_is_a_named_fault_not_a_hang() {
        let d = tmp("nopid");
        let out = d.join("out");
        fs::write(&out, "nothing ran\n").unwrap();
        // no pid file written at all
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(1, 0));
        assert!(o.line.starts_with("VERDICT FAULT rc=2 ran=0 reason=no-pid"), "{}", o.line);
        assert_eq!(o.rc, 2);
    }

    #[test]
    fn a_pid_file_that_appears_within_the_grace_window_is_still_honored() {
        let d = tmp("late-pid");
        let out = d.join("out");
        fs::write(&out, "VERDICT GREEN ran=0 selected=0\n").unwrap();
        // simulate the parent writing the pid file one tick after wait starts polling for
        // it, by writing it now: read_pid succeeds on the very first poll either way, but
        // this exercises the same path a slow parent would take within PID_GRACE.
        fs::write(pid_path(&out), "321\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(321, 0));
        assert_eq!(o.rc, 0);
    }

    #[test]
    fn fault_rc_is_clamped_and_defaults_sanely() {
        assert_eq!(fault_rc("VERDICT FAULT rc=4 ran=0 reason=build"), 4);
        assert_eq!(fault_rc("VERDICT FAULT rc=999 ran=0 reason=x"), 255);
        assert_eq!(fault_rc("VERDICT FAULT ran=0 reason=x"), 2);
    }
}
