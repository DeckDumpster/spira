//! The wall-clock cap every testenv run carries, and the scan that backstops it.
//!
//! A run with `--deadline` is a gate tier and carries the larger of the gate cap and its own
//! deadline; any other run carries the suite cap. At the cap the run cancels itself (killing its containers through the same path a
//! signal takes) and finishes `FAULT reason=over-cap`, naming what was running. `testenv-cap`
//! scans live runs and kills any that outlive their cap plus [`GRACE`], plus the load-wait
//! bound for a suite-tier run, whose cap starts only once load admission lets it in, plus the
//! slot-wait bound. The run's own clock stands still for the whole of every admission-slot
//! wait ([`hold`]); the scan, which cannot see the waits, bounds the run's total age instead.

use std::path::{Path, PathBuf};
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
static CONTAINERS: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    pub static OBSERVE_CAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// The clock of the cap armed on this thread, for [`hold`] to stop.
    static CURRENT: std::cell::RefCell<Option<Arc<Mutex<Clock>>>> = const { std::cell::RefCell::new(None) };
}

/// Whether a cap armed on this thread is counting time now.
#[cfg(test)]
pub fn armed_here() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|k| k.lock().unwrap().counting()))
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

/// The wall-clock cap a run carries, and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    pub secs: u64,
    pub source: &'static str,
}

pub fn limit(suite_cap: u64, gate_cap: u64, deadline: Option<u64>) -> Cap {
    match deadline {
        Some(d) if d > gate_cap => Cap { secs: d + SETTLE, source: "--deadline plus settle" },
        Some(_) => Cap { secs: gate_cap + SETTLE, source: "gate cap plus settle" },
        None => Cap { secs: suite_cap, source: "suite cap" },
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

/// Marks a container as this run's until the guard drops: the watchdog removes it before it
/// cuts the run off, because killing our podman clients leaves conmon and the container running.
pub struct Holding(String);

pub fn holding(container: &str) -> Holding {
    CONTAINERS.lock().unwrap().push(container.to_string());
    Holding(container.to_string())
}

impl Drop for Holding {
    fn drop(&mut self) {
        let mut c = CONTAINERS.lock().unwrap();
        if let Some(i) = c.iter().position(|n| *n == self.0) {
            c.remove(i);
        }
    }
}

fn remove_held(remove: &dyn Fn(&str)) {
    let held: Vec<String> = CONTAINERS.lock().unwrap().clone();
    for name in held {
        remove(&name);
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

/// Wall time since arming, less every slot wait: the cap meters work, not queueing.
struct Clock {
    start: Instant,
    held_since: Option<Instant>,
    excused: Duration,
    ledger: Option<PathBuf>,
}

fn epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where a run publishes its slot waits, for the scan, which cannot see them from outside.
pub fn ledger_path(run: &Path, pid: u32) -> PathBuf {
    run.join("testenv-slot-wait").join(pid.to_string())
}

/// `<start ticks> <seconds excused> <epoch a wait began, or 0>`; a failed write only means the
/// scan falls back to its flat bound.
fn publish(k: &Clock) {
    let Some(path) = &k.ledger else { return };
    let ticks = start_ticks(std::process::id()).unwrap_or(0);
    let since = k.held_since.map_or(0, |t| epoch().saturating_sub(t.elapsed().as_secs()));
    let _ = path.parent().map(std::fs::create_dir_all);
    let _ = std::fs::write(path, format!("{ticks} {} {since}", k.excused.as_secs()));
}

/// Seconds of `p`'s age that were slot waiting, as it published them; 0 when unpublished or
/// when the ledger belongs to an earlier process that had the same pid.
pub fn excused_secs(run: &Path, p: &Proc) -> u64 {
    let Ok(text) = std::fs::read_to_string(ledger_path(run, p.pid)) else { return 0 };
    let f: Vec<u64> = text.split_whitespace().filter_map(|v| v.parse().ok()).collect();
    match f.as_slice() {
        [ticks, done, since] if *ticks == p.start_ticks => done + if *since > 0 { epoch().saturating_sub(*since) } else { 0 },
        _ => 0,
    }
}

impl Clock {
    fn excused(&self) -> Duration {
        self.excused + self.held_since.map_or(Duration::ZERO, |t| t.elapsed())
    }

    fn counted(&self) -> Duration {
        self.start.elapsed().saturating_sub(self.excused())
    }

    #[cfg(test)]
    fn counting(&self) -> bool {
        self.held_since.is_none()
    }
}

/// While this lives the cap armed on this thread does not count.
pub struct Held(Option<Arc<Mutex<Clock>>>);

/// Stops the clock of the cap armed on this thread while the run waits for an admission slot.
pub fn hold() -> Held {
    let clock = CURRENT.with(|c| c.borrow().clone()).filter(|k| {
        let mut k = k.lock().unwrap();
        k.held_since.is_none() && {
            k.held_since = Some(Instant::now());
            publish(&k);
            true
        }
    });
    Held(clock)
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(k) = &self.0 {
            let mut k = k.lock().unwrap();
            if let Some(t) = k.held_since.take() {
                k.excused += t.elapsed();
                publish(&k);
            }
        }
    }
}

pub struct Armed(Arc<AtomicBool>, Trip, Arc<Mutex<Clock>>);

impl Armed {
    /// Time spent held for admission slots so far.
    pub fn waited(&self) -> Duration {
        self.2.lock().unwrap().excused()
    }

    /// Stops the watchdog; what was running when the cap fired, if it did.
    pub fn finish(self) -> Option<(String, u64)> {
        self.0.store(true, Ordering::SeqCst);
        CURRENT.with(|c| c.borrow_mut().take());
        if let Some(path) = &self.2.lock().unwrap().ledger {
            let _ = std::fs::remove_file(path);
        }
        let trip = self.1.lock().unwrap().take();
        if trip.is_some() {
            TRIPPED.store(false, Ordering::SeqCst);
        }
        trip
    }
}

/// Arms the cap on this thread; [`hold`] stops its clock.
pub fn arm(cap: Cap, ledger: Option<PathBuf>) -> Armed {
    let limit = Duration::from_secs(cap.secs);
    let source = cap.source;
    let done = Arc::new(AtomicBool::new(false));
    let seen = done.clone();
    let trip: Trip = Arc::new(Mutex::new(None));
    let slot = trip.clone();
    let clock = Arc::new(Mutex::new(Clock {
        start: Instant::now(),
        held_since: None,
        excused: Duration::ZERO,
        ledger,
    }));
    let shared = clock.clone();
    CURRENT.with(|c| *c.borrow_mut() = Some(clock.clone()));
    std::thread::spawn(move || {
        let wait = |more: &dyn Fn() -> bool| {
            while more() {
                if seen.load(Ordering::SeqCst) {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            !seen.load(Ordering::SeqCst)
        };
        if !wait(&|| clock.lock().unwrap().counted() < limit) {
            return;
        }
        let what = describe();
        *slot.lock().unwrap() = Some((what.clone(), limit.as_secs()));
        TRIPPED.store(true, Ordering::SeqCst);
        // The unwind is timed on the wall clock: a hold taken after the trip must not delay it.
        let tripped = Instant::now();
        if wait(&|| tripped.elapsed() < UNWIND) {
            kill_descendants(std::process::id());
            remove_held(&|n| {
                use crate::runtime::ContainerRuntime;
                crate::runtime::Podman { exe: Default::default(), harness: Default::default() }.purge(n)
            });
            crate::telemetry::push_over_cap(&crate::telemetry::sink(), "self");
            println!("{}", over_cap_line(&what, limit.as_secs(), source));
            println!("VERDICT FAULT rc=2 ran=0 reason=over-cap");
            // SAFETY: a watchdog thread has no caller to return to; _exit skips atexit handlers, stdout is line-buffered and already flushed.
            unsafe { libc::_exit(2) };
        }
    });
    Armed(done, trip, shared)
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

/// A suite-tier run may first wait up to `load_wait` on host load before its cap starts; a
/// gate-tier run never waits on load. Any run's cap stands still for up to `slot_wait` while
/// it waits for admission slots.
pub fn breaches(
    procs: &[Proc],
    suite_cap: u64,
    gate_cap: u64,
    load_wait: u64,
    slot_wait: u64,
    grace: u64,
    exe: Option<&str>,
) -> Vec<Breach> {
    procs
        .iter()
        .filter(|p| is_run(&p.argv) && exe.is_none_or(|e| p.argv[0] == e))
        .filter_map(|p| {
            let gate = has_deadline(&p.argv);
            let cap = limit(suite_cap, gate_cap, gate.then(|| deadline_of(&p.argv).unwrap_or(0))).secs;
            let waited = if gate { 0 } else { load_wait } + slot_wait;
            (p.age_secs > cap + waited + grace).then(|| Breach { proc: p.clone(), tier: if gate { "gate" } else { "suite" }, cap })
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
    fn the_cut_off_removes_every_container_the_run_still_holds() {
        let removed = Mutex::new(Vec::new());
        let mine = |v: &Mutex<Vec<String>>| v.lock().unwrap().iter().filter(|n| n.starts_with("cap-held-")).cloned().collect::<Vec<_>>();
        let a = holding("cap-held-a");
        {
            let _b = holding("cap-held-b");
            remove_held(&|n| removed.lock().unwrap().push(n.to_string()));
            assert_eq!(mine(&removed), ["cap-held-a", "cap-held-b"]);
        }
        removed.lock().unwrap().clear();
        drop(a);
        remove_held(&|n| removed.lock().unwrap().push(n.to_string()));
        assert!(mine(&removed).is_empty(), "a released container is not the cut-off's to remove");
    }

    #[test]
    fn a_deadline_run_carries_the_gate_cap_and_any_other_the_suite_cap() {
        assert_eq!(limit(900, 300, None), Cap { secs: 900, source: "suite cap" });
        assert_eq!(limit(900, 300, Some(120)), Cap { secs: 300 + SETTLE, source: "gate cap plus settle" });
    }

    #[test]
    fn an_explicit_deadline_above_the_gate_cap_governs() {
        assert_eq!(limit(900, 300, Some(600)), Cap { secs: 600 + SETTLE, source: "--deadline plus settle" });
        assert_eq!(deadline_of(&["testenv".into(), "--deadline=600".into()]), Some(600));
        assert_eq!(deadline_of(&["testenv".into(), "--deadline".into(), "600".into(), "b".into()]), Some(600));
        let long = p(1, 640, &["testenv", "--deadline", "600", "b"]);
        assert!(breaches(std::slice::from_ref(&long), 900, 300, 0, 0, GRACE, None).is_empty());
        let over = p(2, 600 + SETTLE + GRACE + 1, &["testenv", "--deadline", "600", "b"]);
        assert_eq!(breaches(&[over], 900, 300, 0, 0, GRACE, None)[0].cap, 630);
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
        let got: Vec<(u32, &str)> = breaches(&procs, 900, 300, 0, 0, GRACE, None).iter().map(|b| (b.proc.pid, b.tier)).collect();
        assert_eq!(got, vec![(2, "suite"), (4, "gate")]);
    }

    #[test]
    fn a_suite_run_is_allowed_its_load_wait_on_top_of_cap_and_grace() {
        let waited = p(1, 600 + 240 + GRACE, &["testenv", "b"]);
        assert!(breaches(&[waited], 600, 300, 240, 0, GRACE, None).is_empty());
        let over = p(2, 600 + 240 + GRACE + 1, &["testenv", "b"]);
        assert_eq!(breaches(&[over], 600, 300, 240, 0, GRACE, None)[0].cap, 600);
        let gate = [p(3, 300 + SETTLE + GRACE + 1, &["testenv", "--deadline", "120", "b"])];
        assert_eq!(breaches(&gate, 600, 300, 240, 0, GRACE, None), breaches(&gate, 600, 300, 0, 0, GRACE, None));
        assert_eq!(breaches(&gate, 600, 300, 240, 0, GRACE, None).len(), 1);
    }

    #[test]
    fn any_run_is_allowed_its_slot_wait_on_top_of_cap_load_wait_and_grace() {
        let waited = p(1, 600 + 240 + 300 + GRACE, &["testenv", "b"]);
        assert!(breaches(&[waited], 600, 300, 240, 300, GRACE, None).is_empty());
        let over = p(2, 600 + 240 + 300 + GRACE + 1, &["testenv", "b"]);
        assert_eq!(breaches(&[over], 600, 300, 240, 300, GRACE, None)[0].cap, 600);
        let gate = p(3, 300 + SETTLE + 300 + GRACE, &["testenv", "--deadline", "120", "b"]);
        assert!(breaches(std::slice::from_ref(&gate), 600, 300, 240, 300, GRACE, None).is_empty());
        let gate_over = p(4, 300 + SETTLE + 300 + GRACE + 1, &["testenv", "--deadline", "120", "b"]);
        assert_eq!(breaches(&[gate_over], 600, 300, 240, 300, GRACE, None).len(), 1);
    }

    #[test]
    fn a_hold_stops_the_clock_until_it_is_released() {
        let armed = arm(Cap { secs: 9_999, source: "test" }, None);
        assert!(armed_here());
        let h = hold();
        assert!(!armed_here());
        drop(h);
        assert!(armed_here());
        armed.finish();
        assert!(!armed_here());
    }

    #[test]
    fn a_run_admitted_after_a_slot_wait_longer_than_the_cap_gets_its_whole_cap() {
        let ago = |s| Instant::now() - Duration::from_secs(s);
        let admitted = Clock { start: ago(1_212 + 5), held_since: None, excused: Duration::from_secs(1_212), ledger: None };
        assert!(admitted.counted() < Duration::from_secs(10), "the wait is not the run's time");
        let queued = Clock { start: ago(1_212), held_since: Some(ago(1_212)), excused: Duration::ZERO, ledger: None };
        assert!(queued.counted() < Duration::from_secs(10));
    }

    #[test]
    fn the_scan_does_not_count_the_slot_waits_a_run_published() {
        let dir = testkit::TempDir::new("testenv-cap-ledger");
        let run = dir.path().to_path_buf();
        let me = std::process::id();
        let mine = live().into_iter().find(|q| q.pid == me).unwrap();
        assert_eq!(excused_secs(&run, &mine), 0, "nothing published");
        let armed = arm(Cap { secs: 9_999, source: "test" }, Some(ledger_path(&run, me)));
        drop(hold());
        assert!(ledger_path(&run, me).exists());
        std::fs::write(ledger_path(&run, me), format!("{} 1212 0", mine.start_ticks)).unwrap();
        assert_eq!(excused_secs(&run, &mine), 1212);
        std::fs::write(ledger_path(&run, me), format!("{} 1212 0", mine.start_ticks + 1)).unwrap();
        assert_eq!(excused_secs(&run, &mine), 0, "another process's ledger under a reused pid");
        armed.finish();
        assert!(!ledger_path(&run, me).exists(), "a finished run leaves none");
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
