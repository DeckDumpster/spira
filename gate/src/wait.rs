//! `gate wait <out>` (sp-tcarr): the gate's half of the same fix as `testenv wait` —
//! block on the run's own pid, never on its `gate: VERDICT=` line. TERM/INT/HUP are caught
//! (`real.rs::install_signal_handlers`) and the trial still meters and prints its line on
//! all three; SIGKILL cannot be caught, and a gate killed that way writes nothing, leaving
//! a marker-keyed `grep -q "VERDICT="` waiter stuck against a dead process forever.
//!
//! The contract matches testenv's: whoever backgrounds the run writes `<out>.pid` (the
//! gate process's own pid) up front, and may write `<out>.rc` once it has reaped it. `wait`
//! decides liveness itself via `kill -0`, so it never depends on the launching parent still
//! being around.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PID_GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(200);

pub fn main(args: &[String]) -> i32 {
    let Some(out) = args.first().filter(|s| !s.is_empty()) else {
        eprintln!("usage: gate wait <out>");
        return 2;
    };
    run(Path::new(out), &RealClock, &RealProc)
}

pub trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

pub trait Proc {
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

/// The last `gate: VERDICT=...` line in `out` (DESIGN.md "Exit status and the VERDICT
/// line"): gate writes it to stderr, so it lands in `out` only when the caller merged
/// stdout and stderr when backgrounding — the documented convention for this contract.
fn last_verdict(out: &Path) -> Option<String> {
    let text = std::fs::read_to_string(out).ok()?;
    text.lines()
        .rev()
        .find(|l| l.contains("gate: VERDICT="))
        .map(str::to_string)
}

/// `gate: VERDICT=PASS|FAIL|BASE_FAIL|NO_VERDICT ...` -> the matching exit code
/// (engine.rs PASS/FAIL/BASEFAIL/NOVERDICT); anything unrecognized reads as NO_VERDICT (75),
/// never a pass.
fn exit_for(line: &str) -> i32 {
    if line.contains("VERDICT=PASS") {
        crate::engine::PASS
    } else if line.contains("VERDICT=FAIL") {
        crate::engine::FAIL
    } else if line.contains("VERDICT=BASE_FAIL") {
        crate::engine::BASEFAIL
    } else {
        crate::engine::NOVERDICT
    }
}

pub struct Outcome {
    pub line: String,
    pub rc: i32,
}

pub fn run(out: &Path, clock: &dyn Clock, proc: &dyn Proc) -> i32 {
    let o = wait_for(out, clock, proc);
    eprintln!("{}", o.line);
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
                "gate: VERDICT=NO_VERDICT reason=no-pid — {} never appeared",
                pid_path(out).display()
            ),
            rc: crate::engine::NOVERDICT,
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
                Some(rc) => format!("gate: VERDICT=NO_VERDICT reason=gone exit={rc}"),
                None => "gate: VERDICT=NO_VERDICT reason=gone".to_string(),
            };
            Outcome {
                line,
                rc: crate::engine::NOVERDICT,
            }
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

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gate-wait-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn a_pass_is_reported_and_exits_0() {
        let out = tmp("pass");
        fs::write(&out, "gate: VERDICT=PASS reason=pass branch=spira/x repo=spira suite=-\n").unwrap();
        fs::write(pid_path(&out), "42\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(42, 1));
        assert_eq!(o.rc, PASS_RC);
        assert!(o.line.contains("VERDICT=PASS"));
    }

    const PASS_RC: i32 = 0;

    #[test]
    fn a_fail_exits_1() {
        let out = tmp("fail");
        fs::write(&out, "gate: VERDICT=FAIL reason=branch-red branch=x repo=spira suite=test-a.sh\n").unwrap();
        fs::write(pid_path(&out), "7\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(7, 1));
        assert_eq!(o.rc, 1);
    }

    #[test]
    fn a_base_fail_exits_76() {
        let out = tmp("basefail");
        fs::write(&out, "gate: VERDICT=BASE_FAIL reason=base-red branch=x repo=spira suite=test-a.sh\n").unwrap();
        fs::write(pid_path(&out), "8\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(8, 1));
        assert_eq!(o.rc, 76);
    }

    #[test]
    fn a_process_gone_without_any_verdict_line_is_no_verdict_reason_gone() {
        let out = tmp("kill9");
        fs::write(&out, "gate: merging...\n").unwrap();
        fs::write(pid_path(&out), "555\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(555, 1));
        assert_eq!(o.line, "gate: VERDICT=NO_VERDICT reason=gone");
        assert_eq!(o.rc, 75);
    }

    #[test]
    fn no_pid_file_ever_appearing_is_a_named_fault_not_a_hang() {
        let out = tmp("nopid");
        fs::write(&out, "nothing ran\n").unwrap();
        let o = wait_for(&out, &FakeClock::new(), &FakeProc::new(1, 0));
        assert!(o.line.contains("reason=no-pid"), "{}", o.line);
        assert_eq!(o.rc, 75);
    }
}
