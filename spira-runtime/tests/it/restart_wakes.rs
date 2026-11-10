//! A task's wake survives a runtime restart: after a process dies and a new one boots on the
//! same journal, batcher.local and gate.worker observe again once the clock passes their cadence.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::{Clock, VirtualClock};
use runtime::{EffectSpec, Engine, MemJournal};
use spira_runtime::tasks::a::batcher_local::{BatcherLocal, CADENCE_MS as BATCHER_CADENCE, OBSERVE as BATCHER_OBSERVE};
use spira_runtime::tasks::a::gate_worker::{GateWorker, CADENCE_MS as GATE_CADENCE, OBSERVE as GATE_OBSERVE};
use spira_runtime::{start_tasks, tasks};

#[derive(Default)]
struct Seen(Mutex<Vec<(String, u64)>>);

struct Recording {
    seen: Arc<Seen>,
    clock: VirtualClock,
}

impl EffectRunner for Recording {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.seen.0.lock().unwrap().push((spec.name.clone(), self.clock.now_ms()));
        Ok("{}".into())
    }
}

fn boot(j: Arc<dyn Journal>, clock: &VirtualClock, seen: &Arc<Seen>) -> VirtualDriver {
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let regs = vec![BatcherLocal::new(BTreeMap::new()).registration(), GateWorker::new(2, BTreeMap::new()).registration()];
    let mut engine = Engine::open(j, c, regs, 1).unwrap();
    start_tasks(&mut engine);
    VirtualDriver::new(engine, clock.clone(), Arc::new(Recording { seen: seen.clone(), clock: clock.clone() }))
}

fn observes(seen: &Seen, name: &str) -> usize {
    seen.0.lock().unwrap().iter().filter(|(n, _)| n == name).count()
}

#[test]
fn batcher_and_gate_worker_observe_on_cadence_after_a_restart() {
    let streams: Vec<_> = tasks::boots().into_iter().map(|(_, stream, _)| stream).collect();
    assert!(streams.contains(&"task.batcher.local") && streams.contains(&"task.gate.worker"), "both tasks are booted: {streams:?}");
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(1_000));
    let seen = Arc::new(Seen::default());

    let mut first = boot(j.clone(), &clock, &seen);
    first.run_until(1_000 + 10_000).unwrap();
    drop(first);
    let before = (observes(&seen, BATCHER_OBSERVE), observes(&seen, GATE_OBSERVE));
    assert!(before.0 >= 1 && before.1 >= 1, "each observes at boot: {before:?}");

    let mut second = boot(j, &clock, &seen);
    second.run_until(clock.now_ms() + BATCHER_CADENCE.max(GATE_CADENCE) * 3).unwrap();
    assert!(observes(&seen, BATCHER_OBSERVE) >= before.0 + 3, "batcher.local observes each cadence after the restart: {:?}", seen.0.lock().unwrap());
    assert!(observes(&seen, GATE_OBSERVE) >= before.1 + 3, "gate.worker observes each cadence after the restart");
}

/// The downtime outlasts several cadences; boot must fire the overdue wake once, not wait a full cadence.
#[test]
fn a_wake_that_fell_due_while_the_runtime_was_down_fires_at_the_next_boot() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(1_000));
    let seen = Arc::new(Seen::default());

    let mut first = boot(j.clone(), &clock, &seen);
    first.run_until(1_000 + 10_000).unwrap();
    drop(first);
    let before = (observes(&seen, BATCHER_OBSERVE), observes(&seen, GATE_OBSERVE));

    clock.advance_to(clock.now_ms() + BATCHER_CADENCE.max(GATE_CADENCE) * 5);
    let mut second = boot(j, &clock, &seen);
    second.run_until(clock.now_ms() + BATCHER_CADENCE.max(GATE_CADENCE)).unwrap();
    assert!(observes(&seen, BATCHER_OBSERVE) > before.0, "batcher.local observes at the first boot after the gap");
    assert!(observes(&seen, GATE_OBSERVE) > before.1, "gate.worker observes at the first boot after the gap");
}
