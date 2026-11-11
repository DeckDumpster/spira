//! Effector classes on the wall-clock host: a class at its cap defers, records it, and runs the
//! effect later; a deferred effect survives a restart and runs exactly once.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::journal::Journal;
use runtime::offload::Pool;
use runtime::pools::{EffectClass, PoolCaps};
use runtime::ports::WallClock;
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Executor, Handler, MemJournal, MemSink, Projection, Registration, Value, EFFECT_SUCCEEDED};
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
            let _ = rx.recv_timeout(Duration::from_secs(5)); // clock-safe: a test holding a runner open
        }
        Ok(spec.input.clone())
    }
}

fn engine(j: Arc<dyn Journal>) -> Engine {
    Engine::open(j, Arc::new(WallClock), vec![Registration::new("builds", Builds)], 1).unwrap()
}

fn deferred(sink: &MemSink) -> Vec<BTreeMap<String, Value>> {
    sink.records().into_iter().filter(|f| f.get("unit") == Some(&Value::from("effect.deferred"))).collect()
}

fn opts(sink: &Arc<MemSink>) -> ServeOpts {
    ServeOpts { sink: Some(sink.clone()), pool: Some(Pool::new(4, 16)), ..ServeOpts::default() }
}

fn count(j: &dyn Journal, tag: &str) -> usize {
    runtime::conformance::trace(j).iter().filter(|t| t.2 == tag).count()
}

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
    for _ in 0..200 {
        if !deferred(&sink).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    let d = deferred(&sink);
    assert!(!d.is_empty(), "a container effect over the cap must be deferred and recorded");
    assert_eq!(d[0].get("class"), Some(&Value::from("container")));
    assert!(d[0].contains_key("task"), "the record names the task");
    assert_eq!(d[0].get("waits_for"), Some(&Value::from("container.slot")), "the record names what it waits for");
    assert_eq!(runner.ran.lock().unwrap().values().sum::<usize>(), 1, "the cap holds: one run at a time");
    for _ in 0..3 {
        let _ = release.send(());
    }
    tokio::time::timeout(Duration::from_secs(20), serving).await.expect("the host drains").unwrap().unwrap();
    assert_eq!(count(&*j, "built"), 3, "every deferred effect runs: nothing is dropped");
    assert!(runner.ran.lock().unwrap().values().all(|n| *n == 1), "each effect runs once");
    let status = board.get().expect("the engine published its status");
    let container = status.classes.iter().find(|c| c.class == "container").unwrap_or_else(|| panic!("status names the runner's class: {:?}", status.classes));
    assert!(container.refusals >= 1, "status counts the class's refusals: {container:?}");
    assert!(status.classes.iter().all(|c| c.class != "host" || c.refusals == 0), "{:?}", status.classes);
}

#[tokio::test]
async fn a_deferred_effect_is_lost_at_a_restart_and_never_runs_again() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let mut e = engine(j.clone());
    e.submit("b0", "s0", EventKind::new("build", "b0")).unwrap();
    e.submit("b1", "s1", EventKind::new("build", "b1")).unwrap();
    let (_hold, gate) = std::sync::mpsc::channel::<()>();
    let runner = Arc::new(Suites { gate: Mutex::new(Some(gate)), ..Suites::default() });
    let sink = MemSink::new();
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(e, Arc::new(WallClock), runner.clone(), PoolCaps { container: 1, ..PoolCaps::default() }, rx, opts(&sink)));
    for _ in 0..200 {
        if !deferred(&sink).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    assert!(!deferred(&sink).is_empty(), "the second effect was deferred");
    serving.abort();
    drop(tx);
    let ran_before: Vec<String> = runner.ran.lock().unwrap().keys().cloned().collect();
    assert_eq!(ran_before.len(), 1);

    let e = engine(j.clone());
    assert!(e.pending_effects().is_empty(), "a restart offers no effect to a runner, the deferred one included");
    let after = Arc::new(Suites::default());
    let (tx, rx) = mpsc::channel::<Submit>(4);
    drop(tx);
    tokio::time::timeout(Duration::from_secs(20), serve_with(e, Arc::new(WallClock), after.clone(), PoolCaps { container: 1, ..PoolCaps::default() }, rx, opts(&sink))).await.expect("the host drains").unwrap();
    assert!(after.ran.lock().unwrap().is_empty(), "nothing ran after the restart");
    assert_eq!(count(&*j, runtime::EFFECT_REQUESTED), 2, "a restart requests nothing again");
}

#[tokio::test]
async fn a_deferred_effect_is_offered_again_only_when_a_slot_frees_not_on_idle_turns() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let mut e = engine(j.clone());
    e.submit("b0", "s0", EventKind::new("build", "b0")).unwrap();
    e.submit("b1", "s1", EventKind::new("build", "b1")).unwrap();
    let (release, gate) = std::sync::mpsc::channel();
    let runner = Arc::new(Suites { gate: Mutex::new(Some(gate)), ..Suites::default() });
    let sink = MemSink::new();
    let (tx, rx) = mpsc::channel::<Submit>(4);
    drop(tx);
    let serving = tokio::spawn(serve_with(e, Arc::new(WallClock), runner.clone(), PoolCaps { container: 1, ..PoolCaps::default() }, rx, opts(&sink)));
    for _ in 0..200 {
        if !deferred(&sink).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    assert_eq!(deferred(&sink).len(), 1, "the second effect is deferred once: {:?}", deferred(&sink));
    tokio::time::sleep(Duration::from_millis(1500)).await; // clock-safe: idle turns would show as more records
    assert_eq!(deferred(&sink).len(), 1, "with no slot freed, the deferred effect is not offered again");
    let _ = release.send(());
    let _ = release.send(());
    tokio::time::timeout(Duration::from_secs(20), serving).await.expect("the host drains").unwrap().unwrap();
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
    tokio::time::timeout(Duration::from_secs(20), serving).await.expect("the host drains").unwrap().unwrap();
    assert_eq!(*runner.0.lock().unwrap(), ["builds"]);
}
