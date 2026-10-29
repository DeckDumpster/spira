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
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, OwnedSemaphorePermit};

pub const WORKERS: usize = 8;
pub const QUEUE_DEPTH: usize = 32;

type Job = Box<dyn FnOnce() + Send>;

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

pub struct Pool {
    tx: SyncSender<Job>,
    queued: Arc<AtomicUsize>,
    cap: usize,
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
        let (tx, rx) = sync_channel::<Job>(cap);
        let rx: Arc<Mutex<Receiver<Job>>> = Arc::new(Mutex::new(rx));
        let queued = Arc::new(AtomicUsize::new(0));
        for i in 0..workers.max(1) {
            let (rx, queued) = (rx.clone(), queued.clone());
            std::thread::Builder::new().name(format!("offload-{i}")).spawn(move || loop {
                let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                let Ok(job) = job else { return };
                queued.fetch_sub(1, Ordering::SeqCst);
                job();
            })
            .expect("an offload worker thread starts");
        }
        Arc::new(Pool { tx, queued, cap })
    }

    /// The process-wide pool, sized once.
    pub fn global() -> Arc<Pool> {
        static GLOBAL: OnceLock<Arc<Pool>> = OnceLock::new();
        GLOBAL.get_or_init(|| Pool::new(WORKERS, QUEUE_DEPTH)).clone()
    }

    /// `permit` is released when `f` returns, not when the caller stops waiting.
    pub fn offload<T: Send + 'static>(&self, what: &'static str, permit: Option<OwnedSemaphorePermit>, f: impl FnOnce() -> T + Send + 'static) -> std::result::Result<Offloaded<T>, Spill> {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move || {
            let _permit = permit;
            let _ = tx.send(catch_unwind(AssertUnwindSafe(f)).map_err(panic_message));
        });
        let depth = self.queued.fetch_add(1, Ordering::SeqCst);
        match self.tx.try_send(job) {
            Ok(()) => Ok(Offloaded { what, rx }),
            Err(e) => {
                self.queued.fetch_sub(1, Ordering::SeqCst);
                if matches!(e, TrySendError::Disconnected(_)) {
                    return Err(Spill { what, depth: self.cap, cap: self.cap });
                }
                *SPILLED.lock().unwrap_or_else(|e| e.into_inner()).entry(what).or_insert(0) += 1;
                Err(Spill { what, depth: depth.min(self.cap), cap: self.cap })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_full_queue_refuses_and_the_permit_outlives_the_wait() {
        let pool = Pool::new(1, 1);
        let gate = Arc::new(std::sync::Barrier::new(2));
        let (g, (started_tx, started_rx)) = (gate.clone(), std::sync::mpsc::channel());
        let running = pool.offload("t", None, move || {
            started_tx.send(()).unwrap();
            g.wait()
        })
        .unwrap();
        started_rx.recv().unwrap();
        let queued = pool.offload("t", None, || 7).unwrap();
        let before = spilled_total("t");
        assert!(matches!(pool.offload("t", None, || 8), Err(Spill { cap: 1, .. })), "the third is refused, not blocked on");
        assert_eq!(spilled_total("t"), before + 1);
        gate.wait();
        running.await.unwrap();
        assert_eq!(queued.await.unwrap(), 7);
    }

    #[tokio::test]
    async fn a_panic_comes_back_as_an_error_and_the_worker_lives() {
        let pool = Pool::new(1, 2);
        let e = pool.offload::<()>("boom", None, || panic!("x")).unwrap().await.unwrap_err();
        assert_eq!((e.what, e.why.as_str()), ("boom", "x"));
        assert_eq!(pool.offload("t", None, || 1).unwrap().await.unwrap(), 1);
    }

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
}
