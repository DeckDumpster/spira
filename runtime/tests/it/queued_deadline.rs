//! An effect's deadline counts from when its runner starts, never from when it was requested:
//! time spent waiting for its class's permit does not time it out. An effect written off as
//! timed out gives its permit back, so it stops holding the class's capacity. And the engine's
//! turn never waits behind effects for a worker.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::journal::Journal;
use runtime::offload::Pool;
use runtime::pools::{EffectClass, PoolCaps};
use runtime::ports::WallClock;
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, MemSink, Projection, Value, Registration, EFFECT_FAILED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use tokio::sync::mpsc;

/// `go <name>:<deadline_ms>` on stream `s-<name>` asks for effect `<name>`; `fan <n>:<name>`
/// asks for `n` of them at once. Each outcome is emitted as `<tag>:<name>`.
struct Asks;
impl Handler for Asks {
    fn interests(&self) -> &[EventKindTag] {
        &["go", "fan", EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "go" {
            let (name, ms) = ev.kind.body.split_once(':').unwrap();
            return vec![Command::Effect(EffectSpec { name: name.into(), input: name.into(), deadline_ms: ms.parse().unwrap(), key: name.into() })];
        }
        if ev.kind.tag == "fan" {
            let (n, name) = ev.kind.body.split_once(':').unwrap();
            return (0..n.parse::<usize>().unwrap()).map(|i| Command::Effect(EffectSpec { name: name.into(), input: i.to_string(), deadline_ms: 30_000, key: format!("{name}/{i}") })).collect();
        }
        let name = ev.stream.0.trim_start_matches("s-");
        vec![Command::Emit(EventKind::new("outcome", &format!("{}:{name}", ev.kind.tag)))]
    }
}

/// Every effect is host-class. `long` and `stuck` run until the test lets them go;
/// `wide` runs until `wide_go` is set; anything else answers at once. Records the effects that have started.
#[derive(Default)]
struct Host {
    started: Mutex<BTreeMap<String, usize>>,
    stuck: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    long: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// Set to let every `wide` runner return.
    wide_go: std::sync::atomic::AtomicBool,
    wide_running: std::sync::atomic::AtomicUsize,
    /// The most `wide` runners ever running at once.
    wide_peak: std::sync::atomic::AtomicUsize,
}

impl EffectRunner for Host {
    fn class(&self, _: &EffectSpec) -> EffectClass {
        EffectClass::Host
    }
    fn run(&self, _key: &str, spec: &EffectSpec) -> Result<String, String> {
        *self.started.lock().unwrap().entry(spec.name.clone()).or_default() += 1;
        match spec.name.as_str() {
            "long" => {
                let rx = self.long.lock().unwrap().take();
                if let Some(rx) = rx {
                    let _ = rx.recv();
                }
            }
            "wide" => {
                use std::sync::atomic::Ordering::SeqCst;
                let now = self.wide_running.fetch_add(1, SeqCst) + 1;
                self.wide_peak.fetch_max(now, SeqCst);
                for _ in 0..3000 {
                    if self.wide_go.load(SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10)); // clock-safe: a hang cap on a runner held until wide_go
                }
                self.wide_running.fetch_sub(1, SeqCst);
            }
            "stuck" => {
                let rx = self.stuck.lock().unwrap().take();
                if let Some(rx) = rx {
                    let _ = rx.recv();
                }
            }
            _ => {}
        }
        Ok(spec.name.clone())
    }
}

const QUEUED_FOR: Duration = Duration::from_millis(5500);

fn outcomes(j: &dyn Journal) -> Vec<String> {
    runtime::conformance::trace(j).into_iter().filter(|t| t.2 == "outcome").map(|t| t.3).collect()
}

fn started(r: &Host, name: &str) -> bool {
    r.started.lock().unwrap().contains_key(name)
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..3000 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: a 30 s hang cap on a wait for a named state
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
    let (release_long, long) = std::sync::mpsc::channel();
    let runner = Arc::new(Host { long: Mutex::new(Some(long)), ..Host::default() });
    let sink = MemSink::new();
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(engine(&j), Arc::new(WallClock), runner.clone(), host_cap_one(), rx, ServeOpts { sink: Some(sink.clone()), ..opts(4) }));
    tx.send(Submit::new("g-long", "s-long", EventKind::new("go", "long:600000"))).await.unwrap();
    until("the long effect to start", || started(&runner, "long")).await;
    tx.send(Submit::new("g-short", "s-short", EventKind::new("go", "short:5000"))).await.unwrap();
    until("the short effect to be deferred", || sink.records().iter().any(|f| f.get("unit") == Some(&Value::from("effect.deferred")))).await;
    tokio::time::sleep(QUEUED_FOR).await; // clock-safe: a lower bound; waiting longer changes nothing
    let _ = release_long.send(());
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    let got = outcomes(&*j);
    assert!(got.contains(&format!("{EFFECT_SUCCEEDED}:long")), "{got:?}");
    assert!(
        got.contains(&format!("{EFFECT_SUCCEEDED}:short")),
        "a 5 s effect that waited {QUEUED_FOR:?} for the host permit must still run and succeed, not time out in the queue: {got:?}"
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
    tx.send(Submit::new("g-quick", "s-quick", EventKind::new("go", "quick:600000"))).await.unwrap();
    until("the quick effect to finish", || outcomes(&*j).iter().any(|o| o.ends_with(":quick"))).await;
    let got = outcomes(&*j);
    let _ = let_go.send(());
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    assert!(
        got.contains(&format!("{EFFECT_SUCCEEDED}:quick")),
        "with host cap 1, an effect asked for after the only holder timed out must run while that runner is still stuck: {got:?}"
    );
}

#[tokio::test]
async fn a_turn_runs_while_every_effect_worker_is_busy() {
    let j: Arc<dyn Journal> = MemJournal::new();
    let (let_go, stuck) = std::sync::mpsc::channel();
    let runner = Arc::new(Host { stuck: Mutex::new(Some(stuck)), ..Host::default() });
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(engine(&j), Arc::new(WallClock), runner.clone(), host_cap_one(), rx, opts(1)));
    tx.send(Submit::new("g-stuck", "s-stuck", EventKind::new("go", "stuck:30000"))).await.unwrap();
    until("the stuck effect to start", || started(&runner, "stuck")).await;
    let (sub, journaled) = Submit::with_reply("g-next", "s-next", EventKind::new("go", "next:30000"));
    tx.send(sub).await.unwrap();
    let heard = tokio::time::timeout(Duration::from_secs(30), journaled).await; // clock-safe: a hang cap on the turn's reply
    let _ = let_go.send(());
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    assert!(
        matches!(heard, Ok(Ok(Ok(())))),
        "with the only effect worker held by a running effect, the engine's turn must still journal a submission while the worker is held: {heard:?}"
    );
}

#[tokio::test]
async fn one_task_fanning_out_leaves_room_in_its_class_for_the_others() {
    use std::sync::atomic::Ordering::SeqCst;
    let j: Arc<dyn Journal> = MemJournal::new();
    let runner = Arc::new(Host::default());
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let caps = PoolCaps { host: 4, ..PoolCaps::default() };
    let serving = tokio::spawn(serve_with(engine(&j), Arc::new(WallClock), runner.clone(), caps, rx, opts(8)));
    tx.send(Submit::new("g-wide", "s-wide", EventKind::new("fan", "6:wide"))).await.unwrap();
    until("the host to launch all the wide effects it will", || runner.wide_running.load(SeqCst) >= runtime::host::task_fan_out(4)).await;
    tx.send(Submit::new("g-other", "s-other", EventKind::new("go", "other:600000"))).await.unwrap();
    until("the other task's effect to finish", || outcomes(&*j).iter().any(|o| o.ends_with(":other"))).await;
    let got = outcomes(&*j);
    runner.wide_go.store(true, SeqCst);
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    assert_eq!(runner.wide_peak.load(SeqCst), runtime::host::task_fan_out(4), "one task holds at most half of host cap 4 at once");
    assert!(got.contains(&format!("{EFFECT_SUCCEEDED}:other")), "another task's host effect must run while one task fans out six: {got:?}");
    assert_eq!(outcomes(&*j).iter().filter(|o| *o == &format!("{EFFECT_SUCCEEDED}:wide")).count(), 6, "every fanned-out effect still runs");
}
