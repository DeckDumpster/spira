//! The wall-clock cap every testenv run carries, and the scan that backstops it.
//!
//! A run with `--deadline` is a gate tier and carries the larger of the gate cap and its own
//! deadline; any other run carries the suite cap. At the cap the run cancels itself (killing its containers through the same path a
//! signal takes) and finishes `FAULT reason=over-cap`, naming what was running. `testenv-cap`
//! scans live runs and kills any that outlive their cap plus [`GRACE`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A gate run's own `--deadline` cuts its suites first; teardown gets this long past the cap.
pub const SETTLE: u64 = 30;
/// How long a run past its cap may take to exit before the scan kills it.
pub const GRACE: u64 = 60;
/// After tripping, how long the run gets to unwind before it is cut off from inside.
const UNWIND: Duration = Duration::from_secs(30);

pub const SUBCOMMANDS: &[&str] = &[
    "suites", "lint", "preflight", "status", "testdb", "wait", "container", "plan", "warm",
];

static TRIPPED: AtomicBool = AtomicBool::new(false);
static PHASE: Mutex<String> = Mutex::new(String::new());
static RUNNING: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    pub static OBSERVE_CAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Ignores the per-thread opt-in: for a fake that stands in for a process outside the run.
#[cfg(test)]
pub fn tripped_anywhere() -> bool {
    TRIPPED.load(Ordering::SeqCst)
}

#[cfg(not(test))]
pub fn tripped() -> bool {
    TRIPPED.load(Ordering::SeqCst)
}

#[cfg(test)]
pub fn tripped() -> bool {
    OBSERVE_CAP.with(|o| o.get()) && TRIPPED.load(Ordering::SeqCst)
}

pub fn limit(suite_cap: u64, gate_cap: u64, deadline: Option<u64>) -> u64 {
    match deadline {
        Some(d) => gate_cap.max(d) + SETTLE,
        None => suite_cap,
    }
}

pub fn source(gate_cap: u64, deadline: Option<u64>) -> &'static str {
    match deadline {
        Some(d) if d > gate_cap => "--deadline plus settle",
        Some(_) => "gate cap plus settle",
        None => "suite cap",
    }
}

pub fn deadline_of(argv: &[String]) -> Option<u64> {
    let mut it = argv.iter().skip(1).take_while(|a| *a != "--");
    while let Some(a) = it.next() {
        let v = if a == "--deadline" { it.next().map(String::as_str) } else { a.strip_prefix("--deadline=") };
        if let Some(v) = v {
            return v.parse().ok();
        }
    }
    None
}

pub fn phase(name: &str) {
    *PHASE.lock().unwrap() = name.to_string();
}

/// Marks a tier or suite as running until the guard drops.
pub struct Running(String);

pub fn running(unit: &str) -> Running {
    RUNNING.lock().unwrap().push(unit.to_string());
    Running(unit.to_string())
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut r = RUNNING.lock().unwrap();
        if let Some(i) = r.iter().position(|u| *u == self.0) {
            r.remove(i);
        }
    }
}

fn describe() -> String {
    let units: Vec<String> = RUNNING.lock().unwrap().clone();
    if !units.is_empty() {
        return format!("tier/suite {}", units.join(","));
    }
    match PHASE.lock().unwrap().as_str() {
        "" => "setup".to_string(),
        p => format!("phase after {p}"),
    }
}

pub fn over_cap_line(what: &str, secs: u64, source: &str) -> String {
    format!("over-cap: {what} was still running at the {secs}s cap ({source})")
}

type Trip = Arc<Mutex<Option<(String, u64)>>>;

pub struct Armed(Arc<AtomicBool>, Trip);

impl Armed {
    /// Stops the watchdog; what was running when the cap fired, if it did.
    pub fn finish(self) -> Option<(String, u64)> {
        self.0.store(true, Ordering::SeqCst);
        let trip = self.1.lock().unwrap().take();
        if trip.is_some() {
            TRIPPED.store(false, Ordering::SeqCst);
        }
        trip
    }
}

pub fn arm(limit: Duration, source: &'static str) -> Armed {
    let done = Arc::new(AtomicBool::new(false));
    let seen = done.clone();
    let trip: Trip = Arc::new(Mutex::new(None));
    let slot = trip.clone();
    let start = Instant::now();
    std::thread::spawn(move || {
        let wait = |until: Duration| {
            while start.elapsed() < until {
                if seen.load(Ordering::SeqCst) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            !seen.load(Ordering::SeqCst)
        };
        if !wait(limit) {
            return;
        }
        let what = describe();
        *slot.lock().unwrap() = Some((what.clone(), limit.as_secs()));
        TRIPPED.store(true, Ordering::SeqCst);
        if wait(limit + UNWIND) {
            kill_descendants(std::process::id());
            crate::telemetry::push_over_cap(&crate::telemetry::sink(), "self");
            println!("{}", over_cap_line(&what, limit.as_secs(), source));
            println!("VERDICT FAULT rc=2 ran=0 reason=over-cap");
            // SAFETY: a watchdog thread has no caller to return to; _exit skips atexit handlers, stdout is line-buffered and already flushed.
            unsafe { libc::_exit(2) };
        }
    });
    Armed(done, trip)
}

fn children(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for e in rd.flatten() {
        let Some(p) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{p}/stat")) else { continue };
        let ppid = stat.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().nth(1)).and_then(|v| v.parse::<u32>().ok());
        if ppid == Some(pid) {
            out.push(p);
        }
    }
    out
}

fn kill_descendants(pid: u32) {
    for c in children(pid) {
        kill_descendants(c);
        // SAFETY: SIGKILL to a descendant pid we just listed.
        unsafe { libc::kill(c as i32, libc::SIGKILL) };
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub start_ticks: u64,
    pub argv: Vec<String>,
    pub age_secs: u64,
}

/// A live `testenv` invocation that is a run (not one of its subcommands).
pub fn is_run(argv: &[String]) -> bool {
    let Some(exe) = argv.first() else { return false };
    if exe.rsplit('/').next() != Some("testenv") {
        return false;
    }
    !argv.get(1).is_some_and(|a| SUBCOMMANDS.contains(&a.as_str()))
}

pub fn has_deadline(argv: &[String]) -> bool {
    argv.iter().skip(1).take_while(|a| *a != "--").any(|a| a == "--deadline" || a.starts_with("--deadline="))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breach {
    pub proc: Proc,
    pub tier: &'static str,
    pub cap: u64,
}

pub fn breaches(procs: &[Proc], suite_cap: u64, gate_cap: u64, grace: u64, exe: Option<&str>) -> Vec<Breach> {
    procs
        .iter()
        .filter(|p| is_run(&p.argv) && exe.is_none_or(|e| p.argv[0] == e))
        .filter_map(|p| {
            let gate = has_deadline(&p.argv);
            let cap = limit(suite_cap, gate_cap, gate.then(|| deadline_of(&p.argv).unwrap_or(0)));
            (p.age_secs > cap + grace).then(|| Breach { proc: p.clone(), tier: if gate { "gate" } else { "suite" }, cap })
        })
        .collect()
}

fn start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

fn argv_of(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(raw.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
}

pub fn live() -> Vec<Proc> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let up = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .unwrap_or(0.0);
    let Ok(rd) = std::fs::read_dir("/proc") else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()))
        .filter_map(|pid| {
            let argv = argv_of(pid)?;
            let t = start_ticks(pid)?;
            let age = (up - t as f64 / hz as f64).max(0.0) as u64;
            Some(Proc { pid, start_ticks: t, argv, age_secs: age })
        })
        .collect()
}

/// Kills `p` only if the pid still carries the exact cmdline and start time that was judged.
pub fn kill_verified(p: &Proc) -> bool {
    if argv_of(p.pid).as_ref() != Some(&p.argv) || start_ticks(p.pid) != Some(p.start_ticks) {
        return false;
    }
    kill_descendants(p.pid);
    // SAFETY: SIGKILL to a pid whose identity was just re-verified.
    unsafe { libc::kill(p.pid as i32, libc::SIGKILL) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, age: u64, argv: &[&str]) -> Proc {
        Proc { pid, start_ticks: 1, argv: argv.iter().map(|s| s.to_string()).collect(), age_secs: age }
    }

    #[test]
    fn a_deadline_run_carries_the_gate_cap_and_any_other_the_suite_cap() {
        assert_eq!(limit(900, 300, None), 900);
        assert_eq!(limit(900, 300, Some(120)), 300 + SETTLE);
    }

    #[test]
    fn an_explicit_deadline_above_the_gate_cap_governs() {
        assert_eq!(limit(900, 300, Some(600)), 600 + SETTLE);
        assert_eq!(source(300, Some(600)), "--deadline plus settle");
        assert_eq!(source(300, Some(120)), "gate cap plus settle");
        assert_eq!(deadline_of(&["testenv".into(), "--deadline=600".into()]), Some(600));
        assert_eq!(deadline_of(&["testenv".into(), "--deadline".into(), "600".into(), "b".into()]), Some(600));
        let long = p(1, 640, &["testenv", "--deadline", "600", "b"]);
        assert!(breaches(&[long.clone()], 900, 300, GRACE, None).is_empty());
        let over = p(2, 600 + SETTLE + GRACE + 1, &["testenv", "--deadline", "600", "b"]);
        assert_eq!(breaches(&[over], 900, 300, GRACE, None)[0].cap, 630);
    }

    #[test]
    fn only_runs_past_cap_plus_grace_are_breaches() {
        let procs = [
            p(1, 900 + GRACE, &["/bin/testenv", "b", "r"]),
            p(2, 900 + GRACE + 1, &["/bin/testenv", "b", "r"]),
            p(3, 350, &["testenv", "--deadline", "300", "--suites", "x", "b"]),
            p(4, 300 + SETTLE + GRACE + 1, &["testenv", "--deadline=300", "b"]),
            p(5, 99_999, &["testenv", "wait", "out"]),
            p(6, 99_999, &["sleep", "9999"]),
            p(7, 99_999, &["/bin/testenv-cap", "watch"]),
        ];
        let got: Vec<(u32, &str)> = breaches(&procs, 900, 300, GRACE, None).iter().map(|b| (b.proc.pid, b.tier)).collect();
        assert_eq!(got, vec![(2, "suite"), (4, "gate")]);
    }

    #[test]
    fn a_dash_dash_branch_named_like_a_subcommand_is_still_a_run() {
        assert!(is_run(&["testenv".into(), "--".into(), "suites".into()]));
        assert!(!is_run(&["testenv".into(), "suites".into()]));
    }

    #[test]
    fn a_pid_whose_cmdline_changed_is_not_killed() {
        let me = std::process::id();
        let mut proc = live().into_iter().find(|q| q.pid == me).unwrap();
        proc.argv.push("not-what-was-judged".into());
        assert!(!kill_verified(&proc));
    }
}
