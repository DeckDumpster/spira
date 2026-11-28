//! A virtual idle hour of every registration, booted the way `main.rs` boots it, with no store
//! change: what the scheduler asks of its journal, and which tasks run.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::changes::ScriptedChanges;
use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::{Boot, Journal, Poison, Turn};
use runtime::ports::{Clock, VirtualClock};
use runtime::{EffectSpec, Event, EventId, MemJournal, NullMarker, NullSink, StoreChanged, StreamKey};
use spira_runtime::settings::fixture;
use spira_runtime::tasks::{self, d::broker_exec, Reads};
use spira_runtime::{open_engine, start_tasks};

const HOUR_MS: u64 = 3_600_000;
/// Past the boot stagger of every task.
const SETTLE_MS: u64 = 600_000;
/// One poison read per five minutes.
const POISON_READS_AN_HOUR: usize = 12;

#[derive(Default)]
struct Asked(Mutex<Vec<(String, String)>>);

struct Recording(Arc<Asked>);

impl EffectRunner for Recording {
    fn run(&self, _: &str, _: &EffectSpec) -> Result<String, String> {
        unreachable!("the driver names the task")
    }
    fn run_for_task(&self, task: &str, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.0 .0.lock().unwrap().push((task.into(), spec.name.clone()));
        Ok("{}".into())
    }
}

impl Asked {
    fn take(&self) -> Vec<(String, String)> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

struct Counting {
    inner: Arc<MemJournal>,
    poison_reads: AtomicUsize,
}

impl Counting {
    fn reads(&self) -> usize {
        self.poison_reads.load(Ordering::SeqCst)
    }
}

impl Journal for Counting {
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
        self.poison_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.poisoned(stream)
    }
    fn poisoned_all(&self) -> runtime::Result<Vec<Poison>> {
        self.poison_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.poisoned_all()
    }
}

struct World {
    d: VirtualDriver,
    clock: VirtualClock,
    journal: Arc<Counting>,
    asked: Arc<Asked>,
}

impl World {
    fn boot(poisoned: &[&str]) -> World {
        let s = fixture();
        let journal = Arc::new(Counting { inner: MemJournal::new(), poison_reads: Default::default() });
        for stream in poisoned {
            let p = Poison { stream: StreamKey((*stream).into()), event: EventId(format!("earlier/{stream}")), handler: "planted".into(), message: "left by the run before".into() };
            journal.inner.poison(&p).unwrap();
        }
        let clock = VirtualClock::new(1_000);
        let c: Arc<dyn Clock> = Arc::new(clock.clone());
        let mut engine = open_engine(&s, journal.clone(), c, Arc::new(NullMarker), Arc::new(NullSink), vec![], std::time::Duration::from_secs(10)).unwrap();
        start_tasks(&mut engine);
        let changes = Arc::new(ScriptedChanges::default());
        changes.set(1);
        let engine = engine.with_sweep(s.sweep_secs * 1000, changes);
        let asked = Arc::new(Asked::default());
        let d = VirtualDriver::new(engine, clock.clone(), Arc::new(Recording(asked.clone())));
        World { d, clock, journal, asked }
    }

    fn run_for(&mut self, ms: u64) {
        self.d.run_until(self.clock.now_ms() + ms).unwrap();
    }
}

fn store_only() -> BTreeSet<&'static str> {
    tasks::all().filter(|t| matches!(t.reads, Reads::Only(_))).map(|t| t.name).collect()
}

/// How many of `turns` carry rows of each task.
fn turns_by_task(turns: &[Turn]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for t in turns {
        let names = t.task_names();
        assert!(!names.is_empty(), "a journaled turn names no task: {t:?}");
        for name in names {
            *out.entry(name.to_string()).or_default() += 1;
        }
    }
    out
}

#[test_id(t5n8jt7)]
#[test]
fn an_idle_hour_reads_poison_at_most_twelve_times_and_runs_no_store_only_task() {
    let mut w = World::boot(&[]);
    w.run_for(SETTLE_MS);
    let only = store_only();
    let booted: BTreeSet<String> = w.asked.take().into_iter().map(|(task, _)| task).collect();
    assert!(!only.is_empty() && only.iter().all(|t| booted.contains(*t)), "each store-only task is seen to ask at boot, so its silence below means something: {only:?} of {booted:?}");
    let (reads, turns) = (w.journal.reads(), w.journal.inner.commits().len());
    assert!(reads <= POISON_READS_AN_HOUR, "the boot and settle read poison {reads} times");

    w.run_for(HOUR_MS);

    let hour_reads = w.journal.reads() - reads;
    assert!(hour_reads <= POISON_READS_AN_HOUR, "an idle hour read poison {hour_reads} times");
    let asked = w.asked.take();
    assert!(!asked.is_empty(), "the polled tasks run through the hour");
    let ran: Vec<&(String, String)> = asked.iter().filter(|(task, _)| only.contains(task.as_str())).collect();
    assert!(ran.is_empty(), "a store-only task ran with no store change: {ran:?}");
    let per_task = turns_by_task(&w.journal.inner.commits()[turns..]);
    println!("journaled turns per task in one idle hour (poison reads: boot and settle {reads}, hour {hour_reads}):");
    for (task, n) in &per_task {
        println!("{n:6} {task}");
    }
}

/// Every task stream quarantined: the cost must not grow with the number of them.
#[test_id(tdz7b42)]
#[test]
fn an_idle_hour_with_every_task_stream_quarantined_still_reads_poison_at_most_twelve_times() {
    let streams: Vec<&str> = tasks::all().map(|t| t.stream()).collect();
    assert!(streams.len() > POISON_READS_AN_HOUR);
    let mut w = World::boot(&streams);
    w.run_for(SETTLE_MS);
    let reads = w.journal.reads();

    w.run_for(HOUR_MS);

    let hour_reads = w.journal.reads() - reads;
    println!("poison reads with {} quarantined streams: boot and settle {reads}, hour {hour_reads}", streams.len());
    assert!(hour_reads >= 1, "a standing quarantine is rechecked");
    assert!(hour_reads <= POISON_READS_AN_HOUR, "an idle hour read poison {hour_reads} times for {} streams", streams.len());
    assert_eq!(w.asked.take(), vec![], "no quarantined stream runs");
}

#[test_id(ts0yav0)]
#[test]
fn a_work_request_change_in_an_idle_hour_wakes_broker_exec_exactly_once() {
    let mut w = World::boot(&[]);
    w.run_for(SETTLE_MS);
    w.run_for(HOUR_MS / 2);
    w.asked.take();

    let change = StoreChanged { rev: 2, keys: vec!["work-requests/wr-1".into()], reload: false };
    w.d.submit(change.submit()).unwrap();
    w.run_for(HOUR_MS / 2);

    let broker: Vec<String> = w.asked.take().into_iter().filter(|(task, _)| task == broker_exec::NAME).map(|(_, effect)| effect).collect();
    assert_eq!(broker, [broker_exec::OBSERVE]);
}
