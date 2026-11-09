//! `Engine::open` rides out a journal that cannot tell for a bounded window instead of failing
//! the boot: the store behind it may have just restarted.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::engine::BootRetry;
use runtime::journal::{Journal, MemJournal, Offset, Poison, ScheduleRow};
use runtime::ports::VirtualClock;
use runtime::*;

struct Unsure {
    inner: Arc<MemJournal>,
    answers_after: usize,
    op: &'static str,
    asked: AtomicUsize,
    refuse: bool,
}

impl Unsure {
    fn new(answers_after: usize) -> Arc<Self> {
        Self::on("cursor", answers_after)
    }
    fn on(op: &'static str, answers_after: usize) -> Arc<Self> {
        Arc::new(Unsure { inner: MemJournal::new(), answers_after, op, asked: AtomicUsize::new(0), refuse: false })
    }
    fn gate(&self, op: &str) -> Result<()> {
        if op != self.op {
            return Ok(());
        }
        if self.asked.fetch_add(1, Ordering::SeqCst) < self.answers_after {
            return Err(if self.refuse {
                RuntimeError::refused("journal: no")
            } else {
                RuntimeError::cannot_tell("cannot tell: spira-lc took the journal request but its reply was lost (no reply)")
            });
        }
        Ok(())
    }
}

impl Journal for Unsure {
    fn append(&self, ev: &Event) -> Result<Offset> {
        self.inner.append(ev)
    }
    fn read_from(&self, from: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.inner.read_from(from, limit)
    }
    fn applied(&self, handler: &str, id: &EventId) -> Result<bool> {
        self.gate("applied")?;
        self.inner.applied(handler, id)
    }
    fn mark_applied(&self, handler: &str, id: &EventId) -> Result<()> {
        self.inner.mark_applied(handler, id)
    }
    fn cursor(&self) -> Result<Offset> {
        self.gate("cursor")?;
        self.inner.cursor()
    }
    fn read_stream(&self, stream: &StreamKey, below: Offset, limit: usize) -> Result<Vec<(Offset, Event)>> {
        self.gate("read_stream")?;
        self.inner.read_stream(stream, below, limit)
    }
    fn end(&self) -> Result<Offset> {
        self.gate("end")?;
        self.inner.end()
    }
    fn unresolved_effects(&self) -> Result<Vec<(Offset, Event)>> {
        self.gate("unresolved_effects")?;
        self.inner.unresolved_effects()
    }
    fn set_cursor(&self, next: Offset) -> Result<()> {
        self.inner.set_cursor(next)
    }
    fn schedule(&self, row: &ScheduleRow) -> Result<()> {
        self.inner.schedule(row)
    }
    fn pending_schedules(&self) -> Result<Vec<ScheduleRow>> {
        self.gate("pending_schedules")?;
        self.inner.pending_schedules()
    }
    fn mark_fired(&self, row_id: &str) -> Result<()> {
        self.inner.mark_fired(row_id)
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
    BootRetry { first_ms: 100, bound_ms, store: "spira-lc".into(), sleep: Arc::new(move |ms| slept.lock().unwrap().push(ms)) }
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
    let j = Arc::new(Unsure { inner: MemJournal::new(), answers_after: 1, op: "cursor", asked: AtomicUsize::new(0), refuse: true });
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
fn every_boot_read_rides_out_a_cannot_tell() {
    for op in ["cursor", "read_stream", "unresolved_effects", "end", "pending_schedules"] {
        let slept = Arc::new(Mutex::new(vec![]));
        Engine::open_with(Unsure::on(op, 2), Arc::new(VirtualClock::new(0)), vec![], 0, retry(&slept, 60_000)).unwrap_or_else(|e| panic!("{op}: {e}"));
        assert_eq!(*slept.lock().unwrap(), vec![100, 200], "{op}");
    }
}

#[test]
fn a_held_release_check_rides_out_a_cannot_tell() {
    use runtime::{EFFECT_HELD, HALT_STREAM};
    let slept = Arc::new(Mutex::new(vec![]));
    let j = Unsure::on("applied", 2);
    let held = Event { id: EventId("h".into()), stream: StreamKey(HALT_STREAM.into()), kind: EventKind::new(EFFECT_HELD, ""), at: 1, cause: None, correlation: CorrelationId("c".into()) };
    j.inner.append(&held).unwrap();
    j.inner.set_cursor(1).unwrap();
    Engine::open_with(j, Arc::new(VirtualClock::new(0)), vec![], 0, retry(&slept, 60_000)).unwrap();
    assert_eq!(*slept.lock().unwrap(), vec![100, 200]);
}

#[test]
fn the_waits_spend_the_whole_bound_with_a_shortened_last_wait() {
    let slept = Arc::new(Mutex::new(Vec::new()));
    let r = Engine::open_with(Unsure::new(usize::MAX), Arc::new(VirtualClock::new(0)), vec![], 0, retry(&slept, 1_000));
    assert!(r.is_err());
    let v = slept.lock().unwrap().clone();
    assert_eq!(v.iter().sum::<u64>(), 1_000, "{v:?}");
}
