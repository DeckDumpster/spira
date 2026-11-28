//! The one bounded queue and writer thread behind every queued push to the Collector.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// What a queue holds and sends at once; `None` is no bound of that kind.
#[derive(Clone, Copy)]
pub struct Bounds {
    /// The most items waiting for the writer.
    pub items: Option<usize>,
    /// The most encoded bytes waiting for the writer.
    pub bytes: Option<usize>,
    /// The most items in one send.
    pub batch: Option<usize>,
    /// The least time between sends, waived while a drain is waiting.
    pub tick: Option<Duration>,
}

/// Items a queue lost.
#[derive(Default)]
pub struct Lost {
    refused: AtomicU64,
    failed: AtomicU64,
}

impl Lost {
    pub const fn new() -> Lost {
        Lost { refused: AtomicU64::new(0), failed: AtomicU64::new(0) }
    }

    /// Items the queue could not take: full, closed, or the writer never started.
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    /// Items of a batch its send reported not delivered.
    pub fn failed(&self) -> u64 {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn total(&self) -> u64 {
        self.refused() + self.failed()
    }
}

struct State<T> {
    /// Each item with its encoded size.
    items: VecDeque<(T, usize)>,
    bytes: usize,
    busy: bool,
    flushers: usize,
    closed: bool,
    /// Every item emitted, taken or refused.
    offered: u64,
    delivered: u64,
}

struct Shared<T> {
    bounds: Bounds,
    state: Mutex<State<T>>,
    wake: Condvar,
    done: Condvar,
    lost: Lost,
    process: Option<&'static Lost>,
}

impl<T> Shared<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn count(&self, kind: impl Fn(&Lost) -> &AtomicU64, n: u64) {
        for lost in std::iter::once(&self.lost).chain(self.process) {
            kind(lost).fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Waits until nothing is queued or in flight, or `bound` passes.
    fn drain<'a>(&self, mut s: MutexGuard<'a, State<T>>, bound: Duration) -> MutexGuard<'a, State<T>> {
        s.flushers += 1;
        self.wake.notify_one();
        let (mut s, _) = self.done.wait_timeout_while(s, bound, |s| !s.items.is_empty() || s.busy).unwrap_or_else(|e| e.into_inner());
        s.flushers -= 1;
        s
    }

    fn write(&self, send: impl Fn(Vec<T>) -> bool) {
        let mut s = self.lock();
        loop {
            while s.items.is_empty() {
                if s.closed {
                    return;
                }
                s = self.wake.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            let n = self.bounds.batch.map_or(s.items.len(), |most| most.clamp(1, s.items.len()));
            let mut sizes = 0;
            let batch = s.items.drain(..n).map(|(item, size)| { sizes += size; item }).collect();
            s.bytes -= sizes;
            s.busy = true;
            drop(s);
            let sent = send(batch);
            if !sent {
                self.count(|l| &l.failed, n as u64);
            }
            s = self.lock();
            s.busy = false;
            if sent {
                s.delivered += n as u64;
            }
            self.done.notify_all();
            if let Some(tick) = self.bounds.tick {
                s = self.wake.wait_timeout_while(s, tick, |s| s.flushers == 0 && !s.closed).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }
}

/// One bounded queue and the one thread that sends from it. Dropping it stops it taking items;
/// the thread still sends what is queued.
pub struct Push<T> {
    shared: Arc<Shared<T>>,
    started: bool,
}

impl<T: Send + 'static> Push<T> {
    /// `send` gets one batch and says whether the whole of it was delivered. Losses also add to
    /// `process`.
    pub fn new(thread: &str, bounds: Bounds, process: Option<&'static Lost>, send: impl Fn(Vec<T>) -> bool + Send + 'static) -> Push<T> {
        let state = State { items: VecDeque::new(), bytes: 0, busy: false, flushers: 0, closed: false, offered: 0, delivered: 0 };
        let shared = Arc::new(Shared { bounds, state: Mutex::new(state), wake: Condvar::new(), done: Condvar::new(), lost: Lost::new(), process });
        let writer = shared.clone();
        let started = std::thread::Builder::new().name(thread.into()).spawn(move || writer.write(send)).is_ok();
        if !started {
            shared.lock().closed = true;
        }
        Push { shared, started }
    }
}

impl<T> Push<T> {
    pub fn started(&self) -> bool {
        self.started
    }

    /// Queues one item of `size` encoded bytes and never blocks on a send.
    pub fn emit(&self, item: T, size: usize) {
        let mut s = self.shared.lock();
        s.offered += 1;
        let bounds = self.shared.bounds;
        let full = bounds.items.is_some_and(|most| s.items.len() >= most) || bounds.bytes.is_some_and(|most| s.bytes + size > most);
        if s.closed || full {
            drop(s);
            return self.shared.count(|l| &l.refused, 1);
        }
        s.bytes += size;
        s.items.push_back((item, size));
        self.shared.wake.notify_one();
    }

    /// Returns once the queue is empty and no send is in flight, or `bound` passes.
    pub fn flush(&self, bound: Duration) {
        drop(self.shared.drain(self.shared.lock(), bound));
    }

    /// Stops taking items, drains within `bound`, and returns how many emitted items, queued or
    /// refused, were not delivered.
    pub fn close(&self, bound: Duration) -> u64 {
        let mut s = self.shared.lock();
        s.closed = true;
        let s = self.shared.drain(s, bound);
        s.offered - s.delivered
    }

    /// This queue's own losses.
    pub fn lost(&self) -> &Lost {
        &self.shared.lost
    }

    /// Items and bytes waiting for the writer.
    #[cfg(test)]
    pub fn queued(&self) -> (usize, usize) {
        let s = self.shared.lock();
        (s.items.len(), s.bytes)
    }
}

impl<T> Drop for Push<T> {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A cap on a wait for the closure's own signal.
    const CAP: Duration = Duration::from_secs(30);
    const ANY: Bounds = Bounds { items: None, bytes: None, batch: None, tick: None };

    /// A send closure that reports each batch as it arrives and answers only when told to.
    struct Script {
        arrived: Receiver<Vec<u32>>,
        answer: Sender<bool>,
    }

    fn scripted(bounds: Bounds, shared: Option<&'static Lost>) -> (Push<u32>, Script) {
        let (arrived_tx, arrived) = channel();
        let (answer, answers) = channel::<bool>();
        let answers = Mutex::new(answers);
        let push = Push::new("push-test", bounds, shared, move |batch: Vec<u32>| {
            let _ = arrived_tx.send(batch);
            answers.lock().unwrap().recv().unwrap_or(false)
        });
        (push, Script { arrived, answer })
    }

    impl Script {
        fn batch(&self) -> Vec<u32> {
            self.arrived.recv_timeout(CAP).expect("a batch reaches the closure")
        }

        /// Queues item 0 and returns once the writer holds it, so later items stay queued.
        fn occupy(&self, push: &Push<u32>) {
            push.emit(0, 1);
            assert_eq!(self.batch(), [0]);
        }
    }

    #[test_id(th53m90)]
    #[test]
    fn a_byte_bound_refuses_the_item_that_would_pass_it() {
        let (push, script) = scripted(Bounds { bytes: Some(10), ..ANY }, None);
        script.occupy(&push);
        push.emit(1, 6);
        push.emit(2, 4);
        assert_eq!(push.lost().refused(), 0, "an item that reaches the bound is taken");
        push.emit(3, 1);
        assert_eq!((push.lost().refused(), push.queued()), (1, (2, 10)));
        script.answer.send(true).unwrap();
        assert_eq!(script.batch(), [1, 2]);
    }

    #[test_id(t6gr7pi)]
    #[test]
    fn an_item_bound_refuses_the_item_that_would_pass_it() {
        let (push, script) = scripted(Bounds { items: Some(2), ..ANY }, None);
        script.occupy(&push);
        for n in 1..=3 {
            push.emit(n, 1);
        }
        assert_eq!((push.lost().refused(), push.queued()), (1, (2, 2)));
        script.answer.send(true).unwrap();
        assert_eq!(script.batch(), [1, 2]);
    }

    #[test_id(tnz4cll)]
    #[test]
    fn items_past_the_batch_bound_leave_in_the_next_batch() {
        let (push, script) = scripted(Bounds { batch: Some(512), ..ANY }, None);
        script.occupy(&push);
        for n in 1..=700 {
            push.emit(n, 1);
        }
        for _ in 0..3 {
            script.answer.send(true).unwrap();
        }
        let (first, second) = (script.batch(), script.batch());
        assert_eq!((first.len(), second.len()), (512, 188));
        assert_eq!(first.into_iter().chain(second).collect::<Vec<_>>(), (1..=700).collect::<Vec<_>>());
        assert_eq!(push.close(CAP), 0);
    }

    #[test_id(tm34xsr)]
    #[test]
    fn a_failed_send_counts_every_item_of_its_batch_once_and_is_not_retried() {
        let (push, script) = scripted(ANY, None);
        script.occupy(&push);
        for n in 1..=3 {
            push.emit(n, 1);
        }
        script.answer.send(true).unwrap();
        assert_eq!(script.batch(), [1, 2, 3]);
        script.answer.send(false).unwrap();
        assert_eq!(push.close(CAP), 3);
        assert_eq!((push.lost().failed(), push.lost().refused()), (3, 0));
        drop(push);
        assert!(script.arrived.recv_timeout(CAP).is_err_and(|e| e == std::sync::mpsc::RecvTimeoutError::Disconnected), "the writer ends without sending again");
    }

    #[test_id(tfijxy1)]
    #[test]
    fn close_returns_within_its_bound_while_a_send_never_returns() {
        let (push, script) = scripted(Bounds { items: Some(2), ..ANY }, None);
        script.occupy(&push);
        for n in 1..=3 {
            push.emit(n, 1);
        }
        assert_eq!(push.close(Duration::from_millis(10)), 4, "one in flight, two queued, one refused");
        push.emit(4, 1);
        assert_eq!(push.lost().refused(), 2, "a closed queue refuses");
    }

    #[test_id(t9jzbnq)]
    #[test]
    fn a_loss_counts_on_the_instance_and_on_the_process_wide_counter() {
        static SHARED: Lost = Lost::new();
        let (a, a_script) = scripted(Bounds { items: Some(1), ..ANY }, Some(&SHARED));
        let (b, b_script) = scripted(ANY, Some(&SHARED));
        a_script.occupy(&a);
        a.emit(1, 1);
        a.emit(2, 1);
        b_script.occupy(&b);
        b_script.answer.send(false).unwrap();
        b.flush(CAP);
        assert_eq!((a.lost().refused(), a.lost().failed()), (1, 0));
        assert_eq!((b.lost().refused(), b.lost().failed()), (0, 1));
        assert_eq!((SHARED.refused(), SHARED.failed(), SHARED.total()), (1, 1, 2));
    }
}
