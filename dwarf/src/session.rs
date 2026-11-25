//! The model session and what watches it: the launch (its own process group, stdin from the
//! task file, stdout+stderr appended to the trace), the liveness lease / thrash heartbeat,
//! and the dwarf's own TERM/INT.
//!
//! PROCESS MODEL (DESIGN.md §8.3). dwarf.sh's heartbeat killed its own process group
//! (`kill -TERM -$$`), which also delivered TERM to dwarf.sh and ran its EXIT trap. Here the
//! session runs in a group of its own; a trip signals THAT group and records the signal on
//! `Stop`, and the main thread goes straight to teardown with rc 143 — the same outcome.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::decide::{hb_tick, HbTick};
use crate::util;

/// Shared between the main thread, the heartbeat and the signal watcher.
#[derive(Default)]
pub struct Stop {
    /// The signal that ended this dwarf (0: none). 15 for a heartbeat trip.
    pub sig: AtomicI32,
    /// The running session's process group (0: none running).
    pub pgid: AtomicI32,
    /// Epoch at which a wait on a harness-owned slot began (0: not waiting).
    pub harness_wait: AtomicI64,
    sent_back: std::sync::Mutex<Option<String>>,
}

impl Stop {
    /// Marks a harness wait until the guard drops, on every return path.
    pub fn harness_waiting(&self, since: i64) -> HarnessWait<'_> {
        self.harness_wait.store(since.max(1), Ordering::SeqCst);
        HarnessWait(self)
    }
    pub fn mark_sent_back(&self, why: &str) {
        *self.sent_back.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.to_string());
    }
    /// The state and reason the heartbeat saw when it stopped the session.
    pub fn sent_back_why(&self) -> Option<String> {
        self.sent_back.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn trip(&self, sig: i32) {
        let _ = self.sig.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        self.kill_session();
    }
    /// Records the signal without touching the session: the launcher decides when it dies.
    pub fn record(&self, sig: i32) {
        let _ = self.sig.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
    }
    pub fn kill_session(&self) {
        let g = self.pgid.load(Ordering::SeqCst);
        if g > 0 {
            // SAFETY: signalling a process group we created; never 0 (our own group).
            unsafe {
                libc::kill(-g, libc::SIGTERM);
            }
        }
    }
    pub fn signalled(&self) -> Option<i32> {
        match self.sig.load(Ordering::SeqCst) {
            0 => None,
            s => Some(s),
        }
    }
}

pub struct HarnessWait<'a>(&'a Stop);

impl Drop for HarnessWait<'_> {
    fn drop(&mut self) {
        self.0.harness_wait.store(0, Ordering::SeqCst);
    }
}

/// Everything one session launch needs.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub prog: String,
    pub args: Vec<String>,
    pub stdin_file: PathBuf,
    pub log: PathBuf,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    /// RUNE_TIMEOUT_SECONDS: killed after this, rc 124 (`timeout`'s code).
    pub timeout: Option<u64>,
    /// The world-halted stamp: while it exists, a signal waits for a turn boundary first.
    pub halted: Option<PathBuf>,
}

pub trait Launcher: Send + Sync {
    /// Runs the session to its end; its exit code (124 on timeout, 128+sig when signalled).
    fn run(&self, spec: &SessionSpec, stop: &Stop) -> i32;
}

pub struct RealLauncher;

impl Launcher for RealLauncher {
    fn run(&self, spec: &SessionSpec, stop: &Stop) -> i32 {
        use std::os::unix::process::CommandExt;
        let Ok(stdin) = std::fs::File::open(&spec.stdin_file) else { return 1 };
        let Ok(out) = OpenOptions::new().create(true).append(true).open(&spec.log) else { return 1 };
        let Ok(err) = out.try_clone() else { return 1 };
        let mut cmd = std::process::Command::new(&spec.prog);
        cmd.args(&spec.args).env_clear().envs(spec.env.iter()).current_dir(&spec.cwd).stdin(stdin).stdout(out).stderr(err).process_group(0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                if let Ok(mut f) = OpenOptions::new().append(true).open(&spec.log) {
                    use std::io::Write;
                    let _ = writeln!(f, "dwarf: cannot start {}: {e}", spec.prog);
                }
                return 127;
            }
        };
        stop.pgid.store(child.id() as i32, Ordering::SeqCst);
        if stop.signalled().is_some() {
            stop.kill_session();
        }
        let start = Instant::now();
        let mut timed_out = false;
        let mut forwarded = false;
        let mut signalled_at: Option<Instant> = None;
        let code = loop {
            match child.try_wait() {
                Ok(Some(s)) => break util::exit_code(s),
                Ok(None) => {}
                Err(_) => break 1,
            }
            if !timed_out && spec.timeout.is_some_and(|t| start.elapsed() >= Duration::from_secs(t)) {
                timed_out = true;
                stop.kill_session();
            }
            if !forwarded && stop.signalled().is_some() {
                let at = *signalled_at.get_or_insert_with(Instant::now);
                let halted = spec.halted.as_ref().is_some_and(|h| h.exists());
                let waiting = halted && at.elapsed() < Duration::from_secs(crate::checkpoint::BOUNDARY_WAIT_SECS) && !crate::checkpoint::at_boundary(&crate::checkpoint::trace_tail(&spec.log));
                if !waiting {
                    forwarded = true;
                    stop.kill_session();
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        stop.pgid.store(0, Ordering::SeqCst);
        if timed_out {
            124
        } else {
            code
        }
    }
}

/// Watch TERM and INT on the dwarf itself: record the signal and pass TERM to the session.
pub fn watch_signals(stop: Arc<Stop>) {
    let term = Arc::new(AtomicBool::new(false));
    let int = Arc::new(AtomicBool::new(false));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term));
    let _ = signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&int));
    std::thread::spawn(move || loop {
        if term.swap(false, Ordering::SeqCst) {
            stop.record(libc::SIGTERM);
        }
        if int.swap(false, Ordering::SeqCst) {
            stop.record(libc::SIGINT);
        }
        std::thread::sleep(Duration::from_millis(50));
    });
}

// ---- the heartbeat -------------------------------------------------------------------

/// What one beat needs from outside (so a test can drive it).
pub trait Beat: Send + Sync {
    fn now(&self) -> i64;
    fn trace_mtime(&self) -> i64;
    /// `dwarf_fuse_minutes` (an integer, or `?`/`gate`).
    fn fuse(&self) -> String;
    /// `trace_last`, first `n` bytes.
    fn trace_last(&self, n: usize) -> String;
    /// utime+stime, in clock ticks, summed over the descendants of `agent` (not `agent`
    /// itself); `None` when it has none.
    fn descendant_ticks(&self, agent: i32) -> Option<u64> {
        descendant_ticks(agent)
    }
    /// Renew the claim's lease to `lease_until` (epoch seconds), once per beat while the
    /// session runs: the lifecycle row's `spira-lc renew` (sp-2jf0a). False ends the
    /// heartbeat.
    fn renew(&self, lease_until: i64) -> bool;
    /// The row's state and reason when the bead is claimable again (READY or REWORK) after
    /// the last renewal; `None` while this session still holds it or the row cannot be read.
    fn sent_back(&self) -> Option<String>;
    /// Record how this session was cut short on the bead's lifecycle row (`spira-lc
    /// disposition`); the teardown reads it from there.
    fn disposition(&self, status: &str, note: &str);
    fn log(&self, msg: &str);
}

/// An identity lease outlives a missed beat or two, and no more: a dead dwarf's name and
/// capacity slot free within this window.
const IDENTITY_TTL_FLOOR: i64 = 90;

/// utime+stime of every descendant of `agent`, from `/proc/<pid>/stat`.
pub fn descendant_ticks(agent: i32) -> Option<u64> {
    fn walk(pid: u32, out: &mut Vec<u32>) {
        for c in spira_config::bounded::children(pid) {
            out.push(c);
            walk(c, out);
        }
    }
    let mut pids = Vec::new();
    walk(u32::try_from(agent).ok()?, &mut pids);
    if pids.is_empty() {
        return None;
    }
    let ticks = |pid: &u32| -> Option<u64> {
        let stat = std::fs::read_to_string(format!("{}/{pid}/stat", system_paths::PROC)).ok()?;
        let mut f = stat.rsplit_once(')')?.1.split_whitespace().skip(11);
        Some(f.next()?.parse::<u64>().ok()? + f.next()?.parse::<u64>().ok()?)
    };
    Some(pids.iter().filter_map(ticks).sum())
}

fn clock_ticks_per_sec() -> u64 {
    // SAFETY: sysconf reads a constant.
    u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).unwrap_or(100).max(1)
}

/// What the previous beat saw, to tell a still session from a quiet one.
struct Seen {
    ticks: Option<u64>,
    at: i64,
    transcript: i64,
    since: Option<i64>,
}

/// Removes the still file on every way out of the heartbeat.
struct StillFile(PathBuf);

impl Drop for StillFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub struct Heartbeat {
    pub bead: String,
    pub rune: String,
    pub run: PathBuf,
    pub lease_s: i64,
    pub every: Duration,
    pub wall_min: i64,
}

impl Heartbeat {
    fn lease_file(&self) -> PathBuf {
        self.run.join("dwarf").join(format!("{}.lease", self.bead))
    }

    fn still_file(&self) -> PathBuf {
        self.run.join("dwarf").join(format!("{}.still", self.bead))
    }

    /// The session is still when its descendants exist, its transcript is unwritten since the
    /// last beat, they used under 5% of a core in that time and it is not in a harness wait.
    fn watch_still(&self, b: &dyn Beat, stop: &Stop, transcript: i64, now: i64, seen: &mut Seen) {
        let agent = stop.pgid.load(Ordering::SeqCst);
        let ticks = if agent > 0 { b.descendant_ticks(agent) } else { None };
        let idle = match (ticks, seen.ticks) {
            (Some(t), Some(p)) if t >= p => (t - p) * 20 < (now - seen.at).max(1) as u64 * clock_ticks_per_sec(),
            _ => false,
        };
        let still = idle && transcript == seen.transcript && stop.harness_wait.load(Ordering::SeqCst) == 0;
        *seen = Seen { ticks, at: now, transcript, since: if still { seen.since.or(Some(now)) } else { None } };
        let file = self.still_file();
        match seen.since {
            Some(since) if since == now => {
                let doc = serde_json::json!({ "since": since, "transcript": transcript, "last": b.trace_last(200) });
                if let Some(p) = file.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let _ = util::write_atomic(&file, doc.to_string().as_bytes());
            }
            Some(_) => {}
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
    }

    fn identity_pidfile(&self) -> PathBuf {
        self.run.join(format!("dwarf-{}-{}.pid", self.rune, self.bead))
    }

    fn renew_identity(&self, now: i64) {
        if self.identity_pidfile().is_file() {
            strand::probe::write_lease(&self.identity_pidfile(), now + IDENTITY_TTL_FLOOR.max(3 * self.every.as_secs() as i64));
        }
    }

    fn write_lease(&self, deadline: i64) {
        let f = self.lease_file();
        if let Some(p) = f.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = util::write_atomic(&f, deadline.to_string().as_bytes());
    }

    /// The loop. Returns when told to shut down, when a renewal ends it, or after a trip.
    pub fn run(&self, b: &dyn Beat, stop: &Stop, shutdown: &AtomicBool) -> Option<HbTick> {
        let mut prev = b.trace_mtime();
        let session_start = b.now();
        let mut deadline = session_start + self.lease_s;
        self.write_lease(deadline);
        self.renew_identity(session_start);
        let _still = StillFile(self.still_file());
        let mut seen = Seen { ticks: None, at: session_start, transcript: prev, since: None };
        loop {
            let t = Instant::now();
            while t.elapsed() < self.every {
                if shutdown.load(Ordering::SeqCst) {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20).min(self.every));
            }
            if shutdown.load(Ordering::SeqCst) {
                return None;
            }
            let cur = b.trace_mtime();
            let now = b.now();
            let fuse = b.fuse();
            self.renew_identity(now);
            if crate::stop::requested(&self.run, &self.bead) {
                let why = crate::stop::reason(&self.run, &self.bead);
                crate::stop::clear(&self.run, &self.bead);
                b.disposition("slain", &why);
                b.log(&format!("{}: {} stop requested — stopping the session", self.rune, self.bead));
                stop.trip(libc::SIGTERM);
                return None;
            }
            match hb_tick(prev, cur, now, deadline, stop.harness_wait.load(Ordering::SeqCst) != 0, if fuse.is_empty() { "?" } else { &fuse }, self.wall_min, session_start) {
                HbTick::Renew => {
                    prev = cur;
                    deadline = now + self.lease_s;
                    self.write_lease(deadline);
                }
                HbTick::Lapse => {
                    let last = b.trace_last(200);
                    let quiet = now - cur;
                    let last = if last.is_empty() { "?".to_string() } else { last };
                    b.disposition("lapsed", &format!("{quiet}\t{last}"));
                    b.log(&format!("{}: {} lease lapsed (trace quiet {quiet}s, last: {last}) — killing", self.rune, self.bead));
                    stop.trip(libc::SIGTERM);
                    return Some(HbTick::Lapse);
                }
                HbTick::Thrash => {
                    let dsess = (now - session_start) / 60;
                    let last = b.trace_last(300);
                    let last = if last.is_empty() { "no last action".to_string() } else { last };
                    b.disposition("thrash", &last);
                    b.log(&format!(
                        "{}: {} deliverable stalled {fuse}m session {dsess}m (wall {}m) — requeueing for thrash",
                        self.rune, self.bead, self.wall_min
                    ));
                    stop.trip(libc::SIGTERM);
                    return Some(HbTick::Thrash);
                }
                HbTick::Ok => {}
            }
            self.watch_still(b, stop, cur, now, &mut seen);
            if !b.renew(now + self.lease_s) {
                return None;
            }
            if let Some(why) = b.sent_back() {
                b.log(&format!("{}: {} was sent back ({why}) while this session ran — stopping it", self.rune, self.bead));
                stop.mark_sent_back(&why);
                stop.trip(libc::SIGTERM);
                return None;
            }
        }
    }
}

pub fn mtime(p: &Path) -> i64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).map(|m| m.mtime()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeBeat {
        now: Mutex<i64>,
        mtimes: Mutex<Vec<i64>>,
        fuse: String,
        logs: Mutex<Vec<String>>,
        beats: Mutex<u32>,
        /// Every deadline a beat renewed to.
        renewals: Mutex<Vec<i64>>,
        /// End the heartbeat after this many renewals (0: never).
        stop_after: usize,
        /// Every (status, note) the heartbeat recorded as the session's disposition.
        dispositions: Mutex<Vec<(String, String)>>,
        /// The beat (1-based) from which `sent_back` answers `Some(..)` (0: never).
        sent_back_at: usize,
        /// What each `descendant_ticks` call answers (the last repeats); empty: no descendants.
        ticks: Mutex<Vec<Option<u64>>>,
        /// Read the real process table instead of `ticks`.
        real_ticks: bool,
        /// The still file's text as each renewal found it.
        still_seen: Mutex<Vec<Option<String>>>,
        run: PathBuf,
    }
    impl Beat for FakeBeat {
        fn descendant_ticks(&self, agent: i32) -> Option<u64> {
            if self.real_ticks {
                return descendant_ticks(agent);
            }
            let mut t = self.ticks.lock().unwrap();
            if t.len() > 1 {
                t.remove(0)
            } else {
                t.first().copied().flatten()
            }
        }
        fn sent_back(&self) -> Option<String> {
            let n = *self.beats.lock().unwrap() as usize;
            (self.sent_back_at != 0 && n >= self.sent_back_at).then(|| "REWORK suites-failed".to_string())
        }
        fn now(&self) -> i64 {
            let mut n = self.now.lock().unwrap();
            *n += 60;
            *n
        }
        fn trace_mtime(&self) -> i64 {
            let mut m = self.mtimes.lock().unwrap();
            if m.len() > 1 {
                m.remove(0)
            } else {
                m[0]
            }
        }
        fn fuse(&self) -> String {
            self.fuse.clone()
        }
        fn trace_last(&self, _n: usize) -> String {
            "{\"type\":\"assistant\"}".into()
        }
        fn renew(&self, lease_until: i64) -> bool {
            *self.beats.lock().unwrap() += 1;
            self.still_seen.lock().unwrap().push(std::fs::read_to_string(self.run.join("dwarf/sp-a.still")).ok());
            let mut r = self.renewals.lock().unwrap();
            r.push(lease_until);
            self.stop_after == 0 || r.len() < self.stop_after
        }
        fn disposition(&self, status: &str, note: &str) {
            self.dispositions.lock().unwrap().push((status.into(), note.into()));
        }
        fn log(&self, m: &str) {
            self.logs.lock().unwrap().push(m.into());
        }
    }

    fn tmp(n: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("dwarf-hb-{n}"))
    }

    // A silent trace lapses the lease, records a lapsed disposition, trips the stop.
    #[test_id(tmumlte)]
    #[test]
    fn silent_trace_lapses_and_trips() {
        let d = tmp("lapse");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 120, every: Duration::from_millis(1), wall_min: 20 };
        let b = FakeBeat { now: Mutex::new(0), mtimes: Mutex::new(vec![5]), fuse: "0".into(), logs: Mutex::new(vec![]), beats: Mutex::new(0), ..Default::default() };
        let stop = Stop::default();
        let r = hb.run(&b, &stop, &AtomicBool::new(false));
        assert_eq!(r, Some(HbTick::Lapse));
        assert_eq!(stop.signalled(), Some(15));
        let recorded = b.dispositions.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "lapsed");
        assert!(recorded[0].1.contains("\t{\"type\":\"assistant\"}"));
        assert!(!d.join("sp-a.lapsed").exists(), "no marker file: the row carries the outcome");
        assert!(b.logs.lock().unwrap()[0].contains("sp-a lease lapsed (trace quiet"));
        assert!(d.join("dwarf/sp-a.lease").is_file());
    }

    // Waiting on a harness-owned slot renews however long the trace is still; the quiet clock
    // restarts when the wait ends.
    #[test_id(tamyybd)]
    #[test]
    fn a_harness_wait_longer_than_the_lease_never_lapses_and_the_clock_restarts_after() {
        let d = tmp("hwait");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 120, every: Duration::from_millis(1), wall_min: 10_000 };
        let b = FakeBeat { mtimes: Mutex::new(vec![5]), fuse: "0".into(), stop_after: 6, ..Default::default() };
        let stop = Stop::default();
        let wait = stop.harness_waiting(1);
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), None, "ended by the renewal count, not a lapse");
        assert_eq!(b.renewals.lock().unwrap().len(), 6, "six beats = 360s of still trace against a 120s lease");
        assert!(b.dispositions.lock().unwrap().is_empty());
        assert!(!b.logs.lock().unwrap().iter().any(|l| l.contains("lease lapsed")));
        assert_eq!(stop.signalled(), None);
        drop(wait);
        assert_eq!(stop.harness_wait.load(Ordering::SeqCst), 0);
        let b2 = FakeBeat { now: Mutex::new(*b.now.lock().unwrap()), mtimes: Mutex::new(vec![5]), fuse: "0".into(), ..Default::default() };
        assert_eq!(hb.run(&b2, &stop, &AtomicBool::new(false)), Some(HbTick::Lapse), "a still trace lapses as before once the wait is over");
        assert_eq!(b2.dispositions.lock().unwrap()[0].0, "lapsed");
    }

    // turns advance, the deliverable does not → thrash.
    #[test_id(th45cj0)]
    #[test]
    fn growing_trace_with_a_stale_fuse_thrashes() {
        let d = tmp("thrash");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_millis(1), wall_min: 2 };
        let b = FakeBeat { now: Mutex::new(0), mtimes: Mutex::new(vec![1, 2, 2, 2, 2, 2]), fuse: "30".into(), logs: Mutex::new(vec![]), beats: Mutex::new(0), ..Default::default() };
        let stop = Stop::default();
        let r = hb.run(&b, &stop, &AtomicBool::new(false));
        assert_eq!(r, Some(HbTick::Thrash));
        let recorded = b.dispositions.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "thrash");
        assert!(recorded[0].1.starts_with("{\"type\""));
        assert!(!d.join("sp-a.thrash").exists(), "no marker file: the row carries the outcome");
        assert!(b.logs.lock().unwrap()[0].contains("deliverable stalled 30m session"));
        assert!(*b.beats.lock().unwrap() >= 1, "bd heartbeat ran on the renewing beats");
    }

    /// sp-2jf0a: a session many leases long renews on every beat, each renewal a fresh
    /// `now + lease` — so the claim's lease advances for as long as the session runs, and
    /// stops advancing the moment the heartbeat does (a dead holder's lease just expires).
    #[test_id(txjm6iq)]
    #[test]
    fn a_session_longer_than_its_lease_renews_on_every_beat_with_an_advancing_deadline() {
        let d = tmp("renew");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 120, every: Duration::from_millis(1), wall_min: 10_000 };
        // The trace grows every beat: a live session. now() steps 60s a call.
        let b = FakeBeat { mtimes: Mutex::new((1..=40).collect()), fuse: "?".into(), stop_after: 20, ..Default::default() };
        let stop = Stop::default();
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), None);
        assert_eq!(stop.signalled(), None, "a live session is never tripped");
        let r = b.renewals.lock().unwrap().clone();
        assert_eq!(r.len(), 20);
        assert!(r.windows(2).all(|w| w[1] > w[0]), "every renewal advances: {r:?}");
        assert!(*r.last().unwrap() - r[0] > 120 * 5, "the session outlived several leases: {r:?}");
        assert!(r.iter().all(|&u| u % 60 == 0 && u >= 120 + 60), "each deadline is that beat's now + lease: {r:?}");
    }

    #[test_id(tq0039v)]
    #[test]
    fn a_stop_request_trips_the_session_and_is_consumed() {
        let d = tmp("stopreq");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_millis(1), wall_min: 10_000 };
        std::fs::create_dir_all(d.join("dwarf")).unwrap();
        std::fs::write(crate::stop::request_file(&d, "sp-a"), "why\n").unwrap();
        let b = FakeBeat { mtimes: Mutex::new((1..=10).collect()), fuse: "?".into(), ..Default::default() };
        let stop = Stop::default();
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), None);
        assert_eq!(stop.signalled(), Some(libc::SIGTERM));
        assert!(!crate::stop::requested(&d, "sp-a"), "the request is consumed");
        assert!(b.logs.lock().unwrap()[0].contains("stop requested"));
        assert_eq!(b.dispositions.lock().unwrap().clone(), vec![("slain".to_string(), "why".to_string())]);
    }

    #[test_id(tbkwnxq)]
    #[test]
    fn a_stop_request_during_a_harness_wait_still_ends_the_session() {
        let d = tmp("stopwait");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_millis(1), wall_min: 10_000 };
        std::fs::create_dir_all(d.join("dwarf")).unwrap();
        std::fs::write(crate::stop::request_file(&d, "sp-a"), "why\n").unwrap();
        let b = FakeBeat { mtimes: Mutex::new(vec![5]), fuse: "?".into(), ..Default::default() };
        let stop = Stop::default();
        let _wait = stop.harness_waiting(1);
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), None);
        assert_eq!(stop.signalled(), Some(libc::SIGTERM));
        assert_eq!(b.dispositions.lock().unwrap().clone(), vec![("slain".to_string(), "why".to_string())]);
    }

    #[test_id(t1182r0)]
    #[test]
    fn every_beat_renews_the_identity_lease_to_a_short_window() {
        let d = tmp("idlease");
        let pf = d.join("dwarf-builder-sp-a.pid");
        std::fs::write(&pf, "1\n").unwrap();
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 6000, every: Duration::from_millis(1), wall_min: 10_000 };
        let b = FakeBeat { mtimes: Mutex::new((1..=10).collect()), fuse: "?".into(), stop_after: 3, ..Default::default() };
        assert_eq!(hb.run(&b, &Stop::default(), &AtomicBool::new(false)), None);
        let deadline: i64 = std::fs::read_to_string(strand::probe::lease_file(&pf)).unwrap().trim().parse().unwrap();
        assert!(deadline <= 60 * 4 + IDENTITY_TTL_FLOOR, "the identity lease is the short liveness window, not the session lease: {deadline}");
    }

    #[test_id(tft4rja)]
    #[test]
    fn shutdown_ends_quietly() {
        let d = tmp("down");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "b".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_secs(60), wall_min: 20 };
        let b = FakeBeat { now: Mutex::new(0), mtimes: Mutex::new(vec![1]), fuse: "?".into(), logs: Mutex::new(vec![]), beats: Mutex::new(0), ..Default::default() };
        let stop = Stop::default();
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(true)), None);
        assert_eq!(stop.signalled(), None);
    }

    #[test_id(tohgwm0)]
    #[test]
    fn a_send_back_stops_the_session_and_marks_it() {
        let d = tmp("sentback");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_millis(1), wall_min: 10_000 };
        let b = FakeBeat { mtimes: Mutex::new((1..=40).collect()), fuse: "?".into(), sent_back_at: 3, ..Default::default() };
        let stop = Stop::default();
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), None);
        assert_eq!(stop.signalled(), Some(libc::SIGTERM));
        assert_eq!(stop.sent_back_why().as_deref(), Some("REWORK suites-failed"));
        assert_eq!(*b.beats.lock().unwrap(), 3);
        assert!(b.logs.lock().unwrap().iter().any(|l| l == "builder: sp-a was sent back (REWORK suites-failed) while this session ran — stopping it"));
        assert!(b.dispositions.lock().unwrap().is_empty(), "the row is not touched");
    }

    #[test_id(t9ylooq)]
    #[test]
    fn a_bead_never_sent_back_keeps_renewing() {
        let d = tmp("notsentback");
        let hb = Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_millis(1), wall_min: 10_000 };
        let b = FakeBeat { mtimes: Mutex::new((1..=40).collect()), fuse: "?".into(), stop_after: 6, ..Default::default() };
        let stop = Stop::default();
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), None);
        assert_eq!(b.renewals.lock().unwrap().len(), 6);
        assert_eq!(stop.signalled(), None);
        assert_eq!(stop.sent_back_why(), None);
    }

    fn still_hb(d: &testkit::TempDir) -> Heartbeat {
        Heartbeat { bead: "sp-a".into(), rune: "builder".into(), run: d.to_path_buf(), lease_s: 6000, every: Duration::from_millis(1), wall_min: 10_000 }
    }

    fn idle_then(n: usize) -> Vec<Option<u64>> {
        vec![Some(7); n]
    }

    #[test_id(tyxnk9r)]
    #[test]
    fn a_sleeping_child_of_the_agent_leaves_a_still_file_that_a_return_removes() {
        let d = tmp("still-real");
        let mut agent = std::process::Command::new("sh").args(["-c", "sleep 30 & echo up; wait"]).stdout(std::process::Stdio::piped()).spawn().unwrap();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(agent.stdout.take().unwrap()), &mut String::new()).unwrap();
        let stop = Stop::default();
        stop.pgid.store(agent.id() as i32, Ordering::SeqCst);
        let b = FakeBeat { mtimes: Mutex::new(vec![5]), fuse: "?".into(), stop_after: 4, real_ticks: true, run: d.to_path_buf(), ..Default::default() };
        assert_eq!(still_hb(&d).run(&b, &stop, &AtomicBool::new(false)), None);
        let _ = agent.kill();
        let _ = agent.wait();
        let seen = b.still_seen.lock().unwrap().clone();
        let doc: serde_json::Value = serde_json::from_str(seen.last().unwrap().as_deref().expect("a sleeping child is still")).unwrap();
        assert_eq!((doc["transcript"].as_i64(), doc["last"].as_str()), (Some(5), Some("{\"type\":\"assistant\"}")));
        assert!(doc["since"].as_i64().unwrap() > 0);
        assert!(!d.join("dwarf/sp-a.still").exists(), "the heartbeat's end removes it");
    }

    #[test_id(tf0u3kx)]
    #[test]
    fn a_busy_child_leaves_no_still_file() {
        let d = tmp("still-busy");
        let stop = Stop::default();
        stop.pgid.store(1, Ordering::SeqCst);
        let busy = (0..8).map(|n| Some(n * 6000)).collect();
        let b = FakeBeat { mtimes: Mutex::new(vec![5]), fuse: "?".into(), stop_after: 8, ticks: Mutex::new(busy), run: d.to_path_buf(), ..Default::default() };
        still_hb(&d).run(&b, &stop, &AtomicBool::new(false));
        assert!(b.still_seen.lock().unwrap().iter().all(Option::is_none));
    }

    #[test_id(t8mhq2w)]
    #[test]
    fn a_child_with_no_descendants_leaves_no_still_file() {
        let d = tmp("still-none");
        let stop = Stop::default();
        stop.pgid.store(1, Ordering::SeqCst);
        let b = FakeBeat { mtimes: Mutex::new(vec![5]), fuse: "?".into(), stop_after: 6, run: d.to_path_buf(), ..Default::default() };
        still_hb(&d).run(&b, &stop, &AtomicBool::new(false));
        assert!(b.still_seen.lock().unwrap().iter().all(Option::is_none));
    }

    #[test_id(tzp4c1e)]
    #[test]
    fn a_harness_wait_leaves_no_still_file() {
        let d = tmp("still-hwait");
        let stop = Stop::default();
        stop.pgid.store(1, Ordering::SeqCst);
        let _wait = stop.harness_waiting(1);
        let b = FakeBeat { mtimes: Mutex::new(vec![5]), fuse: "?".into(), stop_after: 6, ticks: Mutex::new(idle_then(1)), run: d.to_path_buf(), ..Default::default() };
        still_hb(&d).run(&b, &stop, &AtomicBool::new(false));
        assert!(b.still_seen.lock().unwrap().iter().all(Option::is_none));
    }

    #[test_id(t5v7jhb)]
    #[test]
    fn a_transcript_write_removes_the_still_file() {
        let d = tmp("still-write");
        let stop = Stop::default();
        stop.pgid.store(1, Ordering::SeqCst);
        let b = FakeBeat { mtimes: Mutex::new(vec![5, 5, 5, 5, 9]), fuse: "?".into(), stop_after: 6, ticks: Mutex::new(idle_then(1)), run: d.to_path_buf(), ..Default::default() };
        still_hb(&d).run(&b, &stop, &AtomicBool::new(false));
        let seen: Vec<bool> = b.still_seen.lock().unwrap().iter().map(Option::is_some).collect();
        assert_eq!(seen, [false, true, true, false, true, true], "cleared by the write at beat 4, a fresh still period from beat 5: {seen:?}");
    }

    #[test_id(tq2n8ds)]
    #[test]
    fn a_lapse_removes_the_still_file() {
        let d = tmp("still-lapse");
        let stop = Stop::default();
        stop.pgid.store(1, Ordering::SeqCst);
        let hb = Heartbeat { lease_s: 150, ..still_hb(&d) };
        let b = FakeBeat { mtimes: Mutex::new(vec![5]), fuse: "0".into(), ticks: Mutex::new(idle_then(1)), run: d.to_path_buf(), ..Default::default() };
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), Some(HbTick::Lapse));
        assert!(b.still_seen.lock().unwrap().iter().any(Option::is_some), "still before the lapse");
        assert!(!d.join("dwarf/sp-a.still").exists());
    }
}
