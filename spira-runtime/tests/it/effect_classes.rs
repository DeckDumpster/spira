//! The service's effects run under their own classes: with the host class full, a dwarf start
//! (`sentinel-call.unit_start`, unit class in its contract) still starts and succeeds.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::journal::Journal;
use runtime::offload::Pool;
use runtime::pools::{EffectClass, PoolCaps};
use runtime::ports::WallClock;
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration, EFFECT_FAILED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use spira_runtime::tasks::a::sentinel_call::{METRIC, UNIT_START};
use tokio::sync::mpsc;

/// `go <effect>` on stream `s-<effect>` asks for it; each outcome is emitted as `<tag>:<effect>`.
struct Asks;
impl Handler for Asks {
    fn interests(&self) -> &[EventKindTag] {
        &["go", EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "go" {
            return vec![Command::Effect(EffectSpec::by_input(ev.kind.body.clone(), String::new(), 5_000))];
        }
        vec![Command::Emit(EventKind::new("outcome", &format!("{}:{}", ev.kind.tag, ev.stream.0.trim_start_matches("s-"))))]
    }
}

/// Classes every effect as the service's own runner does; runs the metric until let go and
/// answers anything else at once.
struct AsServed {
    effects: spira_runtime::Effects,
    started: Mutex<BTreeSet<String>>,
    hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl EffectRunner for AsServed {
    fn class(&self, spec: &EffectSpec) -> EffectClass {
        self.effects.class(spec)
    }
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        self.started.lock().unwrap().insert(spec.name.clone());
        if spec.name == METRIC {
            let rx = self.hold.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.recv_timeout(Duration::from_secs(10)); // clock-safe: a test holding a runner open
            }
        }
        Ok(String::new())
    }
}

fn outcomes(j: &dyn Journal) -> Vec<String> {
    runtime::conformance::trace(j).into_iter().filter(|t| t.2 == "outcome").map(|t| t.3).collect()
}

#[tokio::test]
async fn with_the_host_class_full_a_unit_start_still_starts_and_succeeds() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let engine = Engine::open(j.clone(), Arc::new(WallClock), vec![Registration::new("asks", Asks)], 1).unwrap();
    let (let_go, hold) = std::sync::mpsc::channel();
    let runner = Arc::new(AsServed { effects: spira_runtime::Effects::default(), started: Mutex::default(), hold: Mutex::new(Some(hold)) });
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let opts = ServeOpts { pool: Some(Pool::new(4, 16)), ..ServeOpts::default() };
    let serving = tokio::spawn(serve_with(engine, Arc::new(WallClock), runner.clone(), PoolCaps { host: 1, ..PoolCaps::default() }, rx, opts));
    tx.send(Submit::new("g-metric", &format!("s-{METRIC}"), EventKind::new("go", METRIC))).await.unwrap();
    for _ in 0..500 {
        if runner.started.lock().unwrap().contains(METRIC) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    tx.send(Submit::new("g-start", &format!("s-{UNIT_START}"), EventKind::new("go", UNIT_START))).await.unwrap();
    let want = format!("{EFFECT_SUCCEEDED}:{UNIT_START}");
    for _ in 0..200 {
        if outcomes(&*j).contains(&want) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    let got = outcomes(&*j);
    let _ = let_go.send(());
    drop(tx);
    tokio::time::timeout(Duration::from_secs(20), serving).await.expect("the host drains").unwrap().unwrap();
    assert!(got.contains(&want), "with host cap 1 held by {METRIC}, {UNIT_START} must run under the unit class and succeed within 2 s: {got:?}");
}
