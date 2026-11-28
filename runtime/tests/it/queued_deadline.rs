//! An effect's deadline counts from when its runner starts, never from when it was requested:
//! time spent waiting for its class's permit does not time it out. An effect written off as
//! timed out gives its permit back, so it stops holding the class's capacity. And the engine's
//! turn never waits behind effects for a worker.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::journal::{Boot, Journal, Poison, Turn};
use runtime::offload::Pool;
use runtime::pools::{EffectClass, PoolCaps};
use runtime::ports::{VirtualClock, WallClock};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, MemSink, Projection, Value, Registration, StreamKey, Executor, EFFECT_FAILED, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT};
use tokio::sync::mpsc;

/// `go <name>:<deadline_ms>` on stream `s-<name>` asks for effect `<name>`; `fan <n>:<name>`
/// asks for `n` of them at once. Each outcome is emitted as `<tag>:<name>`.
struct Asks;
impl Handler for Asks {
    fn interests(&self) -> &[EventKindTag] {
        &["go", "fan", "note", EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "go" {
            let (name, ms) = ev.kind.body.split_once(':').unwrap();
            return vec![Command::Effect(EffectSpec { name: name.into(), input: name.into(), deadline_ms: ms.parse().unwrap(), key: name.into() })];
        }
        if ev.kind.tag == "note" {
            return vec![Command::Emit(EventKind::new("noted", &ev.kind.body))];
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

#[test_id(t3xxzec)]
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

#[test_id(tip8aki)]
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

#[test_id(t3o2ucm)]
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

#[test_id(t99inee)]
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

/// A journal whose every commit costs engine time, as a slow store does. A commit made while
/// `hold` is set also waits until the `answers` effect's runner has returned, then costs a minute.
/// Once `stall_after` commits are made, each further commit waits for an effect's runner to start.
struct SlowJournal {
    inner: Arc<dyn Journal>,
    clock: VirtualClock,
    per_commit_ms: u64,
    commits: std::sync::atomic::AtomicUsize,
    hold: std::sync::atomic::AtomicBool,
    began: std::sync::atomic::AtomicBool,
    answered: Arc<std::sync::atomic::AtomicBool>,
    stall_after: usize,
    ran: std::sync::atomic::AtomicBool,
}

impl Journal for SlowJournal {
    fn record(&self, ev: &Event) {
        self.inner.record(ev)
    }
    fn boot(&self) -> runtime::Result<Boot> {
        self.inner.boot()
    }
    fn commit(&self, turn: &Turn) -> runtime::Result<()> {
        use std::sync::atomic::Ordering::SeqCst;
        let made = self.commits.fetch_add(1, SeqCst);
        if self.stall_after > 0 && made >= self.stall_after {
            for _ in 0..3000 {
                if self.ran.load(SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10)); // clock-safe: a hang cap; a turn that never hands back holds the effect unlaunched
            }
        }
        if self.hold.swap(false, SeqCst) {
            self.began.store(true, SeqCst);
            while !self.answered.load(SeqCst) {
                std::thread::yield_now();
            }
            std::thread::sleep(Duration::from_millis(100)); // clock-safe: lets the answer reach the host's channel; the 60 s commit is virtual
            self.clock.advance(60_000);
        }
        self.clock.advance(self.per_commit_ms);
        self.inner.commit(turn)
    }
    fn poison(&self, p: &Poison) -> runtime::Result<()> {
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> runtime::Result<Option<Poison>> {
        self.inner.poisoned(stream)
    }
    fn poisoned_all(&self) -> runtime::Result<Vec<Poison>> {
        self.inner.poisoned_all()
    }
    fn history(&self) -> Vec<Event> {
        self.inner.history()
    }
}

fn slow(per_commit_ms: u64) -> (Arc<SlowJournal>, VirtualClock) {
    stalling(per_commit_ms, 0)
}

fn stalling(per_commit_ms: u64, stall_after: usize) -> (Arc<SlowJournal>, VirtualClock) {
    let clock = VirtualClock::new(1_000);
    let j = Arc::new(SlowJournal {
        inner: MemJournal::new(),
        clock: clock.clone(),
        per_commit_ms,
        commits: Default::default(),
        hold: Default::default(),
        began: Default::default(),
        answered: Default::default(),
        stall_after,
        ran: Default::default(),
    });
    (j, clock)
}

fn slow_engine(j: &Arc<SlowJournal>, clock: &VirtualClock) -> Engine {
    let dynj: Arc<dyn Journal> = j.clone();
    Engine::open(dynj, Arc::new(clock.clone()), vec![Registration::new("asks", Asks)], 1).unwrap()
}

/// Answers `answers` only after the slow commit has begun, and records how many commits had been made when each effect started.
struct Timed {
    j: Arc<SlowJournal>,
    start_commits: Mutex<BTreeMap<String, usize>>,
}

impl EffectRunner for Timed {
    fn class(&self, _: &EffectSpec) -> EffectClass {
        EffectClass::Host
    }
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        use std::sync::atomic::Ordering::SeqCst;
        self.start_commits.lock().unwrap().insert(spec.name.clone(), self.j.commits.load(SeqCst));
        self.j.ran.store(true, SeqCst);
        match spec.name.as_str() {
            "answers" => {
                while !self.j.began.load(SeqCst) {
                    std::thread::yield_now();
                }
                self.j.answered.store(true, SeqCst);
            }
            "never" => std::thread::sleep(Duration::from_secs(3)), // clock-safe: outlasts the 200 ms wall deadline
            _ => {}
        }
        Ok(spec.name.clone())
    }
}

#[test_id(tt6gg73)]
#[tokio::test]
async fn an_effect_that_answered_during_a_long_commit_is_not_timed_out() {
    use std::sync::atomic::Ordering::SeqCst;
    let (j, clock) = slow(0);
    let runner = Arc::new(Timed { j: j.clone(), start_commits: Default::default() });
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(slow_engine(&j, &clock), Arc::new(clock.clone()), runner.clone(), host_cap_one(), rx, opts(4)));
    tx.send(Submit::new("g-answers", "s-answers", EventKind::new("go", "answers:5000"))).await.unwrap();
    until("the answering effect to start", || runner.start_commits.lock().unwrap().contains_key("answers")).await;
    j.hold.store(true, SeqCst);
    tx.send(Submit::new("g-note", "s-note", EventKind::new("note", "slow"))).await.unwrap();
    until("the effect's outcome to be journaled", || outcomes(&*j.inner).iter().any(|o| o.ends_with(":answers"))).await;
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    assert_eq!(outcomes(&*j.inner), vec![format!("{EFFECT_SUCCEEDED}:answers")]);
}

#[test_id(tb01d7q)]
#[tokio::test]
async fn a_backlog_of_events_does_not_hold_back_an_effect_the_first_asked_for() {
    use std::sync::atomic::Ordering::SeqCst;
    let (j, clock) = stalling(1_000, 4);
    let runner = Arc::new(Timed { j: j.clone(), start_commits: Default::default() });
    let mut e = slow_engine(&j, &clock);
    e.submit("e-00", "s-first", EventKind::new("go", "first:30000")).unwrap();
    for i in 1..20 {
        e.submit(&format!("e-{i:02}"), "s-rest", EventKind::new("note", &i.to_string())).unwrap();
    }
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(e, Arc::new(clock.clone()), runner.clone(), host_cap_one(), rx, opts(4)));
    until("the first event's effect to start", || runner.start_commits.lock().unwrap().contains_key("first")).await;
    let at_start = runner.start_commits.lock().unwrap()["first"];
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
    let notes: Vec<String> = runtime::conformance::trace(&*j.inner).into_iter().filter(|t| t.2 == "note").map(|t| t.3).collect();
    assert_eq!(notes, (1..20).map(|i| i.to_string()).collect::<Vec<_>>(), "event order is unchanged");
    assert!(at_start <= 5, "the effect must be launched within the first turn's slice, not after the backlog of 20 events; {at_start} commits had been made when it started");
    assert!(j.commits.load(SeqCst) >= 20, "each event still commits once");
}

#[test_id(tdht952)]
#[tokio::test]
async fn an_effect_that_never_answers_still_times_out() {
    let (j, clock) = slow(0);
    let runner = Arc::new(Timed { j: j.clone(), start_commits: Default::default() });
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(slow_engine(&j, &clock), Arc::new(clock.clone()), runner.clone(), host_cap_one(), rx, opts(4)));
    tx.send(Submit::new("g-never", "s-never", EventKind::new("go", "never:200"))).await.unwrap();
    until("the effect to time out", || outcomes(&*j.inner).contains(&format!("{EFFECT_TIMED_OUT}:never"))).await;
    drop(tx);
    tokio::time::timeout(Duration::from_secs(60), serving).await.expect("the host drains").unwrap().unwrap();
}
