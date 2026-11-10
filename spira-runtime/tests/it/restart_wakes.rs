//! After a boot built the way `main.rs` builds it (`open_engine` over every registration, then
//! `start_tasks`), batcher.local and gate.worker keep a tick armed and observe on every cadence.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::{Boot, Journal, Poison, Turn};
use runtime::ports::{Clock, VirtualClock};
use runtime::{Command, StreamKey, Ctx, EffectSpec, Event, EventKindTag, FaultyJournal, Handler, MemJournal, NullMarker, NullSink, Projection, Registration};
use spira_runtime::settings::fixture;
use spira_runtime::tasks::a::batcher_local::{CADENCE_MS as BATCHER_CADENCE, OBSERVE as BATCHER_OBSERVE, STREAM as BATCHER_STREAM, TICK as BATCHER_TICK};
use spira_runtime::tasks::a::gate_worker::{CADENCE_MS as GATE_CADENCE, OBSERVE as GATE_OBSERVE};
use spira_runtime::{open_engine, start_tasks};

const BATCHER: &str = "batcher.local";
const GATE: &str = "gate.worker";

#[derive(Default)]
struct Seen(Mutex<Vec<String>>);

struct Recording(Arc<Seen>);

impl EffectRunner for Recording {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.0 .0.lock().unwrap().push(spec.name.clone());
        Ok("{}".into())
    }
}

/// Panics once on the batcher's own tick: another handler failing on the stream.
struct PanicsOnce {
    tags: Vec<EventKindTag>,
    spent: AtomicBool,
}

impl Handler for PanicsOnce {
    fn interests(&self) -> &[EventKindTag] {
        &self.tags
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 == BATCHER_STREAM && !self.spent.swap(true, Ordering::SeqCst) {
            panic!("planted handler failure on {}", ev.stream.0);
        }
        vec![]
    }
}

/// A store whose poison rows the operator can clear.
struct Lifting {
    inner: Arc<dyn Journal>,
    lifted: AtomicBool,
}

impl Journal for Lifting {
    fn record(&self, ev: &Event) {
        self.inner.record(ev)
    }
    fn boot(&self) -> runtime::Result<Boot> {
        self.inner.boot()
    }
    fn commit(&self, turn: &Turn) -> runtime::Result<()> {
        self.inner.commit(turn)
    }
    fn poison(&self, p: &Poison) -> runtime::Result<()> {
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> runtime::Result<Option<Poison>> {
        Ok(self.inner.poisoned(stream)?.filter(|_| !self.lifted.load(Ordering::SeqCst)))
    }
}

fn boot(journal: Arc<dyn Journal>, clock: &VirtualClock, seen: &Arc<Seen>, extra: Vec<Registration>) -> VirtualDriver {
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut engine = open_engine(&fixture(), journal, c, Arc::new(NullMarker), Arc::new(NullSink), extra).unwrap();
    start_tasks(&mut engine);
    VirtualDriver::new(engine, clock.clone(), Arc::new(Recording(seen.clone())))
}

fn observes(seen: &Seen, name: &str) -> usize {
    seen.0.lock().unwrap().iter().filter(|n| *n == name).count()
}

fn armed(d: &VirtualDriver, task: &str) -> Option<u64> {
    d.engine().status().tasks.into_iter().find(|t| t.name == task).unwrap_or_else(|| panic!("{task} is registered")).next_due_at
}

fn run_for(d: &mut VirtualDriver, clock: &VirtualClock, ms: u64) {
    d.run_until(clock.now_ms() + ms).unwrap();
}

/// Past the boot stagger of every task, so each has had its boot tick.
fn settle_boot(d: &mut VirtualDriver, clock: &VirtualClock) {
    run_for(d, clock, 60_000);
}

#[test]
fn batcher_and_gate_worker_stay_armed_and_observe_every_cadence_after_a_boot_of_all_tasks() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(1_000));
    let seen = Arc::new(Seen::default());
    let mut d = boot(j, &clock, &seen, vec![]);
    settle_boot(&mut d, &clock);
    for task in [BATCHER, GATE] {
        assert!(armed(&d, task).is_some(), "{task} is armed after its boot tick");
    }
    let before = (observes(&seen, BATCHER_OBSERVE), observes(&seen, GATE_OBSERVE));
    assert!(before.0 >= 1 && before.1 >= 1, "each observes at boot: {before:?}");
    for _ in 0..3 {
        run_for(&mut d, &clock, BATCHER_CADENCE.max(GATE_CADENCE));
        for task in [BATCHER, GATE] {
            assert!(armed(&d, task).is_some(), "{task} is armed after each later tick");
        }
    }
    assert!(observes(&seen, BATCHER_OBSERVE) >= before.0 + 3, "batcher.local observes each cadence");
    assert!(observes(&seen, GATE_OBSERVE) >= before.1 + 3, "gate.worker observes each cadence");
}

/// A handler fails on the batcher's stream while the store refuses the poison row: the stream is
/// skipped in memory with no row, and the tick that would have followed is consumed.
#[test]
fn a_tick_swallowed_by_a_stream_quarantine_that_could_not_be_stored_is_armed_again() {
    let (faulty, clock) = (FaultyJournal::new(MemJournal::new()), VirtualClock::new(1_000));
    let seen = Arc::new(Seen::default());
    let extra = Registration::new("planted", PanicsOnce { tags: vec![BATCHER_TICK], spent: AtomicBool::new(false) });
    let mut d = boot(faulty.clone(), &clock, &seen, vec![extra]);
    faulty.fail_next("poison");
    // The failed write comes back as an error, as it does to the host, which goes on.
    for _ in 0..40 {
        let _ = d.run_until(clock.now_ms() + 5_000);
    }
    let after_fault = observes(&seen, BATCHER_OBSERVE);
    let gate_after_fault = observes(&seen, GATE_OBSERVE);
    assert!(armed(&d, BATCHER).is_some(), "batcher.local keeps a tick armed");
    for _ in 0..3 {
        run_for(&mut d, &clock, BATCHER_CADENCE.max(GATE_CADENCE));
        assert!(armed(&d, BATCHER).is_some(), "armed after each later tick");
        assert!(armed(&d, GATE).is_some(), "gate.worker is untouched by the batcher's fault");
    }
    assert!(observes(&seen, GATE_OBSERVE) >= gate_after_fault + 3, "gate.worker observes each cadence");
    assert!(observes(&seen, BATCHER_OBSERVE) >= after_fault + 3, "batcher.local observes each cadence after the fault");
}

/// The stream is quarantined with a row; ticks that fall on it are not heard, and once the
/// operator clears the row the stream ticks again without a restart.
#[test]
fn a_quarantined_batcher_stream_ticks_again_once_its_row_is_cleared() {
    let (store, clock) = (Arc::new(Lifting { inner: MemJournal::new(), lifted: AtomicBool::new(false) }), VirtualClock::new(1_000));
    let seen = Arc::new(Seen::default());
    let extra = Registration::new("planted", PanicsOnce { tags: vec![BATCHER_TICK], spent: AtomicBool::new(false) });
    let mut d = boot(store.clone(), &clock, &seen, vec![extra]);
    run_for(&mut d, &clock, 60_000);
    assert!(store.inner.poisoned(&StreamKey(BATCHER_STREAM.into())).unwrap().is_some(), "the quarantine left its row in the store");
    for _ in 0..3 {
        run_for(&mut d, &clock, 30_000);
        assert!(armed(&d, BATCHER).is_some(), "batcher.local stays armed while its row stands");
    }
    store.lifted.store(true, Ordering::SeqCst);
    for _ in 0..40 {
        run_for(&mut d, &clock, 30_000);
    }
    let resumed = observes(&seen, BATCHER_OBSERVE);
    let gate_resumed = observes(&seen, GATE_OBSERVE);
    assert!(armed(&d, BATCHER).is_some(), "batcher.local keeps a tick armed once the row is cleared");
    for _ in 0..3 {
        run_for(&mut d, &clock, BATCHER_CADENCE.max(GATE_CADENCE));
        assert!(armed(&d, GATE).is_some(), "gate.worker stays armed beside the quarantined stream");
    }
    assert!(observes(&seen, GATE_OBSERVE) >= gate_resumed + 3, "gate.worker observes each cadence");
    assert!(observes(&seen, BATCHER_OBSERVE) >= resumed + 3, "batcher.local observes each cadence once the row is cleared");
}

/// A restart onto a stream whose poison row stands: the boot tick lands on it, is parked, and
/// the stream ticks again once the row is cleared.
#[test]
fn a_restart_onto_a_standing_poison_row_keeps_the_batcher_armed_and_resumes_when_it_is_cleared() {
    let (store, clock) = (Arc::new(Lifting { inner: MemJournal::new(), lifted: AtomicBool::new(false) }), VirtualClock::new(1_000));
    store.inner.poison(&Poison { stream: StreamKey(BATCHER_STREAM.into()), event: runtime::EventId("earlier".into()), handler: "planted".into(), message: "left by the run before the restart".into() }).unwrap();
    let seen = Arc::new(Seen::default());
    let mut d = boot(store.clone(), &clock, &seen, vec![]);
    for _ in 0..3 {
        run_for(&mut d, &clock, 60_000);
        assert!(armed(&d, BATCHER).is_some(), "batcher.local stays armed while the inherited row stands");
    }
    assert_eq!(observes(&seen, BATCHER_OBSERVE), 0, "a stream under a standing row is not observed");
    store.lifted.store(true, Ordering::SeqCst);
    for _ in 0..40 {
        run_for(&mut d, &clock, 30_000);
    }
    let resumed = observes(&seen, BATCHER_OBSERVE);
    for _ in 0..3 {
        run_for(&mut d, &clock, BATCHER_CADENCE);
        assert!(armed(&d, BATCHER).is_some(), "armed after each tick once the row is cleared");
    }
    assert!(observes(&seen, BATCHER_OBSERVE) >= resumed + 3, "batcher.local observes each cadence once the row is cleared");
}
