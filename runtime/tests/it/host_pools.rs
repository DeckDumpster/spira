//! Effector classes on the wall-clock host: a class at its cap defers, records it, and runs the
//! effect later.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit, VirtualDriver};
use runtime::journal::Journal;
use runtime::offload::Pool;
use runtime::pools::{ClassPools, EffectClass, PoolCaps};
use runtime::ports::{VirtualClock, WallClock};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Executor, Handler, MemJournal, MemSink, Projection, Registration, Value, EFFECT_REQUESTED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use tokio::sync::mpsc;

struct Builds;
impl Handler for Builds {
    fn interests(&self) -> &[EventKindTag] {
        &["build", EFFECT_SUCCEEDED]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        match ev.kind.tag.as_str() {
            "build" => vec![Command::Effect(EffectSpec { name: "suite".into(), input: ev.kind.body.clone(), deadline_ms: 60_000, key: format!("suite/{}", ev.kind.body) })],
            _ => vec![Command::Emit(EventKind::new("built", &ev.kind.body))],
        }
    }
}

/// Runs `suite` as a container-class effect, holding each run until released.
#[derive(Default)]
struct Suites {
    ran: Mutex<BTreeMap<String, usize>>,
    gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl EffectRunner for Suites {
    fn class(&self, spec: &EffectSpec) -> EffectClass {
        if spec.name == "suite" { EffectClass::Container } else { EffectClass::Host }
    }
    fn run(&self, _key: &str, spec: &EffectSpec) -> Result<String, String> {
        *self.ran.lock().unwrap().entry(spec.input.clone()).or_default() += 1;
        if let Some(rx) = self.gate.lock().unwrap().as_ref() {
            let _ = rx.recv();
        }
        Ok(spec.input.clone())
    }
}

fn engine(j: Arc<dyn Journal>) -> Engine {
    Engine::open(j, Arc::new(WallClock), vec![Registration::new("builds", Builds)], 1).unwrap()
}

fn engine_on(j: Arc<dyn Journal>, clock: &VirtualClock) -> Engine {
    Engine::open(j, Arc::new(clock.clone()), vec![Registration::new("builds", Builds)], 1).unwrap()
}

fn deferred(sink: &MemSink) -> Vec<BTreeMap<String, Value>> {
    sink.records().into_iter().filter(|f| f.get("unit") == Some(&Value::from("effect.deferred"))).collect()
}

fn opts(sink: &Arc<MemSink>) -> ServeOpts {
    ServeOpts { sink: Some(sink.clone()), pool: Some(Pool::new(4, 16)), ..ServeOpts::default() }
}

async fn until(f: impl Fn() -> bool) {
    for _ in 0..3000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: a 30 s hang cap on a wait for a named state
    }
    panic!("timed out waiting for the host");
}

fn count(j: &dyn Journal, tag: &str) -> usize {
    runtime::conformance::trace(j).iter().filter(|t| t.2 == tag).count()
}

#[test_id(t9vl57p)]
#[tokio::test]
async fn a_full_class_defers_records_it_and_runs_the_effect_later() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let board = Arc::new(runtime::engine::StatusBoard::default());
    let mut e = engine(j.clone()).with_status_board(board.clone());
    for i in 0..3 {
        e.submit(&format!("b{i}"), &format!("s{i}"), EventKind::new("build", &format!("b{i}"))).unwrap();
    }
    let (release, gate) = std::sync::mpsc::channel();
    let runner = Arc::new(Suites { gate: Mutex::new(Some(gate)), ..Suites::default() });
    let sink = MemSink::new();
    let (tx, rx) = mpsc::channel::<Submit>(4);
    drop(tx);
    let serving = tokio::spawn(serve_with(e, Arc::new(WallClock), runner.clone(), PoolCaps { container: 1, ..PoolCaps::default() }, rx, opts(&sink)));
    until(|| !deferred(&sink).is_empty() && runner.ran.lock().unwrap().values().sum::<usize>() == 1).await;
    let d = deferred(&sink);
    assert!(!d.is_empty(), "a container effect over the cap must be deferred and recorded");
    assert_eq!(d[0].get("class"), Some(&Value::from("container")));
    assert!(d[0].contains_key("task"), "the record names the task");
    assert_eq!(d[0].get("waits_for"), Some(&Value::from("container.slot")), "the record names what it waits for");
    for _ in 0..3 {
        let _ = release.send(());
    }
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    assert_eq!(count(&*j, "built"), 3, "every deferred effect runs: nothing is dropped");
    assert!(runner.ran.lock().unwrap().values().all(|n| *n == 1), "each effect runs once");
    let status = board.get().expect("the engine published its status");
    let container = status.classes.iter().find(|c| c.class == "container").unwrap_or_else(|| panic!("status names the runner's class: {:?}", status.classes));
    assert!(container.refusals >= 1, "status counts the class's refusals: {container:?}");
    assert!(status.classes.iter().all(|c| c.class != "host" || c.refusals == 0), "{:?}", status.classes);
}

#[test_id(tcmyptr)]
#[tokio::test]
async fn a_deferred_effect_is_offered_again_only_when_a_slot_frees_not_on_idle_turns() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let mut e = engine(j.clone());
    e.submit("b0", "s0", EventKind::new("build", "b0")).unwrap();
    e.submit("b1", "s1", EventKind::new("build", "b1")).unwrap();
    let (release, gate) = std::sync::mpsc::channel();
    let runner = Arc::new(Suites { gate: Mutex::new(Some(gate)), ..Suites::default() });
    let sink = MemSink::new();
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let kick: Arc<dyn Fn() + Send + Sync> = { let w = wakes.clone(); Arc::new(move || { w.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }) };
    let (tx, rx) = mpsc::channel::<Submit>(4);
    drop(tx);
    let serving = tokio::spawn(serve_with(e, Arc::new(WallClock), runner.clone(), PoolCaps { container: 1, ..PoolCaps::default() }, rx, ServeOpts { kick: Some(kick), max_sleep_ms: Some(5), ..opts(&sink) }));
    until(|| !deferred(&sink).is_empty()).await;
    assert_eq!(deferred(&sink).len(), 1, "the second effect is deferred once: {:?}", deferred(&sink));
    let seen = wakes.load(std::sync::atomic::Ordering::SeqCst);
    until(|| wakes.load(std::sync::atomic::Ordering::SeqCst) >= seen + 100).await;
    assert_eq!(deferred(&sink).len(), 1, "with no slot freed, the deferred effect is not offered again");
    let _ = release.send(());
    let _ = release.send(());
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    assert_eq!(deferred(&sink).len(), 1, "the freed slot takes it: no further deferral");
    assert_eq!(count(&*j, "built"), 2);
}

struct Named(Mutex<Vec<String>>);
impl EffectRunner for Named {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        Ok(spec.input.clone())
    }
    fn run_for_task(&self, task: &str, key: &str, spec: &EffectSpec) -> Result<String, String> {
        self.0.lock().unwrap().push(task.to_string());
        self.run(key, spec)
    }
}

#[test_id(tazpky0)]
#[tokio::test]
async fn the_live_host_names_the_registered_task_to_the_runner() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let mut e = engine(j.clone());
    e.submit("b0", "s0", EventKind::new("build", "b0")).unwrap();
    let runner = Arc::new(Named(Mutex::new(Vec::new())));
    let sink = MemSink::new();
    let (tx, rx) = mpsc::channel::<Submit>(4);
    drop(tx);
    let serving = tokio::spawn(serve_with(e, Arc::new(WallClock), runner.clone(), PoolCaps::default(), rx, opts(&sink)));
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    assert_eq!(*runner.0.lock().unwrap(), ["builds"]);
}

#[test_id(td7o6vb)]
#[test]
fn a_deferred_effect_is_lost_at_a_restart_and_never_runs_again() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let clock = VirtualClock::new(0);
    let mut e = engine_on(j.clone(), &clock);
    e.submit("b0", "s0", EventKind::new("build", "b0")).unwrap();
    e.submit("b1", "s1", EventKind::new("build", "b1")).unwrap();
    e.run_until_idle().unwrap();
    let pending = e.pending_effects();
    assert_eq!(pending.len(), 2);
    for p in &pending {
        e.note_queued(&p.key);
    }
    let pools = ClassPools::new(PoolCaps { container: 1, ..PoolCaps::default() });
    let held = pools.try_take(EffectClass::Container).expect("the first effect takes the only slot");
    e.note_attempt(&pending[0].key);
    let refused = pools.try_take(EffectClass::Container).expect_err("the class is at its cap");
    e.note_deferred(&pending[1].key, refused.class.name());
    drop(e);
    drop(held);

    let e = engine_on(j.clone(), &clock);
    assert!(e.pending_effects().is_empty(), "a restart offers no effect to a runner, the deferred one included");
    let runner = Arc::new(Suites::default());
    let mut driver = VirtualDriver::new(e, clock.clone(), runner.clone());
    driver.run_until(120_000).unwrap();
    assert_eq!(driver.effects_run(), 0);
    assert!(runner.ran.lock().unwrap().is_empty(), "nothing runs after the restart");
    assert_eq!(count(&*j, EFFECT_REQUESTED), 2, "the restart does not request the effects again");
    let lost = j.history().into_iter().filter(|ev| ev.kind.tag == EFFECT_TIMED_OUT && ev.kind.body == runtime::engine::LOST_AT_RESTART).count();
    assert_eq!(lost, 2, "each unresolved effect is told it was lost, once");
}

struct StartsTrue;
impl EffectRunner for StartsTrue {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        spira_config::bounded::bounded("true").status().map_err(|e| e.to_string())?;
        Ok(spec.input.clone())
    }
}

#[test_id(th9t8gk)]
#[tokio::test]
async fn a_succeeded_effect_that_started_a_program_writes_its_count_on_the_effect_record() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let sink = MemSink::new();
    let mut e = engine(j.clone()).with_sink(sink.clone());
    e.submit("b0", "s0", EventKind::new("build", "b0")).unwrap();
    let (tx, rx) = mpsc::channel::<Submit>(4);
    drop(tx);
    let serving = tokio::spawn(serve_with(e, Arc::new(WallClock), Arc::new(StartsTrue), PoolCaps::default(), rx, opts(&sink)));
    until(|| count(&*j, "built") == 1).await;
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    let effects: Vec<_> = sink.records().into_iter().filter(|f| f.get("unit") == Some(&Value::from("effect"))).collect();
    assert_eq!(effects.len(), 1, "{effects:?}");
    assert_eq!(effects[0].get("effect.outcome"), Some(&Value::from("succeeded")));
    assert_eq!(effects[0].get("effect.children.true"), Some(&Value::Int(1)));
    assert_eq!(effects[0].get("task"), Some(&Value::from("builds")), "the registered task, not the stream s0");
}
