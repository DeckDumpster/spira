//! The wall-clock cap every testenv run carries, and the scan that backstops it.
//!
//! A run with `--deadline` is a gate tier and carries the larger of the gate cap and its own
//! deadline; any other run carries the suite cap. At the cap the run cancels itself (killing its containers through the same path a
//! signal takes) and finishes `FAULT reason=over-cap`, naming what was running. `testenv-cap`
//! scans live runs and kills any that outlive their cap plus [`GRACE`], plus the load-wait
//! bound for a suite-tier run, whose cap starts only once load admission lets it in. The run's
//! own clock stands still for the whole of every admission-slot or warm-cache wait ([`hold`]) and it
//! publishes the waits in a ledger the scan subtracts ([`uncounted`]); a run with no ledger
//! gets the flat slot-wait bound instead.

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
/// How many cut units the report quotes; every parallel suite may be running at the cap.
const TAIL_UNITS: usize = 4;

pub const SUBCOMMANDS: &[&str] = &[
    "lint", "preflight", "status", "testdb", "wait", "container", "plan", "warm", "test-bins", "sandbox-supervise",
];

static TRIPPED: AtomicBool = AtomicBool::new(false);
static PHASE: Mutex<String> = Mutex::new(String::new());
static RUNNING: Mutex<Vec<Unit>> = Mutex::new(Vec::new());
static CONTAINERS: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    pub static ONLY_MY_UNITS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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

#[cfg(test)]
thread_local! {
    /// A trip only this thread sees: a real one is process-wide and any `finish` clears it.
    pub static TRIPPED_HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
    TRIPPED_HERE.with(|t| t.get()) || (OBSERVE_CAP.with(|o| o.get()) && TRIPPED.load(Ordering::SeqCst))
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

#[derive(Clone)]
struct Unit {
    id: u64,
    name: String,
    step: Option<String>,
    log: Option<PathBuf>,
    #[cfg(test)]
    owner: std::thread::ThreadId,
}

fn units() -> Vec<Unit> {
    let all = RUNNING.lock().unwrap().clone();
    // Tests run tiers in parallel in one process; an exact-text assertion must see only its own.
    #[cfg(test)]
    if ONLY_MY_UNITS.with(|o| o.get()) {
        return all.into_iter().filter(|u| u.owner == std::thread::current().id()).collect();
    }
    all
}

/// Marks a tier or suite as running until the guard drops.
pub struct Running(u64);

pub fn running(unit: &str) -> Running {
    static IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = IDS.fetch_add(1, Ordering::Relaxed);
    RUNNING.lock().unwrap().push(Unit {
        id,
        name: unit.to_string(),
        step: None,
        log: None,
        #[cfg(test)]
        owner: std::thread::current().id(),
    });
    Running(id)
}

impl Running {
    pub fn step(&self, step: &str) {
        self.update(|u| u.step = Some(step.to_string()));
    }

    pub fn log(&self, path: &Path) {
        self.update(|u| u.log = Some(path.to_path_buf()));
    }

    fn update(&self, f: impl FnOnce(&mut Unit)) {
        if let Some(u) = RUNNING.lock().unwrap().iter_mut().find(|u| u.id == self.0) {
            f(u);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.lock().unwrap().retain(|u| u.id != self.0);
    }
}

pub(crate) fn log_line(unit: &str, path: &Path) -> String {
    format!("  log ({unit}): {}", path.display())
}

/// A cut run's whole explanation: nothing the run buffers or prints later survives `_exit`.
pub fn trip_report(what: &str, secs: u64, source: &str) -> Vec<String> {
    with_verdict(explain(&units(), what, secs, source))
}

fn with_verdict(mut lines: Vec<String>) -> Vec<String> {
    lines.push("VERDICT FAULT rc=2 ran=0 reason=over-cap".to_string());
    lines
}

fn explain(cut: &[Unit], what: &str, secs: u64, source: &str) -> Vec<String> {
    let mut out = vec![over_cap_line(what, secs, source)];
    let mut held_back = Vec::new();
    for (i, (u, path)) in cut.iter().filter_map(|u| Some((u, u.log.as_ref()?))).enumerate() {
        out.push(log_line(&u.name, path));
        if i >= TAIL_UNITS {
            held_back.push(u.name.as_str());
            continue;
        }
        match std::fs::read(path) {
            Ok(bytes) => {
                out.push(format!("  last 60 lines ({}):", u.name));
                let tail = crate::foreign::tail(&String::from_utf8_lossy(&bytes), 60);
                out.extend(tail.lines().map(|l| format!("    {l}")));
            }
            Err(e) => out.push(format!("  log ({}) unreadable: {e}", u.name)),
        }
    }
    if !held_back.is_empty() {
        out.push(format!(
            "  last lines held back for {} more units (cap {TAIL_UNITS}): {}",
            held_back.len(),
            held_back.join(",")
        ));
    }
    out
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

#[cfg(test)]
pub(crate) fn describe() -> String {
    describe_units(&units())
}

fn describe_units(cut: &[Unit]) -> String {
    let units: Vec<String> = cut
        .iter()
        .map(|u| match &u.step {
            Some(step) => format!("{} (in {step})", u.name),
            None => u.name.clone(),
        })
        .collect();
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

/// Read as the cap fired: an unwound run has dropped its guards by the time it reports.
pub struct Tripped {
    pub report: Vec<String>,
}

type Trip = Arc<Mutex<Option<Tripped>>>;

/// Wall time since arming, less every slot wait: the cap meters work, not queueing.
struct Clock {
    start: Instant,
    limit: Duration,
    held_since: Option<Instant>,
    /// Live [`Held`] guards: concurrent tiers wait at once and finish waiting in any order.
    holds: u32,
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

/// `<start ticks> <seconds excused> <epoch a wait began>` as parsed from a ledger.
fn ledger_fields(text: &str) -> Option<(u64, u64, u64)> {
    let f: Vec<u64> = text.split_whitespace().filter_map(|v| v.parse().ok()).collect();
    match f.as_slice() {
        [ticks, done, since] => Some((*ticks, *done, *since)),
        _ => None,
    }
}

/// Excused seconds from `p`'s own ledger; `None` when it has none.
fn ledgered_secs(run: &Path, p: &Proc) -> Option<u64> {
    let text = std::fs::read_to_string(ledger_path(run, p.pid)).ok()?;
    let (ticks, done, since) = ledger_fields(&text)?;
    (ticks == p.start_ticks).then(|| done + if since > 0 { epoch().saturating_sub(since) } else { 0 })
}

/// Each run's age less its published slot waits. A run with no ledger keeps its age and is
/// marked unledgered, so [`breaches`] gives it the flat slot-wait bound instead.
pub fn uncounted(run: &Path, procs: Vec<Proc>) -> Vec<Proc> {
    procs
        .into_iter()
        .map(|p| match ledgered_secs(run, &p) {
            Some(x) => Proc { age_secs: p.age_secs.saturating_sub(x), ledgered: true, ..p },
            None => p,
        })
        .collect()
}

/// Removes ledgers whose pid is gone or now belongs to another process; one that does not parse
/// may be mid-write by a run arming now, so it stays.
fn sweep_ledgers(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let dead = e
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
            .zip(std::fs::read_to_string(e.path()).ok().and_then(|t| ledger_fields(&t)))
            .is_some_and(|(pid, (ticks, ..))| start_ticks(pid) != Some(ticks));
        if dead {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

impl Clock {
    fn excused(&self) -> Duration {
        self.excused + self.held_since.map_or(Duration::ZERO, |t| t.elapsed())
    }

    fn left(&self) -> Duration {
        self.limit.saturating_sub(self.counted())
    }

    fn counted(&self) -> Duration {
        self.start.elapsed().saturating_sub(self.excused())
    }

    #[cfg(test)]
    fn counting(&self) -> bool {
        self.held_since.is_none()
    }
}

/// Seconds left of the cap armed on this thread; `None` when nothing is armed.
pub fn remaining() -> Option<u64> {
    let k = CURRENT.with(|c| c.borrow().clone())?;
    let k = k.lock().unwrap();
    Some(k.left().as_secs())
}

/// The cap armed on a thread, for a thread it spawns to [`hold`].
#[derive(Clone)]
pub struct Handle(Option<Arc<Mutex<Clock>>>);

pub fn handle() -> Handle {
    Handle(CURRENT.with(|c| c.borrow().clone()))
}

impl Handle {
    pub fn adopt(self) {
        CURRENT.with(|c| *c.borrow_mut() = self.0);
    }
}

/// While this lives the cap armed on this thread does not count.
pub struct Held(Option<Arc<Mutex<Clock>>>);

/// Stops the clock of the cap armed on this thread while the run waits for an admission slot
/// or a warm cache.
pub fn hold() -> Held {
    let clock = CURRENT.with(|c| c.borrow().clone());
    if let Some(k) = &clock {
        let mut k = k.lock().unwrap();
        k.holds += 1;
        if k.holds == 1 {
            k.held_since = Some(Instant::now());
            publish(&k);
        }
    }
    Held(clock)
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(k) = &self.0 {
            let mut k = k.lock().unwrap();
            k.holds -= 1;
            if let Some(t) = (k.holds == 0).then(|| k.held_since.take()).flatten() {
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
    pub fn finish(self) -> Option<Tripped> {
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
        limit,
        held_since: None,
        holds: 0,
        excused: Duration::ZERO,
        ledger,
    }));
    let shared = clock.clone();
    if let Some(dir) = clock.lock().unwrap().ledger.as_ref().and_then(|l| l.parent()) {
        sweep_ledgers(dir);
    }
    publish(&clock.lock().unwrap());
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
        let cut = units();
        let what = describe_units(&cut);
        *slot.lock().unwrap() = Some(Tripped { report: explain(&cut, &what, limit.as_secs(), source) });
        TRIPPED.store(true, Ordering::SeqCst);
        // The unwind is timed on the wall clock: a hold taken after the trip must not delay it.
        let tripped = Instant::now();
        if wait(&|| tripped.elapsed() < UNWIND) {
            // The units as cut, with their logs as they are now: a tier may since have dropped its guard.
            let report = with_verdict(explain(&cut, &what, limit.as_secs(), source));
            kill_descendants(std::process::id());
            remove_held(&|n| {
                use crate::runtime::ContainerRuntime;
                crate::runtime::Podman { exe: Default::default(), harness: Default::default() }.purge(n)
            });
            crate::telemetry::push_over_cap(&crate::telemetry::sink(), "self");
            for line in report {
                println!("{line}");
            }
            // SAFETY: a watchdog thread has no caller to return to; _exit skips atexit handlers, stdout is line-buffered and already flushed.
            unsafe { libc::_exit(2) };
        }
    });
    Armed(done, trip, shared)
}

fn children(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(system_paths::PROC) else { return out };
    for e in rd.flatten() {
        let Some(p) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("{}/{p}/stat", system_paths::PROC)) else { continue };
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
    /// `age_secs` already excludes the run's published slot waits.
    pub ledgered: bool,
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

/// Says a kill once, on stdout and in the Concierge inbox.
pub fn report(sink: &inbox_rules::Sink, b: &Breach) {
    let line = format!(
        "testenv over-cap killed: pid {} tier={} wall={}s cap={}s cmd={}",
        b.proc.pid,
        b.tier,
        b.proc.age_secs,
        b.cap,
        b.proc.argv.join(" ")
    );
    println!("{line}");
    let stamp = crate::util::iso_utc(crate::batch::now_epoch());
    if let Err(e) = sink.record(&format!("{stamp} {line}")) {
        eprintln!("testenv-cap: {e}");
    }
}

/// A suite-tier run may first wait up to `load_wait` on host load before its cap starts; a
/// gate-tier run never waits on load. A run with no ledger may have waited up to `slot_wait`
/// for admission slots; a ledgered run's age already excludes its waits.
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
            let waited = if gate { 0 } else { load_wait } + if p.ledgered { 0 } else { slot_wait };
            (p.age_secs > cap + waited + grace).then(|| Breach { proc: p.clone(), tier: if gate { "gate" } else { "suite" }, cap })
        })
        .collect()
}

fn start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("{}/{pid}/stat", system_paths::PROC)).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

pub(crate) fn argv_of(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("{}/{pid}/cmdline", system_paths::PROC)).ok()?;
    Some(raw.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
}

pub fn live() -> Vec<Proc> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let up = std::fs::read_to_string(format!("{}/uptime", system_paths::PROC))
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .unwrap_or(0.0);
    let Ok(rd) = std::fs::read_dir(system_paths::PROC) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()))
        .filter_map(|pid| {
            let argv = argv_of(pid)?;
            let t = start_ticks(pid)?;
            let age = (up - t as f64 / hz as f64).max(0.0) as u64;
            Some(Proc { pid, start_ticks: t, argv, age_secs: age, ledgered: false })
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
        Proc { pid, start_ticks: 1, argv: argv.iter().map(|s| s.to_string()).collect(), age_secs: age, ledgered: false }
    }

    #[test_id(twzph60)]
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

    #[test_id(tgh822g)]
    #[test]
    fn a_trip_report_names_each_running_units_step_and_log() {
        ONLY_MY_UNITS.with(|o| o.set(true));
        let dir = testkit::TempDir::new("testenv-cap-trip");
        let log = dir.path().join("rust.log");
        let body: String = (1..100).map(|n| format!("line {n}\n")).chain(["==> CI passed in 450s.\n".to_string()]).collect();
        std::fs::write(&log, body).unwrap();
        let unit = running("rust");
        unit.step("warm-cache merge, container rc=0 after 530s");
        unit.log(&log);
        let got = trip_report(&describe(), 600, "suite cap");
        let mut want = vec![
            "over-cap: tier/suite rust (in warm-cache merge, container rc=0 after 530s) was still running at the 600s cap (suite cap)".to_string(),
            format!("  log (rust): {}", log.display()),
            "  last 60 lines (rust):".to_string(),
        ];
        want.extend((41..100).map(|n| format!("    line {n}")));
        want.push("    ==> CI passed in 450s.".into());
        want.push("VERDICT FAULT rc=2 ran=0 reason=over-cap".into());
        assert_eq!(got, want);
    }

    #[test_id(tld4p25)]
    #[test]
    fn a_trip_report_tails_only_the_oldest_units_and_names_the_rest() {
        ONLY_MY_UNITS.with(|o| o.set(true));
        let dir = testkit::TempDir::new("testenv-cap-bound");
        let body: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let names: Vec<String> = (1..=6).map(|n| format!("unit-{n}")).collect();
        let units: Vec<(Running, PathBuf)> = names
            .iter()
            .map(|name| {
                let log = dir.path().join(name);
                std::fs::write(&log, &body).unwrap();
                let unit = running(name);
                unit.log(&log);
                (unit, log)
            })
            .collect();
        let got = trip_report(&describe(), 600, "suite cap");
        assert_eq!(got.iter().filter(|l| l.starts_with("    line ")).count(), 4 * 60);
        for (name, (_, log)) in names.iter().zip(&units) {
            assert!(got.contains(&log_line(name, log)), "{name}");
            assert!(got[0].contains(name.as_str()), "{name}");
        }
        assert!(got.contains(&"  last 60 lines (unit-4):".to_string()));
        assert_eq!(
            got[got.len() - 2],
            "  last lines held back for 2 more units (cap 4): unit-5,unit-6"
        );
    }

    #[test_id(thjs95r)]
    #[test]
    fn a_run_that_unwinds_still_has_the_log_of_a_unit_cut_by_the_cap() {
        let log = Path::new("cap-unwound.log");
        let unit = running("cap-unwound");
        unit.log(log);
        let armed = arm(Cap { secs: 0, source: "test" }, None);
        // clock-safe: a zero cap trips on the watchdog's first look; this waits for that signal
        while !tripped_anywhere() {
            std::thread::yield_now();
        }
        drop(unit);
        let trip = armed.finish().expect("tripped");
        assert!(trip.report.contains(&log_line("cap-unwound", log)), "{:?}", trip.report);
    }

    #[test_id(tyzcyxi)]
    #[test]
    fn a_deadline_run_carries_the_gate_cap_and_any_other_the_suite_cap() {
        assert_eq!(limit(900, 300, None), Cap { secs: 900, source: "suite cap" });
        assert_eq!(limit(900, 300, Some(120)), Cap { secs: 300 + SETTLE, source: "gate cap plus settle" });
    }

    #[test_id(tk7dyn7)]
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

    #[test_id(t8n0dep)]
    #[test]
    fn only_runs_past_cap_plus_grace_are_breaches() {
        let procs = [
            p(1, 900 + GRACE, &["bin/testenv", "b", "r"]),
            p(2, 900 + GRACE + 1, &["bin/testenv", "b", "r"]),
            p(3, 350, &["testenv", "--deadline", "300", "--suites", "x", "b"]),
            p(4, 300 + SETTLE + GRACE + 1, &["testenv", "--deadline=300", "b"]),
            p(5, 99_999, &["testenv", "wait", "out"]),
            p(6, 99_999, &["sleep", "9999"]),
            p(7, 99_999, &["bin/testenv-cap", "watch"]),
        ];
        let got: Vec<(u32, &str)> = breaches(&procs, 900, 300, 0, 0, GRACE, None).iter().map(|b| (b.proc.pid, b.tier)).collect();
        assert_eq!(got, vec![(2, "suite"), (4, "gate")]);
    }

    #[test_id(tc8ijtx)]
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

    #[test_id(tm52xst)]
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

    #[test_id(t5omk6h)]
    #[test]
    fn remaining_is_the_armed_cap_less_counted_time_and_a_hold_does_not_spend_it() {
        assert_eq!(remaining(), None);
        let armed = arm(Cap { secs: 600, source: "test" }, None);
        assert!(matches!(remaining(), Some(s) if (599..=600).contains(&s)), "{:?}", remaining());
        armed.finish();
        assert_eq!(remaining(), None);
        let ago = |s| Instant::now() - Duration::from_secs(s);
        let queued = Clock { start: ago(1_000), limit: Duration::from_secs(1_600), held_since: Some(ago(1_000)), holds: 1, excused: Duration::ZERO, ledger: None };
        assert!((1_599..=1_600).contains(&queued.left().as_secs()), "{:?}", queued.left());
    }

    #[test_id(trxaj4s)]
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

    #[test_id(tgelvar)]
    #[test]
    fn a_run_admitted_after_a_slot_wait_longer_than_the_cap_gets_its_whole_cap() {
        let ago = |s| Instant::now() - Duration::from_secs(s);
        let admitted = Clock { start: ago(1_212 + 5), limit: Duration::ZERO, held_since: None, holds: 0, excused: Duration::from_secs(1_212), ledger: None };
        assert!(admitted.counted() < Duration::from_secs(10), "the wait is not the run's time");
        let queued = Clock { start: ago(1_212), limit: Duration::ZERO, held_since: Some(ago(1_212)), holds: 1, excused: Duration::ZERO, ledger: None };
        assert!(queued.counted() < Duration::from_secs(10));
    }

    #[test_id(tp496tk)]
    #[test]
    fn the_clock_stays_stopped_until_the_last_of_overlapping_holds_ends() {
        let armed = arm(Cap { secs: 9_999, source: "test" }, None);
        let (first, second) = (hold(), hold());
        drop(first);
        assert!(!armed_here(), "the second waiter still waits");
        drop(second);
        assert!(armed_here());
        armed.finish();
    }

    #[test_id(t48dljh)]
    #[test]
    fn the_scan_does_not_count_the_slot_waits_a_run_published() {
        let dir = testkit::TempDir::new("testenv-cap-ledger");
        let run = dir.path().to_path_buf();
        let me = std::process::id();
        let mine = live().into_iter().find(|q| q.pid == me).unwrap();
        assert_eq!(ledgered_secs(&run, &mine), None, "nothing published");
        let armed = arm(Cap { secs: 9_999, source: "test" }, Some(ledger_path(&run, me)));
        drop(hold());
        assert!(ledger_path(&run, me).exists());
        std::fs::write(ledger_path(&run, me), format!("{} 1212 0", mine.start_ticks)).unwrap();
        assert_eq!(ledgered_secs(&run, &mine), Some(1212));
        std::fs::write(ledger_path(&run, me), format!("{} 1212 0", mine.start_ticks + 1)).unwrap();
        assert_eq!(ledgered_secs(&run, &mine), None, "another process's ledger under a reused pid");
        armed.finish();
        assert!(!ledger_path(&run, me).exists(), "a finished run leaves none");
    }

    #[test_id(tyn5w28)]
    #[test]
    fn arm_leaves_a_ledger_before_any_hold() {
        let dir = testkit::TempDir::new("testenv-cap-arm-ledger");
        let run = dir.path().to_path_buf();
        let me = std::process::id();
        let mine = live().into_iter().find(|q| q.pid == me).unwrap();
        assert!(!ledger_path(&run, me).exists());
        let armed = arm(Cap { secs: 9_999, source: "test" }, Some(ledger_path(&run, me)));
        assert_eq!(std::fs::read_to_string(ledger_path(&run, me)).unwrap(), format!("{} 0 0", mine.start_ticks));
        let got = uncounted(&run, vec![mine.clone()]);
        assert!(got[0].ledgered);
        armed.finish();
        assert!(!uncounted(&run, vec![mine])[0].ledgered);
    }

    #[test_id(tjzjn40)]
    #[test]
    fn arm_removes_the_ledgers_of_runs_that_died() {
        let dir = testkit::TempDir::new("testenv-cap-sweep");
        let run = dir.path().to_path_buf();
        let me = std::process::id();
        let mine = live().into_iter().find(|q| q.pid == me).unwrap();
        let dead = ledger_path(&run, 4_000_000);
        let reused = ledger_path(&run, me + 1_000_000);
        let other = ledger_path(&run, 1);
        std::fs::create_dir_all(dead.parent().unwrap()).unwrap();
        std::fs::write(&dead, "5 0 0").unwrap();
        std::fs::write(&reused, "5 0 0").unwrap();
        std::fs::write(&other, format!("{} 0 0", mine.start_ticks + 12345)).unwrap();
        let armed = arm(Cap { secs: 9_999, source: "test" }, Some(ledger_path(&run, me)));
        assert!(!dead.exists() && !reused.exists() && !other.exists());
        assert!(ledger_path(&run, me).exists());
        armed.finish();
    }

    #[test_id(tp5tzuv)]
    #[test]
    fn a_ledgered_run_is_not_given_the_flat_slot_wait() {
        let mut never = p(1, 660, &["testenv", "b"]);
        never.ledgered = true;
        assert!(breaches(std::slice::from_ref(&never), 600, 300, 0, 600, GRACE, None).is_empty());
        never.age_secs = 661;
        assert_eq!(breaches(&[never], 600, 300, 0, 600, GRACE, None).len(), 1);
    }

    #[test_id(t0lklm8)]
    #[test]
    fn a_dash_dash_branch_named_like_a_subcommand_is_still_a_run() {
        assert!(is_run(&["testenv".into(), "--".into(), "lint".into()]));
        assert!(!is_run(&["testenv".into(), "lint".into()]));
        assert!(!is_run(&["testenv".into(), "sandbox-supervise".into(), "rec".into(), "bwrap".into()]));
    }

    #[test_id(tmpkug3)]
    #[test]
    fn suites_is_no_longer_a_subcommand_so_it_is_a_branch_name() {
        assert!(is_run(&["testenv".into(), "suites".into()]));
    }

    #[test_id(tmx0m0i)]
    #[test]
    fn a_pid_whose_cmdline_changed_is_not_killed() {
        let me = std::process::id();
        let mut proc = live().into_iter().find(|q| q.pid == me).unwrap();
        proc.argv.push("not-what-was-judged".into());
        assert!(!kill_verified(&proc));
    }
}
