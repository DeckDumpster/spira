//! `Engine::open` rides out a journal that cannot tell for a bounded window instead of failing
//! the boot: the store behind it may have just restarted.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::engine::BootRetry;
use runtime::journal::{Boot, Journal, MemJournal, Poison, Turn};
use runtime::ports::VirtualClock;
use runtime::*;

struct Unsure {
    inner: Arc<MemJournal>,
    answers_after: usize,
    asked: AtomicUsize,
    refuse: bool,
}

impl Unsure {
    fn new(answers_after: usize) -> Arc<Self> {
        Arc::new(Unsure { inner: MemJournal::new(), answers_after, asked: AtomicUsize::new(0), refuse: false })
    }
}

impl Journal for Unsure {
    fn record(&self, ev: &Event) {
        self.inner.record(ev)
    }
    fn boot(&self) -> Result<Boot> {
        if self.asked.fetch_add(1, Ordering::SeqCst) < self.answers_after {
            return Err(if self.refuse {
                RuntimeError::refused("journal: no")
            } else {
                RuntimeError::cannot_tell("cannot tell: spira-lc took the journal request but its reply was lost (no reply)")
            });
        }
        self.inner.boot()
    }
    fn commit(&self, turn: &Turn) -> Result<()> {
        self.inner.commit(turn)
    }
    fn poison(&self, p: &Poison) -> Result<()> {
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.inner.poisoned(stream)
    }
}

fn retry(slept: &Arc<Mutex<Vec<u64>>>, bound_ms: u64) -> BootRetry {
    let slept = slept.clone();
    BootRetry { first_ms: 100, ceiling_ms: u64::MAX, bound_ms, store: "spira-lc".into(), sleep: Arc::new(move |ms| slept.lock().unwrap().push(ms)) }
}

#[test]
fn a_journal_that_cannot_tell_three_times_then_answers_opens_the_engine() {
    let slept = Arc::new(Mutex::new(vec![]));
    let j = Unsure::new(3);
    Engine::open_with(j, Arc::new(VirtualClock::new(0)), vec![], 0, retry(&slept, 60_000)).unwrap();
    assert_eq!(*slept.lock().unwrap(), vec![100, 200, 400], "exponential backoff between the asks");
}

#[test]
fn a_journal_that_never_answers_fails_the_open_after_the_bound_naming_the_store() {
    let slept = Arc::new(Mutex::new(vec![]));
    let j = Unsure::new(usize::MAX);
    let err = Engine::open_with(j, Arc::new(VirtualClock::new(0)), vec![], 0, retry(&slept, 1_000)).err().expect("must fail");
    assert!(err.0.contains("spira-lc"), "{err}");
    assert!(err.0.contains("cannot tell"), "{err}");
    assert!(slept.lock().unwrap().iter().sum::<u64>() <= 1_000, "never waits past the bound");
}

#[test]
fn a_refusal_is_not_retried() {
    let slept = Arc::new(Mutex::new(vec![]));
    let j = Arc::new(Unsure { inner: MemJournal::new(), answers_after: 1, asked: AtomicUsize::new(0), refuse: true });
    assert!(Engine::open_with(j, Arc::new(VirtualClock::new(0)), vec![], 0, retry(&slept, 60_000)).is_err());
    assert!(slept.lock().unwrap().is_empty());
}

#[test]
fn open_without_a_retry_policy_asks_once() {
    let j = Unsure::new(1);
    assert!(Engine::open(j.clone(), Arc::new(VirtualClock::new(0)), vec![], 0).is_err());
    assert_eq!(j.asked.load(Ordering::SeqCst), 1);
}

#[test]
fn the_waits_spend_the_whole_bound_with_a_shortened_last_wait() {
    let slept = Arc::new(Mutex::new(Vec::new()));
    let r = Engine::open_with(Unsure::new(usize::MAX), Arc::new(VirtualClock::new(0)), vec![], 0, retry(&slept, 1_000));
    assert!(r.is_err());
    let v = slept.lock().unwrap().clone();
    assert_eq!(v.iter().sum::<u64>(), 1_000, "{v:?}");
}

#[test]
fn the_wait_stops_doubling_at_the_ceiling_so_a_store_that_arrives_late_is_found_soon() {
    let slept = Arc::new(Mutex::new(Vec::new()));
    let mut r = retry(&slept, 60_000);
    r.ceiling_ms = 300;
    Engine::open_with(Unsure::new(5), Arc::new(VirtualClock::new(0)), vec![], 0, r).unwrap();
    assert_eq!(*slept.lock().unwrap(), vec![100, 200, 300, 300, 300]);
}
