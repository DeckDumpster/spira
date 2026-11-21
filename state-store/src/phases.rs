//! Where one request's time went inside the store. [`track`] installs a [`Recorder`] for the
//! current thread; [`time`] and [`begin`] record into it and do nothing when none is installed,
//! so the gc, the shipper and the sweep, which are never tracked, pay one thread-local read.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    DeadlinesWait,
    WriterWait,
    Commit,
    ReaderWait,
    Read,
    Drain,
    ContentWait,
    ContentQuery,
    Child,
}

const PHASES: usize = 9;

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::DeadlinesWait => "deadlines_wait",
            Phase::WriterWait => "writer_wait",
            Phase::Commit => "commit",
            Phase::ReaderWait => "reader_wait",
            Phase::Read => "read",
            Phase::Drain => "drain",
            Phase::ContentWait => "content_wait",
            Phase::ContentQuery => "content_query",
            Phase::Child => "child",
        }
    }

    const ALL: [Phase; PHASES] = [
        Phase::DeadlinesWait,
        Phase::WriterWait,
        Phase::Commit,
        Phase::ReaderWait,
        Phase::Read,
        Phase::Drain,
        Phase::ContentWait,
        Phase::ContentQuery,
        Phase::Child,
    ];
}

#[derive(Default)]
struct Inner {
    sum: [Duration; PHASES],
    count: [u64; PHASES],
    commits: Vec<Duration>,
    programs: Vec<String>,
    open: Vec<Phase>,
    read_max: Duration,
    reads_ended: u64,
    reads_overran: u64,
}

/// What a request recorded; shared between the worker thread and the one that answers.
#[derive(Default)]
pub struct Recorder(Mutex<Inner>);

/// A [`Recorder`] read at one moment.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Microseconds summed per phase that was recorded at least once.
    pub phases: Vec<(&'static str, u64)>,
    /// The duration of each commit, in order.
    pub commits_us: Vec<u64>,
    /// SQLite read transactions.
    pub reads: u64,
    /// The longest single read transaction, in microseconds.
    pub read_max_us: u64,
    /// Reads stopped while running for outliving `max_read_ms`.
    pub reads_ended: u64,
    /// Reads that returned past `max_read_ms` without being stopped in time; expected 0.
    pub reads_overran: u64,
    /// The innermost phase still running.
    pub open: Option<&'static str>,
    /// Dolt SQL statements run.
    pub content_queries: u64,
    /// Child processes run.
    pub children: u64,
    /// Basename of each child's program, in order.
    pub programs: Vec<String>,
}

impl Recorder {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn add(&self, phase: Phase, took: Duration) {
        let mut g = self.inner();
        g.sum[phase as usize] += took;
        g.count[phase as usize] += 1;
        if phase == Phase::Commit {
            g.commits.push(took);
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let g = self.inner();
        Snapshot {
            phases: Phase::ALL
                .iter()
                .filter(|p| g.count[**p as usize] > 0)
                .map(|p| (p.name(), g.sum[*p as usize].as_micros() as u64))
                .collect(),
            commits_us: g.commits.iter().map(|d| d.as_micros() as u64).collect(),
            reads: g.count[Phase::Read as usize],
            read_max_us: g.read_max.as_micros() as u64,
            reads_ended: g.reads_ended,
            reads_overran: g.reads_overran,
            open: g.open.last().map(|p| p.name()),
            content_queries: g.count[Phase::ContentQuery as usize],
            children: g.count[Phase::Child as usize],
            programs: g.programs.clone(),
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Recorder>>> = const { RefCell::new(None) };
}

/// Records this thread's phases into `recorder` until [`untrack`].
pub fn track(recorder: Arc<Recorder>) {
    CURRENT.with(|c| *c.borrow_mut() = Some(recorder));
}

/// Removes the recorder when dropped, so a panicking request leaves none installed.
pub struct Tracking;

impl Drop for Tracking {
    fn drop(&mut self) {
        untrack();
    }
}

pub fn track_scoped(recorder: Arc<Recorder>) -> Tracking {
    track(recorder);
    Tracking
}

pub fn untrack() {
    CURRENT.with(|c| *c.borrow_mut() = None);
}

thread_local! {
    static DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Clears the thread's request deadline when dropped.
pub struct DeadlineScope;

impl Drop for DeadlineScope {
    fn drop(&mut self) {
        DEADLINE.with(|d| d.set(None));
    }
}

/// A write on this thread that reaches its commit after `at` is refused unstarted.
pub fn deadline_scoped(at: Instant) -> DeadlineScope {
    DEADLINE.with(|d| d.set(Some(at)));
    DeadlineScope
}

pub fn deadline_passed() -> bool {
    DEADLINE.with(|d| d.get()).is_some_and(|at| Instant::now() >= at) // clock-safe: the deadline is a monotonic instant the server sets
}

thread_local! {
    static HOLDER: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// Clears the thread's holder label when dropped.
pub struct HolderScope;

impl Drop for HolderScope {
    fn drop(&mut self) {
        HOLDER.with(|h| *h.borrow_mut() = None);
    }
}

/// Names this thread, to whoever reports a lock it holds, as `verb` called by `caller`.
pub fn holder_scoped(verb: &str, caller: &str) -> HolderScope {
    HOLDER.with(|h| *h.borrow_mut() = Some((verb.to_string(), caller.to_string())));
    HolderScope
}

/// The thread's `(verb, caller)`; `unlabelled` when nothing named it.
pub fn holder() -> (String, String) {
    HOLDER.with(|h| h.borrow().clone()).unwrap_or_else(|| ("unlabelled".into(), String::new()))
}

fn current() -> Option<Arc<Recorder>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// A phase that ends, recording its duration, when dropped.
pub struct Open(Option<(Arc<Recorder>, Phase, Instant)>, bool);

impl Drop for Open {
    fn drop(&mut self) {
        let record = self.1;
        if let Some((rec, phase, at)) = self.0.take() {
            let mut g = rec.inner();
            if let Some(i) = g.open.iter().rposition(|p| *p == phase) {
                g.open.remove(i);
            }
            drop(g);
            if record {
                rec.add(phase, at.elapsed());
            }
        }
    }
}

impl Open {
    /// Ends the phase and records it; an `Open` from [`begin_unconfirmed`] dropped otherwise records nothing.
    pub fn confirm(mut self) {
        self.1 = true;
    }
}

pub fn begin(phase: Phase) -> Open {
    open(phase, true)
}

/// Like [`begin`], but the phase is recorded only if [`Open::confirm`]ed: a commit that errors out is no commit.
pub fn begin_unconfirmed(phase: Phase) -> Open {
    open(phase, false)
}

fn open(phase: Phase, record: bool) -> Open {
    Open(
        current().map(|rec| {
            rec.inner().open.push(phase);
            (rec, phase, Instant::now())
        }),
        record,
    )
}

pub fn time<T>(phase: Phase, f: impl FnOnce() -> T) -> T {
    let _open = begin(phase);
    f()
}

/// How a read transaction finished against `max_read_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadEnd {
    Finished,
    /// Stopped while running.
    Ended,
    /// Returned past the limit and was ended by the final check.
    Overran,
}

/// Records one read transaction's own duration and how it ended; nothing when no recorder is installed.
pub fn record_read(took: Duration, end: ReadEnd) {
    if let Some(rec) = current() {
        let mut g = rec.inner();
        g.read_max = g.read_max.max(took);
        match end {
            ReadEnd::Finished => {}
            ReadEnd::Ended => g.reads_ended += 1,
            ReadEnd::Overran => g.reads_overran += 1,
        }
    }
}

/// Times `f`, a child process of `program`, as one `child` and records the program's basename.
pub fn child<T>(program: &str, f: impl FnOnce() -> T) -> T {
    let out = time(Phase::Child, f);
    if let Some(rec) = current() {
        let base = program.rsplit('/').next().unwrap_or(program);
        rec.inner().programs.push(base.to_string());
    }
    out
}

/// How many commits the installed recorder holds; 0 when none is installed.
fn commits() -> usize {
    current().map_or(0, |r| r.inner().commits.len())
}

/// Runs a backend commit, recording it as one `commit` unless the backend recorded its own; a refused commit is none.
pub fn commit<T, E>(f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    let Some(rec) = current() else { return f() };
    let before = commits();
    let at = Instant::now();
    let out = f();
    if out.is_ok() && commits() == before {
        rec.add(Phase::Commit, at.elapsed());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked() -> Arc<Recorder> {
        let rec = Arc::new(Recorder::default());
        track(rec.clone());
        rec
    }

    #[test]
    fn with_no_recorder_installed_time_records_nothing() {
        untrack();
        let rec = Recorder::default();
        time(Phase::Commit, || ());
        drop(begin(Phase::Read));
        assert_eq!(rec.snapshot(), Snapshot::default());
        assert_eq!(time(Phase::Read, || 7), 7);
    }

    #[test]
    fn an_unconfirmed_phase_dropped_records_nothing_and_closes() {
        let rec = tracked();
        drop(begin_unconfirmed(Phase::Commit));
        begin_unconfirmed(Phase::Commit).confirm();
        untrack();
        let s = rec.snapshot();
        assert_eq!(s.commits_us.len(), 1);
        assert_eq!(s.open, None);
    }

    #[test]
    fn two_timed_commits_are_two_entries_in_order() {
        let rec = tracked();
        time(Phase::Commit, || ());
        time(Phase::Commit, || ());
        untrack();
        let s = rec.snapshot();
        assert_eq!(s.commits_us.len(), 2);
        assert_eq!(s.phases.iter().map(|p| p.0).collect::<Vec<_>>(), ["commit"]);
        assert_eq!(s.open, None);
    }

    #[test]
    fn a_begin_that_is_never_ended_is_the_open_phase() {
        let rec = tracked();
        std::mem::forget(begin(Phase::WriterWait));
        untrack();
        assert_eq!(rec.snapshot().open, Some("writer_wait"));
    }

    #[test]
    fn a_commit_the_backend_recorded_is_not_recorded_twice() {
        let rec = tracked();
        let _ = commit(|| time(Phase::Commit, || Ok::<(), ()>(())));
        let _ = commit(|| Ok::<(), ()>(()));
        untrack();
        assert_eq!(rec.snapshot().commits_us.len(), 2);
    }

    #[test]
    fn a_child_is_counted_and_named_by_its_basename() {
        let rec = tracked();
        child("tools/bd", || ());
        child("git", || ());
        untrack();
        let s = rec.snapshot();
        assert_eq!((s.children, s.programs), (2, vec!["bd".to_string(), "git".to_string()]));
    }

    #[test]
    fn a_read_is_counted() {
        let rec = tracked();
        time(Phase::Read, || ());
        untrack();
        assert_eq!(rec.snapshot().reads, 1);
    }

    #[test]
    fn a_refused_commit_is_not_recorded_as_a_commit() {
        let rec = tracked();
        let out: Result<(), ()> = commit(|| Err(()));
        untrack();
        assert!(out.is_err());
        assert_eq!(rec.snapshot().commits_us.len(), 0);
    }

    #[test]
    fn the_longest_single_read_is_kept_beside_the_sum() {
        let rec = Arc::new(Recorder::default());
        let _t = track_scoped(rec.clone());
        for us in [30, 70] {
            let d = Duration::from_micros(us);
            rec.add(Phase::Read, d);
            record_read(d, ReadEnd::Finished);
        }
        record_read(Duration::from_micros(5), ReadEnd::Ended);
        record_read(Duration::from_micros(5), ReadEnd::Overran);
        let s = rec.snapshot();
        assert_eq!(s.read_max_us, 70);
        assert_eq!(s.phases, vec![("read", 100)]);
        assert_eq!((s.reads_ended, s.reads_overran), (1, 1));
    }

    #[test]
    fn recording_a_read_with_no_recorder_installed_does_nothing() {
        record_read(Duration::from_micros(1), ReadEnd::Overran);
    }
}
