//! The model session and what watches it: the launch (its own process group, stdin from the
//! task file, stdout+stderr appended to the trace), the liveness lease / thrash heartbeat,
//! and the aeon's own TERM/INT.
//!
//! PROCESS MODEL (DESIGN.md §8.3). aeon.sh's heartbeat killed its own process group
//! (`kill -TERM -$$`), which also delivered TERM to aeon.sh and ran its EXIT trap. Here the
//! session runs in a group of its own; a trip signals THAT group and records the signal on
//! `Stop`, and the main thread goes straight to teardown with rc 143 — the same outcome.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::decide::{hb_tick, HbTick};
use crate::util;

/// Shared between the main thread, the heartbeat and the signal watcher.
#[derive(Default)]
pub struct Stop {
    /// The signal that ended this aeon (0: none). 15 for a heartbeat trip.
    pub sig: AtomicI32,
    /// The running session's process group (0: none running).
    pub pgid: AtomicI32,
}

impl Stop {
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

/// Everything one session launch needs.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub prog: String,
    pub args: Vec<String>,
    pub stdin_file: PathBuf,
    pub log: PathBuf,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    /// FAYTH_TIMEOUT_SECONDS: killed after this, rc 124 (`timeout`'s code).
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
                    let _ = writeln!(f, "aeon: cannot start {}: {e}", spec.prog);
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

/// Watch TERM and INT on the aeon itself: record the signal and pass TERM to the session.
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
    /// `aeon_fuse_minutes` (an integer, or `?`/`gate`).
    fn fuse(&self) -> String;
    /// `trace_last`, first `n` bytes.
    fn trace_last(&self, n: usize) -> String;
    /// Renew the claim's lease to `lease_until` (epoch seconds), once per beat while the
    /// session runs: the lifecycle row's `spira-lc renew` (sp-2jf0a). False ends the
    /// heartbeat.
    fn renew(&self, lease_until: i64) -> bool;
    fn log(&self, msg: &str);
}

/// An identity lease outlives a missed beat or two, and no more: a dead aeon's name and
/// capacity slot free within this window.
const IDENTITY_TTL_FLOOR: i64 = 90;

pub struct Heartbeat {
    pub bead: String,
    pub fayth: String,
    pub run: PathBuf,
    pub lease_s: i64,
    pub every: Duration,
    pub wall_min: i64,
}

impl Heartbeat {
    fn lease_file(&self) -> PathBuf {
        self.run.join("aeon").join(format!("{}.lease", self.bead))
    }

    fn identity_pidfile(&self) -> PathBuf {
        self.run.join(format!("aeon-{}-{}.pid", self.fayth, self.bead))
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
                crate::stop::clear(&self.run, &self.bead);
                b.log(&format!("{}: {} stop requested — stopping the session", self.fayth, self.bead));
                stop.trip(libc::SIGTERM);
                return None;
            }
            match hb_tick(prev, cur, now, deadline, if fuse.is_empty() { "?" } else { &fuse }, self.wall_min, session_start) {
                HbTick::Renew => {
                    prev = cur;
                    deadline = now + self.lease_s;
                    self.write_lease(deadline);
                }
                HbTick::Lapse => {
                    let last = b.trace_last(200);
                    let quiet = now - cur;
                    let last = if last.is_empty() { "?".to_string() } else { last };
                    let _ = std::fs::write(self.run.join(format!("{}.lapsed", self.bead)), format!("{quiet}\t{last}\n"));
                    b.log(&format!("{}: {} lease lapsed (trace quiet {quiet}s, last: {last}) — killing", self.fayth, self.bead));
                    stop.trip(libc::SIGTERM);
                    return Some(HbTick::Lapse);
                }
                HbTick::Thrash => {
                    let dsess = (now - session_start) / 60;
                    let last = b.trace_last(300);
                    let last = if last.is_empty() { "no last action".to_string() } else { last };
                    let _ = std::fs::write(self.run.join(format!("{}.thrash", self.bead)), format!("{last}\n"));
                    b.log(&format!(
                        "{}: {} deliverable stalled {fuse}m session {dsess}m (wall {}m) — requeueing for thrash",
                        self.fayth, self.bead, self.wall_min
                    ));
                    stop.trip(libc::SIGTERM);
                    return Some(HbTick::Thrash);
                }
                HbTick::Ok => {}
            }
            if !b.renew(now + self.lease_s) {
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
    }
    impl Beat for FakeBeat {
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
            let mut r = self.renewals.lock().unwrap();
            r.push(lease_until);
            self.stop_after == 0 || r.len() < self.stop_after
        }
        fn log(&self, m: &str) {
            self.logs.lock().unwrap().push(m.into());
        }
    }

    fn tmp(n: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("aeon-hb-{n}"))
    }

    // test-aeon-lease.sh: a silent trace lapses the lease, writes .lapsed, trips the stop.
    #[test]
    fn silent_trace_lapses_and_trips() {
        let d = tmp("lapse");
        let hb = Heartbeat { bead: "sp-a".into(), fayth: "builder".into(), run: d.to_path_buf(), lease_s: 120, every: Duration::from_millis(1), wall_min: 20 };
        let b = FakeBeat { now: Mutex::new(0), mtimes: Mutex::new(vec![5]), fuse: "0".into(), logs: Mutex::new(vec![]), beats: Mutex::new(0), ..Default::default() };
        let stop = Stop::default();
        let r = hb.run(&b, &stop, &AtomicBool::new(false));
        assert_eq!(r, Some(HbTick::Lapse));
        assert_eq!(stop.signalled(), Some(15));
        let lapsed = std::fs::read_to_string(d.join("sp-a.lapsed")).unwrap();
        assert!(lapsed.contains("\t{\"type\":\"assistant\"}"));
        assert!(b.logs.lock().unwrap()[0].contains("sp-a lease lapsed (trace quiet"));
        assert!(d.join("aeon/sp-a.lease").is_file());
    }

    // test-thrash-teardown.sh: turns advance, the deliverable does not → thrash.
    #[test]
    fn growing_trace_with_a_stale_fuse_thrashes() {
        let d = tmp("thrash");
        let hb = Heartbeat { bead: "sp-a".into(), fayth: "builder".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_millis(1), wall_min: 2 };
        let b = FakeBeat { now: Mutex::new(0), mtimes: Mutex::new(vec![1, 2, 2, 2, 2, 2]), fuse: "30".into(), logs: Mutex::new(vec![]), beats: Mutex::new(0), ..Default::default() };
        let stop = Stop::default();
        let r = hb.run(&b, &stop, &AtomicBool::new(false));
        assert_eq!(r, Some(HbTick::Thrash));
        assert!(std::fs::read_to_string(d.join("sp-a.thrash")).unwrap().starts_with("{\"type\""));
        assert!(b.logs.lock().unwrap()[0].contains("deliverable stalled 30m session"));
        assert!(*b.beats.lock().unwrap() >= 1, "bd heartbeat ran on the renewing beats");
    }

    /// sp-2jf0a: a session many leases long renews on every beat, each renewal a fresh
    /// `now + lease` — so the claim's lease advances for as long as the session runs, and
    /// stops advancing the moment the heartbeat does (a dead holder's lease just expires).
    #[test]
    fn a_session_longer_than_its_lease_renews_on_every_beat_with_an_advancing_deadline() {
        let d = tmp("renew");
        let hb = Heartbeat { bead: "sp-a".into(), fayth: "builder".into(), run: d.to_path_buf(), lease_s: 120, every: Duration::from_millis(1), wall_min: 10_000 };
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

    #[test]
    fn a_stop_request_trips_the_session_and_is_consumed() {
        let d = tmp("stopreq");
        let hb = Heartbeat { bead: "sp-a".into(), fayth: "builder".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_millis(1), wall_min: 10_000 };
        std::fs::create_dir_all(d.join("aeon")).unwrap();
        std::fs::write(crate::stop::request_file(&d, "sp-a"), "why\n").unwrap();
        let b = FakeBeat { mtimes: Mutex::new((1..=10).collect()), fuse: "?".into(), ..Default::default() };
        let stop = Stop::default();
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(false)), None);
        assert_eq!(stop.signalled(), Some(libc::SIGTERM));
        assert!(!crate::stop::requested(&d, "sp-a"), "the request is consumed");
        assert!(b.logs.lock().unwrap()[0].contains("stop requested"));
    }

    #[test]
    fn every_beat_renews_the_identity_lease_to_a_short_window() {
        let d = tmp("idlease");
        let pf = d.join("aeon-builder-sp-a.pid");
        std::fs::write(&pf, "1\n").unwrap();
        let hb = Heartbeat { bead: "sp-a".into(), fayth: "builder".into(), run: d.to_path_buf(), lease_s: 6000, every: Duration::from_millis(1), wall_min: 10_000 };
        let b = FakeBeat { mtimes: Mutex::new((1..=10).collect()), fuse: "?".into(), stop_after: 3, ..Default::default() };
        assert_eq!(hb.run(&b, &Stop::default(), &AtomicBool::new(false)), None);
        let deadline: i64 = std::fs::read_to_string(strand::probe::lease_file(&pf)).unwrap().trim().parse().unwrap();
        assert!(deadline <= 60 * 4 + IDENTITY_TTL_FLOOR, "the identity lease is the short liveness window, not the session lease: {deadline}");
    }

    #[test]
    fn shutdown_ends_quietly() {
        let d = tmp("down");
        let hb = Heartbeat { bead: "sp-a".into(), fayth: "b".into(), run: d.to_path_buf(), lease_s: 600, every: Duration::from_secs(60), wall_min: 20 };
        let b = FakeBeat { now: Mutex::new(0), mtimes: Mutex::new(vec![1]), fuse: "?".into(), logs: Mutex::new(vec![]), beats: Mutex::new(0), ..Default::default() };
        let stop = Stop::default();
        assert_eq!(hb.run(&b, &stop, &AtomicBool::new(true)), None);
        assert_eq!(stop.signalled(), None);
    }

    /// The trip signals the SESSION's group, never pgid 0 (the aeon's own).
    #[test]
    fn trip_signals_session_group() {
        let stop = Stop::default();
        stop.trip(15); // no session: must not signal ourselves (we are still here)
        assert_eq!(stop.signalled(), Some(15));
        let d = tmp("launch");
        let task = d.join("task.md");
        std::fs::write(&task, "hi").unwrap();
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
        let spec = SessionSpec { prog: "sh".into(), args: vec!["-c".into(), "cat; sleep 30".into()], stdin_file: task, log: d.join("log"), cwd: d.to_path_buf(), env, timeout: None, halted: None };
        let t0 = Instant::now();
        let rc = RealLauncher.run(&spec, &stop);
        assert!(t0.elapsed() < Duration::from_secs(10), "a stop already recorded kills the session at once");
        assert_eq!(rc, 143);
        // Without a stop the same launch reads the task on stdin into the trace.
        let spec2 = SessionSpec { args: vec!["-c".into(), "cat".into()], log: d.join("log2"), ..spec };
        assert_eq!(RealLauncher.run(&spec2, &Stop::default()), 0);
        assert_eq!(std::fs::read_to_string(d.join("log2")).unwrap(), "hi");
    }

    #[test]
    fn timeout_is_124() {
        let d = tmp("timeout");
        let task = d.join("task.md");
        std::fs::write(&task, "").unwrap();
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());
        let spec = SessionSpec { prog: "sleep".into(), args: vec!["30".into()], stdin_file: task, log: d.join("log"), cwd: d.to_path_buf(), env, timeout: Some(0), halted: None };
        assert_eq!(RealLauncher.run(&spec, &Stop::default()), 124);
    }
}
