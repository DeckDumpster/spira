//! Every blocking call the loop would make goes through here: a fixed set of worker threads
//! behind a fixed-depth queue. A full queue refuses the submission; it never blocks the
//! caller and never grows a thread.

use std::any::Any;
use std::collections::BTreeMap;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};

use ports::{Clock, SimFs, VirtualClock};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, OwnedSemaphorePermit};

pub const WORKERS: usize = 8;
pub const QUEUE_DEPTH: usize = 32;

/// A submission the queue refused. The work was not started and nothing was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spill {
    pub what: &'static str,
    pub depth: usize,
    pub cap: usize,
}

impl std::fmt::Display for Spill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "offload queue full for {}: {} of {}", self.what, self.depth, self.cap)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffloadError {
    pub what: &'static str,
    pub why: String,
}

impl std::fmt::Display for OffloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.what, self.why)
    }
}

impl std::error::Error for OffloadError {}

/// The result of work on the pool. Dropping it does not stop the work.
pub struct Offloaded<T> {
    what: &'static str,
    rx: oneshot::Receiver<std::result::Result<T, String>>,
}

impl<T> Offloaded<T> {
    /// The result if it has been delivered, without waiting; a simulation reads it this way.
    pub fn try_take(&mut self) -> Option<std::result::Result<T, OffloadError>> {
        let what = self.what;
        match self.rx.try_recv() {
            Ok(Ok(v)) => Some(Ok(v)),
            Ok(Err(why)) => Some(Err(OffloadError { what, why })),
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => Some(Err(OffloadError { what, why: "cancelled before it ran".into() })),
        }
    }
}

impl<T> Future for Offloaded<T> {
    type Output = std::result::Result<T, OffloadError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let what = self.what;
        Pin::new(&mut self.rx).poll(cx).map(|r| match r {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(why)) => Err(OffloadError { what, why }),
            Err(_) => Err(OffloadError { what, why: "cancelled before it ran".into() }),
        })
    }
}

/// Work handed to a `Jobs`: `run` does the work and returns what completes it, which delivers
/// the result and releases the permit. Threads complete a job the moment it has run; a
/// simulation completes it later.
pub type Run = Box<dyn FnOnce() -> Box<dyn FnOnce() + Send> + Send>;

/// Where the pool's work runs.
pub trait Jobs: Send + Sync {
    /// `Err(depth)` where the work was refused: the queue was full, or nothing is left to run it.
    fn submit(&self, what: &'static str, run: Run) -> std::result::Result<(), usize>;
    fn cap(&self) -> usize;
}

/// Worker threads behind a fixed-depth queue.
pub struct ThreadJobs {
    tx: SyncSender<Run>,
    queued: Arc<AtomicUsize>,
    cap: usize,
}

impl ThreadJobs {
    pub fn new(workers: usize, cap: usize) -> ThreadJobs {
        let (tx, rx) = sync_channel::<Run>(cap);
        let rx: Arc<Mutex<Receiver<Run>>> = Arc::new(Mutex::new(rx));
        let queued = Arc::new(AtomicUsize::new(0));
        for i in 0..workers.max(1) {
            let (rx, queued) = (rx.clone(), queued.clone());
            std::thread::Builder::new().name(format!("offload-{i}")).spawn(move || loop {
                let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                let Ok(job) = job else { return };
                queued.fetch_sub(1, Ordering::SeqCst);
                job()();
            })
            .expect("an offload worker thread starts");
        }
        ThreadJobs { tx, queued, cap }
    }
}

impl Jobs for ThreadJobs {
    fn submit(&self, _what: &'static str, run: Run) -> std::result::Result<(), usize> {
        let depth = self.queued.fetch_add(1, Ordering::SeqCst);
        match self.tx.try_send(run) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.queued.fetch_sub(1, Ordering::SeqCst);
                Err(if matches!(e, TrySendError::Disconnected(_)) { self.cap } else { depth.min(self.cap) })
            }
        }
    }

    fn cap(&self) -> usize {
        self.cap
    }
}

struct Due {
    at: u64,
    seq: u64,
    what: &'static str,
    complete: Box<dyn FnOnce() + Send>,
}

/// Jobs for a simulation. A job runs on `submit`, so its `SimFs` calls happen in submission
/// order; what it cost them, in virtual milliseconds, is when it completes. `run_next` moves the
/// clock to the earliest completion and delivers it.
pub struct SimJobs {
    clock: VirtualClock,
    fs: Arc<SimFs>,
    cap: usize,
    due: Mutex<Vec<Due>>,
    seq: AtomicU64,
}

impl SimJobs {
    pub fn new(clock: VirtualClock, fs: Arc<SimFs>, cap: usize) -> Arc<SimJobs> {
        Arc::new(SimJobs { clock, fs, cap, due: Mutex::new(Vec::new()), seq: AtomicU64::new(0) })
    }

    pub fn pending(&self) -> usize {
        self.due.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Completes the job due first, ties in submission order; its name and completion time.
    pub fn run_next(&self) -> Option<(&'static str, u64)> {
        let next = {
            let mut due = self.due.lock().unwrap_or_else(|e| e.into_inner());
            let i = (0..due.len()).min_by_key(|&i| (due[i].at, due[i].seq))?;
            due.remove(i)
        };
        self.clock.advance_to(next.at);
        (next.complete)();
        Some((next.what, next.at))
    }

    pub fn run_all(&self) -> Vec<&'static str> {
        std::iter::from_fn(|| self.run_next().map(|(what, _)| what)).collect()
    }
}

impl Jobs for SimJobs {
    fn submit(&self, what: &'static str, run: Run) -> std::result::Result<(), usize> {
        let depth = self.pending();
        if depth >= self.cap {
            return Err(depth);
        }
        let (start, before) = (self.clock.now_ms(), self.fs.accrued_ms());
        let complete = run();
        let cost = self.fs.accrued_ms() - before;
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        self.due.lock().unwrap_or_else(|e| e.into_inner()).push(Due { at: start + cost, seq, what, complete });
        Ok(())
    }

    fn cap(&self) -> usize {
        self.cap
    }
}

pub struct Pool {
    jobs: Arc<dyn Jobs>,
}

static SPILLED: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());

/// Submissions refused for `what` since this process started.
pub fn spilled_total(what: &str) -> u64 {
    SPILLED.lock().unwrap_or_else(|e| e.into_inner()).get(what).copied().unwrap_or(0)
}

fn panic_message(p: Box<dyn Any + Send>) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panicked".into())
}

impl Pool {
    pub fn new(workers: usize, cap: usize) -> Arc<Pool> {
        Self::over(Arc::new(ThreadJobs::new(workers, cap)))
    }

    pub fn over(jobs: Arc<dyn Jobs>) -> Arc<Pool> {
        Arc::new(Pool { jobs })
    }

    /// The process-wide pool, sized once.
    pub fn global() -> Arc<Pool> {
        static GLOBAL: OnceLock<Arc<Pool>> = OnceLock::new();
        GLOBAL.get_or_init(|| Pool::new(WORKERS, QUEUE_DEPTH)).clone()
    }

    /// `permit` is released when `f` returns, not when the caller stops waiting.
    pub fn offload<T: Send + 'static>(&self, what: &'static str, permit: Option<OwnedSemaphorePermit>, f: impl FnOnce() -> T + Send + 'static) -> std::result::Result<Offloaded<T>, Spill> {
        let (tx, rx) = oneshot::channel();
        let run: Run = Box::new(move || {
            let result = catch_unwind(AssertUnwindSafe(f)).map_err(panic_message);
            Box::new(move || {
                let _permit = permit;
                let _ = tx.send(result);
            })
        });
        match self.jobs.submit(what, run) {
            Ok(()) => Ok(Offloaded { what, rx }),
            Err(depth) => {
                *SPILLED.lock().unwrap_or_else(|e| e.into_inner()).entry(what).or_insert(0) += 1;
                Err(Spill { what, depth, cap: self.jobs.cap() })
            }
        }
    }
}

pub fn offload<T: Send + 'static>(what: &'static str, permit: Option<OwnedSemaphorePermit>, f: impl FnOnce() -> T + Send + 'static) -> std::result::Result<Offloaded<T>, Spill> {
    Pool::global().offload(what, permit, f)
}

/// The monotonic start of the handler call in progress; zero when none is.
pub struct Busy {
    base: Instant,
    since: AtomicU64,
}

impl Default for Busy {
    fn default() -> Self {
        Busy { base: Instant::now(), since: AtomicU64::new(0) }
    }
}

impl Busy {
    pub fn enter(&self) {
        self.since.store(self.base.elapsed().as_nanos() as u64 + 1, Ordering::SeqCst);
    }

    pub fn exit(&self) {
        self.since.store(0, Ordering::SeqCst);
    }

    pub fn held_for(&self) -> Duration {
        match self.since.load(Ordering::SeqCst) {
            0 => Duration::ZERO,
            s => self.base.elapsed().saturating_sub(Duration::from_nanos(s - 1)),
        }
    }
}

/// The step the engine's turn is in, and the longest step the turn has finished. Entering and
/// leaving take a lock held only to copy a few fields, so a slow store call cannot hold it.
#[derive(Default)]
pub struct Steps(Mutex<StepState>);

#[derive(Default)]
struct StepState {
    current: Option<(&'static str, String, Instant)>,
    longest: Option<(&'static str, Duration)>,
}

/// The step in progress, as the host reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepNow {
    pub name: &'static str,
    pub of: String,
    pub ran: Duration,
}

impl Steps {
    fn state(&self) -> std::sync::MutexGuard<'_, StepState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()) // loop-safe: held only to copy a few fields, never across a call
    }

    pub(crate) fn enter(self: &Arc<Self>, name: &'static str, of: &str) -> StepGuard {
        self.state().current = Some((name, of.to_string(), Instant::now()));
        StepGuard(self.clone())
    }

    /// Forgets the longest step: the next turn's record starts from nothing.
    pub fn begin_turn(&self) {
        self.state().longest = None;
    }

    pub fn now(&self) -> Option<StepNow> {
        self.state().current.as_ref().map(|(name, of, at)| StepNow { name, of: of.clone(), ran: at.elapsed() })
    }

    pub fn longest(&self) -> Option<(&'static str, Duration)> {
        self.state().longest
    }
}

pub(crate) struct StepGuard(Arc<Steps>);

impl Drop for StepGuard {
    fn drop(&mut self) {
        let mut s = self.0.state();
        if let Some((name, _, at)) = s.current.take() {
            let ran = at.elapsed();
            if s.longest.is_none_or(|(_, l)| ran > l) {
                s.longest = Some((name, ran));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t0l6gny)]
    #[test]
    fn busy_reads_zero_until_entered() {
        let b = Busy::default();
        assert_eq!(b.held_for(), Duration::ZERO);
        b.enter();
        std::thread::sleep(Duration::from_millis(20)); // clock-safe: measures a real elapsed interval
        assert!(b.held_for() >= Duration::from_millis(15));
        b.exit();
        assert_eq!(b.held_for(), Duration::ZERO);
    }

    fn sim(seed: u64) -> (VirtualClock, Arc<SimFs>, Arc<SimJobs>, Arc<Pool>) {
        let clock = VirtualClock::new(0);
        let fs = SimFs::new(seed, ports::SimFaults { delay_ms: (1, 100), ..Default::default() }, clock.clone());
        ports::Fs::create_dir(fs.as_ref(), std::path::Path::new("d")).unwrap();
        let jobs = SimJobs::new(clock.clone(), fs.clone(), 8);
        let pool = Pool::over(jobs.clone());
        (clock, fs, jobs, pool)
    }

    fn append_job(pool: &Pool, fs: &Arc<SimFs>, what: &'static str) -> Offloaded<()> {
        let fs = fs.clone();
        pool.offload(what, None, move || {
            ports::Fs::append(fs.as_ref(), std::path::Path::new("d/log"), what.as_bytes()).unwrap();
        })
        .unwrap()
    }

    fn completion_order(seed: u64) -> Vec<&'static str> {
        let (_, fs, jobs, pool) = sim(seed);
        let (_a, _b) = (append_job(&pool, &fs, "a"), append_job(&pool, &fs, "b"));
        jobs.run_all()
    }

    #[test_id(tb0u6dq)]
    #[test]
    fn a_seeded_delay_reorders_two_completions_and_the_same_seed_repeats_it() {
        let orders: Vec<_> = (0..16).map(completion_order).collect();
        assert!(orders.iter().any(|o| o == &["a", "b"]), "some seed leaves them in order: {orders:?}");
        assert!(orders.iter().any(|o| o == &["b", "a"]), "some seed reorders them: {orders:?}");
        for seed in 0..16 {
            assert_eq!(completion_order(seed), orders[seed as usize], "seed {seed} repeats");
        }
    }

    #[test_id(tj1k8sx)]
    #[test]
    fn a_sim_job_completes_at_the_virtual_time_its_fs_calls_cost() {
        let (clock, fs, jobs, pool) = sim(3);
        let before = fs.accrued_ms();
        let mut done = append_job(&pool, &fs, "a");
        assert!(done.try_take().is_none(), "nothing is delivered before its time");
        let (what, at) = jobs.run_next().unwrap();
        assert_eq!((what, at), ("a", fs.accrued_ms() - before));
        assert_eq!(clock.now_ms(), at);
        assert!(done.try_take().unwrap().is_ok());
    }

    #[test_id(t5pn2he)]
    #[test]
    fn a_sim_pool_refuses_past_its_depth_and_counts_the_spill() {
        let clock = VirtualClock::new(0);
        let fs = SimFs::new(1, Default::default(), clock.clone());
        let pool = Pool::over(SimJobs::new(clock, fs, 1));
        let _held = pool.offload("sim.depth", None, || ()).unwrap();
        let spill = pool.offload("sim.depth", None, || ()).err().unwrap();
        assert_eq!((spill.depth, spill.cap), (1, 1));
        assert_eq!(spilled_total("sim.depth"), 1);
    }

    #[test_id(t7wd3ac)]
    #[test]
    fn a_job_blocked_in_the_sim_disk_completes_once_the_disk_releases_it() {
        let (_, fs, jobs, pool) = sim(1);
        fs.hold_next(1);
        let submitted = {
            let (fs, pool) = (fs.clone(), pool.clone());
            std::thread::spawn(move || append_job(&pool, &fs, "held"))
        };
        fs.wait_blocked(1);
        assert_eq!(jobs.pending(), 0, "the job has not finished");
        fs.release();
        let mut done = submitted.join().unwrap();
        jobs.run_all();
        assert!(done.try_take().unwrap().is_ok());
    }
}
