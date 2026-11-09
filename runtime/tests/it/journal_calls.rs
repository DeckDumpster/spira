//! The scheduler's store costs one round-trip per engine turn, however many handlers consume the
//! turn's event, over the real socket client against a store that speaks spira-lc's `journal`
//! verbs; every event is shipped to VictoriaLogs ahead of the turn. The conformance suite runs
//! over the same client.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use runtime::conformance::{run_all, InProcessRig, Rig};
use runtime::engine::{Engine, Executor, Outcome};
use runtime::journal::{Boot, Journal, MemJournal, Poison, Turn};
use runtime::ports::{Clock, VirtualClock};
use runtime::socket::{answer, SocketJournal};
use runtime::*;
use spira_otel::vlogs::fake::FakeLogs;
use spira_otel::vlogs::Shipper;
use testkit::TempDir;

/// A store answering `journal <op>` over a `MemJournal`, counting requests and the turns among them.
pub(crate) struct Store {
    pub(crate) mem: Arc<MemJournal>,
    calls: AtomicUsize,
    turns: AtomicUsize,
    /// The next request is answered as spira-lc answers a store call past its deadline.
    pub(crate) deadline_next: std::sync::atomic::AtomicBool,
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap()
}

fn de<T: serde::de::DeserializeOwned>(s: &str) -> Result<T> {
    serde_json::from_str(s).map_err(|e| RuntimeError::refused(format!("journal: {e}")))
}

impl Store {
    fn respond(&self, argv: &[String]) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.deadline_next.swap(false, Ordering::SeqCst) {
            return Err(RuntimeError::cannot_tell("cannot tell: deadline"));
        }
        assert_eq!(argv.first().map(String::as_str), Some("journal"));
        let j = &*self.mem;
        Ok(match argv[1].as_str() {
            "turn" => {
                self.turns.fetch_add(1, Ordering::SeqCst);
                j.commit(&de::<Turn>(&argv[2])?)?;
                String::new()
            }
            "state" => json(&j.boot()?.state),
            "open-effects" => json(&j.boot()?.effects),
            "due" => json(&j.boot()?.schedules),
            "poison" => {
                j.poison(&de::<Poison>(&argv[2])?)?;
                String::new()
            }
            "poisoned" => json(&j.poisoned(&StreamKey(argv[2].clone()))?),
            op => return Err(RuntimeError::refused(format!("journal: unknown op {op:?}"))),
        })
    }
}

/// The store, its socket, and the directory holding it (removed when dropped).
pub(crate) fn serve() -> (Arc<Store>, PathBuf, TempDir) {
    let dir = TempDir::new("rkb6wl");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let store = Arc::new(Store { mem: MemJournal::new(), calls: AtomicUsize::new(0), turns: AtomicUsize::new(0), deadline_next: Default::default() });
    let s = Arc::clone(&store);
    std::thread::spawn(move || answer(&listener, &|argv| s.respond(argv)));
    (store, path, dir)
}

/// A socket journal shipping to a fake VictoriaLogs, whose records are its history.
struct Observed {
    inner: SocketJournal,
    logs: FakeLogs,
    recorded: AtomicUsize,
}

impl Observed {
    fn new(path: &std::path::Path) -> Self {
        let logs = FakeLogs::start();
        Observed { inner: SocketJournal::new(path, Shipper::start(&logs.url())), logs, recorded: AtomicUsize::new(0) }
    }
}

impl Journal for Observed {
    fn record(&self, ev: &Event) {
        self.recorded.fetch_add(1, Ordering::SeqCst);
        self.inner.record(ev)
    }
    fn boot(&self) -> Result<Boot> {
        self.inner.boot()
    }
    fn commit(&self, turn: &Turn) -> Result<()> {
        self.inner.commit(turn)
    }
    fn poison(&self, p: &Poison) -> Result<()> {
        self.inner.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> Result<Option<Poison>> {
        self.inner.poisoned(stream)
    }
    fn history(&self) -> Vec<Event> {
        let want = self.recorded.load(Ordering::SeqCst);
        self.logs
            .wait_records(want, Duration::from_secs(5))
            .into_iter()
            .map(|r| Event {
                id: EventId(r["event_id"].as_str().unwrap().into()),
                stream: StreamKey(r["stream"].as_str().unwrap().into()),
                kind: EventKind::new(r["kind_tag"].as_str().unwrap(), r["body"].as_str().unwrap()),
                at: r["at"].as_u64().unwrap(),
                cause: r["cause"].as_str().map(|c| EventId(c.into())),
                correlation: CorrelationId(r["correlation"].as_str().unwrap().into()),
            })
            .collect()
    }
}

/// On `tick`, one observe effect and its next tick; an outcome changes nothing.
struct Ticker;
impl Handler for Ticker {
    fn interests(&self) -> &[EventKindTag] {
        &["tick", EFFECT_SUCCEEDED]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag != "tick" {
            return vec![];
        }
        vec![
            Command::Effect(EffectSpec { name: "observe".into(), input: String::new(), deadline_ms: 5_000, key: "observe".into() }),
            Command::Schedule { at: ev.at + 5_000, kind: EventKind::new("tick", "") },
        ]
    }
}

/// Hears every outcome and does nothing with it, as the watchers do at idle.
struct Watcher;
impl Handler for Watcher {
    fn interests(&self) -> &[EventKindTag] {
        &[EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        vec![]
    }
    fn watches_foreign_outcomes(&self) -> bool {
        true
    }
}

fn regs(watchers: usize) -> Vec<Registration> {
    let mut r = vec![Registration::new("ticker", Ticker)];
    r.extend((0..watchers).map(|i| Registration::new(&format!("watcher-{i}"), Watcher)));
    r
}

fn shipper() -> (FakeLogs, Shipper) {
    let logs = FakeLogs::start();
    let s = Shipper::start(&logs.url());
    (logs, s)
}

/// The store calls of each turn in which an outcome arrives: the driver's `complete` and the
/// `run_until_idle` that dispatches it to every consumer.
fn outcome_turns(store: &Store, path: &std::path::Path, watchers: usize) -> Vec<usize> {
    let clock = VirtualClock::new(0);
    let (_logs, ship) = shipper();
    let mut x = Engine::open(Arc::new(SocketJournal::new(path, ship)), Arc::new(clock.clone()), regs(watchers), 1).unwrap();
    x.submit("boot", "task.ticker", EventKind::new("tick", "")).unwrap();
    x.run_until_idle().unwrap();
    let mut per_turn = vec![];
    while clock.now_ms() < 3_600_000 {
        for p in x.pending_effects() {
            let before = store.calls.load(Ordering::SeqCst);
            x.complete(&p.key, Outcome::Succeeded("ok".into())).unwrap();
            x.run_until_idle().unwrap();
            per_turn.push(store.calls.load(Ordering::SeqCst) - before);
        }
        clock.advance_to(clock.now_ms() + 5_000);
        x.run_until_idle().unwrap();
    }
    per_turn
}

#[test]
fn an_outcome_turn_costs_one_store_call_however_many_handlers_consume_it() {
    for watchers in [7, 0] {
        let (store, path, _dir) = serve();
        let turns = outcome_turns(&store, &path, watchers);
        assert!(turns.len() >= 700, "a virtual hour of 5 s ticks: {} outcome turns", turns.len());
        let worst = turns.iter().copied().max().unwrap();
        assert!(worst <= 2, "with {watchers} watchers an outcome turn made {worst} store calls (want <= 2: the turn's commit and a poison check); turns {:?}", &turns[..5]);
        assert!(turns.iter().filter(|&&n| n == 2).count() <= 15, "the poison check is asked once per recheck window, not per turn: {:?}", &turns[..5]);
    }
}

#[test]
fn a_restart_finds_the_effect_a_crash_left_unanswered_and_nothing_else() {
    let (store, path, _dir) = serve();
    let clock = VirtualClock::new(0);
    let (_logs, ship) = shipper();
    let mut x = Engine::open(Arc::new(SocketJournal::new(&path, ship)), Arc::new(clock.clone()), regs(3), 1).unwrap();
    x.submit("boot", "task.ticker", EventKind::new("tick", "")).unwrap();
    x.run_until_idle().unwrap();
    let key = x.pending_effects()[0].key.clone();
    std::mem::forget(x);
    let turns = store.turns.load(Ordering::SeqCst);
    let (_logs, ship) = shipper();
    let mut y = Engine::open(Arc::new(SocketJournal::new(&path, ship)), Arc::new(clock.clone()), regs(3), 1).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(y.pending_effects().iter().map(|p| p.key.clone()).collect::<Vec<_>>(), vec![key], "the unanswered effect is found, once");
    assert_eq!(store.turns.load(Ordering::SeqCst), turns, "the restart commits nothing");
    assert_eq!(store.mem.boot().unwrap().schedules.len(), 1, "and the next tick's row is waiting");
}

/// Every conformance scenario over the socket client, with the fake VictoriaLogs as the history.
#[test]
fn the_conformance_suite_holds_over_the_socket_journal() {
    let dirs: Arc<Mutex<Vec<TempDir>>> = Arc::default();
    let rig = InProcessRig(move || {
        let (_, path, dir) = serve();
        dirs.lock().unwrap().push(dir);
        Arc::new(Observed::new(&path)) as Arc<dyn Journal>
    });
    run_all(&rig as &dyn Rig);
}

fn ev(stream: &str, body: &str) -> Event {
    Event { id: EventId("e1".into()), stream: StreamKey(stream.into()), kind: EventKind::new("t", body), at: 7, cause: Some(EventId("c0".into())), correlation: CorrelationId("co".into()) }
}

#[test]
fn an_append_reaches_the_listener_with_the_bead_id_from_its_stream_or_else_its_body() {
    let (_store, path, _dir) = serve();
    let (logs, ship) = shipper();
    let j = SocketJournal::new(&path, ship);
    j.record(&ev("watch.sp-abcde1.tick", "no id here"));
    j.record(&Event { id: EventId("e2".into()), ..ev("runtime.heartbeat", "closing sp-94ptu6.2 now") });
    j.record(&Event { id: EventId("e3".into()), ..ev("runtime.heartbeat", "nothing") });
    let got = logs.wait_records(3, Duration::from_secs(5));
    assert_eq!(got.len(), 3, "{got:?}");
    let by = |id: &str| got.iter().find(|r| r["event_id"] == id).unwrap().clone();
    assert_eq!(by("e1")["bead_id"], "sp-abcde1", "the first sp- id in the stream name");
    assert_eq!(by("e2")["bead_id"], "sp-94ptu6.2", "else in the body");
    assert!(by("e3")["bead_id"].is_null());
    let r = by("e1");
    for (k, v) in [("stream", "watch.sp-abcde1.tick"), ("kind_tag", "t"), ("cause", "c0"), ("correlation", "co"), ("body", "no id here")] {
        assert_eq!(r[k], v, "{k}");
    }
    assert_eq!(r["at"], 7);
    let target = &logs.insert_targets()[0];
    assert!(target.contains("_stream_fields=stream,kind_tag") && target.contains("_time_field=at") && target.contains("_msg_field=kind_tag"), "{target}");
}

struct Echo;
impl Handler for Echo {
    fn interests(&self) -> &[EventKindTag] {
        &["ping"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        vec![Command::Emit(EventKind::new("pong", ""))]
    }
}

#[test]
fn an_event_is_shipped_before_the_turn_that_made_it_commits() {
    let (store, path, _dir) = serve();
    let (logs, ship) = shipper();
    let mut x = Engine::open(Arc::new(SocketJournal::new(&path, ship)), Arc::new(VirtualClock::new(5)), vec![Registration::new("echo", Echo)], 1).unwrap();
    x.submit("p1", "s", EventKind::new("ping", "")).unwrap();
    x.run_until_idle().unwrap();
    assert_eq!(store.turns.load(Ordering::SeqCst), 1);
    let shipped: Vec<String> = logs.wait_records(2, Duration::from_secs(5)).iter().map(|r| r["kind_tag"].as_str().unwrap().to_string()).collect();
    assert_eq!(shipped, ["ping", "pong"]);
}

#[test]
fn a_full_shipping_queue_is_counted_on_the_status_and_the_sink_and_never_blocks_the_engine() {
    let (_store, path, _dir) = serve();
    let hung = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ship = Shipper::with_capacity(&format!("http://{}", hung.local_addr().unwrap()), 2);
    let sink = MemSink::new();
    let mut x = Engine::open(Arc::new(SocketJournal::new(&path, ship)), Arc::new(VirtualClock::new(5)), vec![Registration::new("echo", Echo)], 1).unwrap().with_sink(sink.clone());
    let began = Instant::now();
    for i in 0..60 {
        x.submit(&format!("p{i}"), "s", EventKind::new("ping", "")).unwrap();
    }
    x.run_until_idle().unwrap();
    assert!(began.elapsed() < Duration::from_secs(2), "the engine ran at the speed of its own store, not the shipper's: {:?}", began.elapsed());
    let status = x.status();
    assert!(status.ship.is_some_and(|s| s.dropped >= 50 && s.failed == 0), "{:?}", status.ship);
    let v = serde_json::to_value(&status).unwrap();
    assert!(v["ship.dropped"].as_u64().unwrap() >= 50 && v["ship.failed"] == 0, "{v}");
    assert!(sink.records().iter().any(|r| matches!(r.get("ship.dropped"), Some(Value::Int(n)) if *n > 0)), "the wide events carry the count");
}

/// The scheduler's turns over the socket: an outcome turn commits once, a tick turn (its schedule
/// fired, an effect asked for, the next tick scheduled) once.
#[test]
fn an_engine_turn_commits_in_one_transaction() {
    let (store, path, _dir) = serve();
    let clock = VirtualClock::new(0);
    let (_logs, ship) = shipper();
    let mut x = Engine::open(Arc::new(SocketJournal::new(&path, ship)), Arc::new(clock.clone()), regs(3), 1).unwrap();
    x.submit("boot", "task.ticker", EventKind::new("tick", "")).unwrap();
    x.run_until_idle().unwrap();
    let txns = || store.turns.load(Ordering::SeqCst);
    let (mut outcome, mut tick) = (vec![], vec![]);
    while clock.now_ms() < 600_000 {
        for p in x.pending_effects() {
            let before = txns();
            x.complete(&p.key, Outcome::Succeeded("ok".into())).unwrap();
            x.run_until_idle().unwrap();
            outcome.push(txns() - before);
        }
        clock.advance_to(clock.now_ms() + 5_000);
        let before = txns();
        x.run_until_idle().unwrap();
        tick.push(txns() - before);
    }
    assert!(outcome.len() >= 100 && tick.len() >= 100);
    assert!(outcome.iter().all(|&n| n <= 1), "outcome turns: {:?}", &outcome[..10]);
    assert!(tick.iter().all(|&n| n <= 1), "tick turns: {:?}", &tick[..10]);
}
