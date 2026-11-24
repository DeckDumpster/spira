//! An overrunning turn names the step it is in, and the record of its end keeps the longest step.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::journal::{Boot, Journal, Poison, Turn};
use runtime::marker::{Mark, Marker, MemMarker};
use runtime::pools::PoolCaps;
use runtime::ports::WallClock;
use runtime::{Command, Ctx, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, MemSink, Projection, Registration, Value};
use tokio::sync::mpsc;

const BUDGET: Duration = Duration::from_millis(50);

struct Echo;
impl Handler for Echo {
    fn interests(&self) -> &[EventKindTag] {
        &["ping"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        vec![Command::Emit(EventKind::new("pong", ""))]
    }
}

struct NoEffects;
impl EffectRunner for NoEffects {
    fn run(&self, _: &str, _: &runtime::EffectSpec) -> Result<String, String> {
        Ok(String::new())
    }
}

/// A door a call waits at while it is shut.
#[derive(Default)]
struct Door(Mutex<bool>, Condvar);

impl Door {
    fn shut(&self) {
        *self.0.lock().unwrap() = true;
    }
    fn open(&self) {
        *self.0.lock().unwrap() = false;
        self.1.notify_all();
    }
    fn pass(&self) {
        let _open = self.1.wait_while(self.0.lock().unwrap(), |shut| *shut).unwrap();
    }
}

struct HeldMarker(MemMarker, Arc<Door>);
impl Marker for HeldMarker {
    fn set(&self, m: &Mark) -> runtime::Result<()> {
        self.1.pass();
        self.0.set(m)
    }
    fn clear(&self) -> runtime::Result<()> {
        self.0.clear()
    }
    fn take(&self) -> runtime::Result<Option<Mark>> {
        self.0.take()
    }
}

struct HeldCommit(Arc<dyn Journal>, Arc<Door>);
impl Journal for HeldCommit {
    fn record(&self, ev: &Event) {
        self.0.record(ev)
    }
    fn boot(&self) -> runtime::Result<Boot> {
        self.0.boot()
    }
    fn commit(&self, turn: &Turn) -> runtime::Result<()> {
        self.1.pass();
        self.0.commit(turn)
    }
    fn poison(&self, p: &Poison) -> runtime::Result<()> {
        self.0.poison(p)
    }
    fn poisoned(&self, stream: &runtime::StreamKey) -> runtime::Result<Option<Poison>> {
        self.0.poisoned(stream)
    }
}

fn turns(sink: &MemSink, outcome: &str) -> Vec<runtime::wide::Fields> {
    sink.records().into_iter().filter(|r| r.get("unit") == Some(&Value::from("loop.turn")) && r.get("outcome") == Some(&Value::from(outcome))).collect()
}

/// Holds `door` shut through one ping turn until its overrun is recorded, then lets it end.
async fn overrun_at(engine: Engine, door: &Door, sink: &Arc<MemSink>) -> (runtime::wide::Fields, runtime::wide::Fields) {
    let opts = ServeOpts { sink: Some(sink.clone()), turn_budget: Some(BUDGET), ..ServeOpts::default() };
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(NoEffects), PoolCaps::default(), rx, opts));
    door.shut();
    tx.send(Submit::new("p1", "s", EventKind::new("ping", ""))).await.unwrap();
    wait_for(|| !turns(sink, "overrun").is_empty()).await;
    door.open();
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    (turns(sink, "overrun").remove(0), turns(sink, "ok").remove(0))
}

async fn wait_for(f: impl Fn() -> bool) {
    for _ in 0..3000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: a 30 s hang cap on a wait for a named state
    }
    panic!("timed out waiting for the host");
}

#[test_id(tr3p9kd)]
#[tokio::test]
async fn a_marker_held_past_the_turn_budget_is_named_in_the_overrun_with_its_handler_and_kept_in_the_end() {
    let door = Arc::new(Door::default());
    let engine = Engine::open(MemJournal::new(), Arc::new(WallClock), vec![Registration::new("echo", Echo)], 1).unwrap().with_marker(Arc::new(HeldMarker(MemMarker::default(), door.clone()))).unwrap();
    let sink = MemSink::new();
    let (overrun, end) = overrun_at(engine, &door, &sink).await;
    assert_eq!(overrun.get("turn.step"), Some(&Value::from("marker.set")), "{overrun:?}");
    assert_eq!(overrun.get("turn.step_of"), Some(&Value::from("echo")), "{overrun:?}");
    assert!(overrun.contains_key("turn.step_ms"), "{overrun:?}");
    assert_eq!(end.get("turn.longest_step"), Some(&Value::from("marker.set")), "{end:?}");
    let ms = match end.get("turn.longest_step_ms") {
        Some(Value::Int(n)) => *n as u64,
        other => panic!("the end record keeps the step's length: {other:?}"),
    };
    assert!(ms >= BUDGET.as_millis() as u64, "{end:?}");
}

#[test_id(tm7xq2c)]
#[tokio::test]
async fn a_commit_held_past_the_turn_budget_is_named_in_the_overrun() {
    let door = Arc::new(Door::default());
    let journal: Arc<dyn Journal> = Arc::new(HeldCommit(MemJournal::new(), door.clone()));
    let engine = Engine::open(journal, Arc::new(WallClock), vec![Registration::new("echo", Echo)], 1).unwrap();
    let sink = MemSink::new();
    let (overrun, end) = overrun_at(engine, &door, &sink).await;
    assert_eq!(overrun.get("turn.step"), Some(&Value::from("journal.commit")), "{overrun:?}");
    assert_eq!(end.get("turn.longest_step"), Some(&Value::from("journal.commit")), "{end:?}");
}
