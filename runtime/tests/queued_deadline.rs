//! An effect's deadline counts from when its runner starts, never from when it was requested:
//! time spent waiting for its class's permit does not time it out. An effect written off as
//! timed out gives its permit back, so it stops holding the class's capacity.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::journal::Journal;
use runtime::offload::Pool;
use runtime::pools::{EffectClass, PoolCaps};
use runtime::ports::WallClock;
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration, EFFECT_FAILED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use tokio::sync::mpsc;

/// `go <name>:<deadline_ms>` on stream `s-<name>` asks for effect `<name>`; each outcome is
/// emitted as `<tag>:<name>`.
struct Asks;
impl Handler for Asks {
    fn interests(&self) -> &[EventKindTag] {
        &["go", EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "go" {
            let (name, ms) = ev.kind.body.split_once(':').unwrap();
            return vec![Command::Effect(EffectSpec { name: name.into(), input: name.into(), deadline_ms: ms.parse().unwrap() })];
        }
        let name = ev.stream.0.trim_start_matches("s-");
        vec![Command::Emit(EventKind::new("outcome", &format!("{}:{name}", ev.kind.tag)))]
    }
}

/// Every effect is host-class. `long` runs `LONG`; `stuck` runs until the test lets it go;
/// anything else answers at once. Records the effects that have started.
#[derive(Default)]
struct Host {
    started: Mutex<BTreeMap<String, usize>>,
    stuck: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

const LONG: Duration = Duration::from_millis(700);

impl EffectRunner for Host {
    fn class(&self, _: &EffectSpec) -> EffectClass {
        EffectClass::Host
    }
    fn run(&self, _key: &str, spec: &EffectSpec) -> Result<String, String> {
        *self.started.lock().unwrap().entry(spec.name.clone()).or_default() += 1;
        match spec.name.as_str() {
            "long" => std::thread::sleep(LONG), // clock-safe: a test runner holding its permit
            "stuck" => {
                let rx = self.stuck.lock().unwrap().take();
                if let Some(rx) = rx {
                    let _ = rx.recv_timeout(Duration::from_secs(10)); // clock-safe: a test holding a runner open
                }
            }
            _ => {}
        }
        Ok(spec.name.clone())
    }
}

fn outcomes(j: &dyn Journal) -> Vec<String> {
    runtime::conformance::trace(j).into_iter().filter(|t| t.2 == "outcome").map(|t| t.3).collect()
}

fn started(r: &Host, name: &str) -> bool {
    r.started.lock().unwrap().contains_key(name)
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    panic!("timed out waiting for {what}");
}

fn host_cap_one() -> PoolCaps {
    PoolCaps { host: 1, ..PoolCaps::default() }
}

fn opts(workers: usize) -> ServeOpts {
    ServeOpts { pool: Some(Pool::new(workers, 16)), ..ServeOpts::default() }
}

fn engine(j: &Arc<dyn Journal>) -> Engine {
    Engine::open(j.clone(), Arc::new(WallClock), vec![Registration::new("asks", Asks)], 1).unwrap()
}

#[tokio::test]
async fn an_effect_queued_past_its_nominal_deadline_still_runs_once_it_gets_a_permit() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let runner = Arc::new(Host::default());
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(engine(&j), Arc::new(WallClock), runner.clone(), host_cap_one(), rx, opts(4)));
    tx.send(Submit::new("g-long", "s-long", EventKind::new("go", "long:10000"))).await.unwrap();
    until("the long effect to start", || started(&runner, "long")).await;
    tx.send(Submit::new("g-short", "s-short", EventKind::new("go", "short:200"))).await.unwrap();
    drop(tx);
    tokio::time::timeout(Duration::from_secs(20), serving).await.expect("the host drains").unwrap().unwrap();
    let got = outcomes(&*j);
    assert!(got.contains(&format!("{EFFECT_SUCCEEDED}:long")), "{got:?}");
    assert!(
        got.contains(&format!("{EFFECT_SUCCEEDED}:short")),
        "a 200 ms effect that waited {LONG:?} for the host permit must still run and succeed, not time out in the queue: {got:?}"
    );
    assert!(started(&runner, "short"), "the queued effect's runner ran");
}

#[tokio::test]
async fn a_timed_out_effect_gives_its_permit_back() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let (let_go, stuck) = std::sync::mpsc::channel();
    let runner = Arc::new(Host { stuck: Mutex::new(Some(stuck)), ..Host::default() });
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(engine(&j), Arc::new(WallClock), runner.clone(), host_cap_one(), rx, opts(4)));
    tx.send(Submit::new("g-stuck", "s-stuck", EventKind::new("go", "stuck:200"))).await.unwrap();
    until("the stuck effect to start", || started(&runner, "stuck")).await;
    until("the stuck effect to time out", || outcomes(&*j).contains(&format!("{EFFECT_TIMED_OUT}:stuck"))).await;
    tx.send(Submit::new("g-quick", "s-quick", EventKind::new("go", "quick:3000"))).await.unwrap();
    for _ in 0..200 {
        if outcomes(&*j).iter().any(|o| o.ends_with(":quick")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    let got = outcomes(&*j);
    let _ = let_go.send(());
    drop(tx);
    tokio::time::timeout(Duration::from_secs(20), serving).await.expect("the host drains").unwrap().unwrap();
    assert!(
        got.contains(&format!("{EFFECT_SUCCEEDED}:quick")),
        "with host cap 1, an effect asked for after the only holder timed out must run while that runner is still stuck: {got:?}"
    );
}

