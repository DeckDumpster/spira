//! A config change reaching the running scheduler: `spira-config`'s request over the control
//! socket, the engine's turn taking it at a safe point, and `scheduler running` reporting
//! what is held meanwhile. The task is the real `sentinel-call` on the real engine; the observe
//! answer plays the runner, which reads the pool size from the pin the probe was handed.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::engine::{Outcome, StatusBoard};
use runtime::journal::MemJournal;
use runtime::ports::VirtualClock;
use runtime::socket::{answer, lc_call};
use runtime::{Engine, EventKind, Executor, PendingEffect};
use spira_runtime::repin::Repin;
use spira_runtime::status::{Control, SubmitFn};
use spira_runtime::tasks::a::sentinel_call::{ObserveIn, Snapshot, NAME, OBSERVE, STREAM, TICK, UNIT_START};

const KEY: &str = "SPIRA_MAX_DWARVES";

type File = Arc<Mutex<BTreeMap<String, String>>>;
type Queue = Arc<Mutex<Vec<(String, String, EventKind)>>>;

struct World {
    engine: Engine,
    clock: VirtualClock,
    file: File,
    queue: Queue,
    repin: Arc<Repin>,
    journal: Arc<MemJournal>,
    live: u32,
    sock: std::path::PathBuf,
    _dir: testkit::TempDir,
}

fn lookup_with(file: &BTreeMap<String, String>) -> impl Fn(&str) -> Result<String, String> + Send + Sync + 'static {
    let file = file.clone();
    let base = spira_runtime::fixture::lookup("7");
    move |k| file.get(k).cloned().map_or_else(|| base(k), Ok)
}

/// Stands for `batcher.local`: its carry is whatever the test says it is.
struct Batcher;
impl runtime::Handler for Batcher {
    fn interests(&self) -> &[runtime::EventKindTag] {
        &["test.carry"]
    }
    fn handle(&self, _: &runtime::Ctx, _: &runtime::Projection, ev: &runtime::Event) -> Vec<runtime::Command> {
        vec![runtime::Command::Emit(EventKind::new(spira_runtime::tasks::a::batcher_local::STATE, &ev.kind.body))]
    }
}

fn world(pool: &str) -> World {
    let boot: BTreeMap<String, String> = [(KEY.to_string(), pool.to_string())].into();
    let settings = spira_runtime::Settings::from_lookup(&lookup_with(&boot)).unwrap();
    let file: File = Arc::new(Mutex::new(boot.clone()));
    let (clock, board) = (VirtualClock::new(0), Arc::new(StatusBoard::default()));
    let task = spira_runtime::tasks::all().find(|t| t.name == NAME).unwrap();
    let journal = MemJournal::new();
    let engine = Engine::open(journal.clone(), Arc::new(clock.clone()), vec![(task.build)(&settings), runtime::Registration::new(spira_runtime::tasks::a::batcher_local::NAME, Batcher)], 1).unwrap().with_status_board(board.clone());
    let f = file.clone();
    let fresh = Arc::new(move |keys: &[String]| Ok(keys.iter().map(|k| (k.clone(), f.lock().unwrap().get(k).cloned().unwrap_or_default())).collect()));
    let repin = Arc::new(Repin::new(settings, fresh, Arc::new(lookup_with(&boot))));
    let queue: Queue = Arc::default();
    let q = queue.clone();
    let submit: SubmitFn = Arc::new(move |id: &str, stream: &str, kind: EventKind| {
        q.lock().unwrap().push((id.into(), stream.into(), kind));
        Ok(())
    });
    let dir = testkit::TempDir::new("repin-sock");
    let sock = dir.path().join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let control = Control::new(board, submit, Arc::new(clock.clone())).with_repin(repin.clone());
    std::thread::spawn(move || answer(&listener, &|argv| control.respond(argv)));
    World { engine, clock, file, queue, repin, journal, live: 0, sock, _dir: dir }
}

impl World {
    fn change(&self, value: &str) {
        self.file.lock().unwrap().insert(KEY.into(), value.into());
    }

    fn ask(&self, argv: &[&str]) -> Result<String, runtime::RuntimeError> {
        lc_call(&self.sock, argv)
    }

    /// One turn as the host runs it: submissions in, the re-pin point, then the handlers.
    fn turn(&mut self) {
        for (id, stream, kind) in self.queue.lock().unwrap().drain(..) {
            self.engine.submit(&id, &stream, kind).unwrap();
        }
        self.repin.apply(&mut self.engine);
        self.engine.run_until_idle().unwrap();
    }

    fn pending(&self, name: &str) -> Vec<PendingEffect> {
        self.engine.pending_effects().into_iter().filter(|p| p.spec.name == name).collect()
    }

    /// Answers an observe the way the runner does: the pool size is the one the probe was handed.
    fn answer_observe(&mut self, ready: u32) {
        for p in self.pending(OBSERVE) {
            let pinned = serde_json::from_str::<ObserveIn>(&p.spec.input).unwrap().pinned;
            let snap = Snapshot {
                ready_by_rune: [("builder".to_string(), Some(ready))].into(),
                live_by_rune: [("builder".to_string(), self.live)].into(),
                max_live: Some(10),
                lanes_max_live: Some(2),
                express_ready: Some(0),
                max_dwarves: pinned.get(KEY).and_then(|v| v.parse().ok()),
                roster: vec!["builder".into()],
                ..Snapshot::default()
            };
            self.engine.complete(&p.key, Outcome::Succeeded(serde_json::to_string(&snap).unwrap())).unwrap();
        }
        self.engine.run_until_idle().unwrap();
    }

    fn tick(&mut self) {
        self.engine.start(STREAM, EventKind::new(TICK, ""));
        self.turn();
    }

    fn calls(&mut self, ready: u32) -> usize {
        self.tick();
        self.answer_observe(ready);
        let started = self.pending(UNIT_START).len();
        self.live += started as u32;
        for p in self.pending(UNIT_START) {
            self.engine.complete(&p.key, Outcome::Succeeded("{}".into())).unwrap();
        }
        self.engine.run_until_idle().unwrap();
        started
    }

    fn driving_a_round(&mut self) {
        use spira_runtime::tasks::a::batcher_local as b;
        let carry = b::Carry { repos: [("spira".to_string(), b::RepoCarry { state: b::State::Certifying, ..b::RepoCarry::default() })].into() };
        self.engine.start(b::STREAM, EventKind::new("test.carry", &serde_json::to_string(&carry).unwrap()));
    }
}

#[test]
fn a_pool_size_set_from_0_to_4_with_no_landing_calls_four_builders_within_one_tick() {
    let mut w = world("0");
    assert_eq!(w.calls(6), 0, "control: a pool of 0 calls nobody");
    w.change("4");
    let told: serde_json::Value = serde_json::from_str(&w.ask(&["scheduler", "repin", KEY]).unwrap()).unwrap();
    assert_eq!((told["state"].as_str(), told["held"].as_str(), told["wanted"].as_str()), (Some("pending"), Some("0"), Some("4")));
    assert_eq!(w.ask(&["scheduler", "running", KEY]).unwrap(), "0", "until a turn takes it, the old value is the one held");
    w.clock.advance_to(61_000);
    w.turn();
    assert_eq!(w.ask(&["scheduler", "running", KEY]).unwrap(), "4");
    let facts: Vec<String> = w.journal.log().into_iter().filter(|e| e.kind.tag == spira_runtime::repin::REPINNED).map(|e| e.kind.body).collect();
    assert_eq!(facts, vec![format!("re-pinned {KEY} 0 -> 4")], "one journal fact says what was re-pinned");
    assert_eq!(w.calls(6), 1, "the first tick after the re-pin calls a builder");

    let mut full = world("0");
    full.change("4");
    full.ask(&["scheduler", "repin", KEY]).unwrap();
    full.turn();
    full.live = 4;
    assert_eq!(full.calls(6), 0, "four are live: the pool is full");
}

#[test]
fn the_same_change_while_a_round_is_driven_waits_for_the_round_and_status_shows_it_pending() {
    let mut w = world("0");
    w.driving_a_round();
    w.turn();
    w.change("4");
    w.ask(&["scheduler", "repin", KEY]).unwrap();
    w.turn();
    assert_eq!(w.ask(&["scheduler", "running", KEY]).unwrap(), "0", "a round is being driven");
    w.engine.run_until_idle().unwrap();
    let status: serde_json::Value = serde_json::from_str(&w.ask(&["scheduler", "status"]).unwrap()).unwrap();
    assert_eq!(status["repin"]["pending"][KEY], serde_json::json!({ "held": "0", "wanted": "4" }));
    assert!(status["repin"]["waits"].as_str().unwrap().contains("round"), "{status}");

    use spira_runtime::tasks::a::batcher_local as b;
    let done = b::Carry { repos: [("spira".to_string(), b::RepoCarry { state: b::State::Done, ..b::RepoCarry::default() })].into() };
    w.engine.start(b::STREAM, EventKind::new("test.carry", &serde_json::to_string(&done).unwrap()));
    w.turn();
    w.turn();
    assert_eq!(w.ask(&["scheduler", "running", KEY]).unwrap(), "4", "the round ended");
    let status: serde_json::Value = serde_json::from_str(&w.ask(&["scheduler", "status"]).unwrap()).unwrap();
    assert!(status["repin"]["pending"].as_object().unwrap().is_empty());
}

#[test]
fn a_task_that_started_before_a_re_pin_reads_the_value_it_started_with() {
    let mut w = world("0");
    w.tick();
    let started = w.pending(OBSERVE);
    assert_eq!(started.len(), 1);
    w.change("4");
    w.ask(&["scheduler", "repin", KEY]).unwrap();
    w.turn();
    assert_eq!(w.ask(&["scheduler", "running", KEY]).unwrap(), "0", "its probe is in flight");
    let held = serde_json::from_str::<ObserveIn>(&w.pending(OBSERVE)[0].spec.input).unwrap().pinned;
    assert_eq!(held[KEY], "0");
    w.answer_observe(6);
    assert_eq!(w.pending(UNIT_START).len(), 0, "the started tick decided on its own pin");
    for p in w.pending(UNIT_START) {
        w.engine.complete(&p.key, Outcome::Succeeded("{}".into())).unwrap();
    }
    w.turn();
    assert_eq!(w.ask(&["scheduler", "running", KEY]).unwrap(), "4", "taken once the tick is over");
}

#[test]
fn a_key_the_scheduler_pins_only_at_start_is_told_to_restart_and_an_unchanged_one_is_unchanged() {
    let w = world("0");
    let told: serde_json::Value = serde_json::from_str(&w.ask(&["scheduler", "repin", "SPIRA_RUNTIME_STALL_LIMIT"]).unwrap()).unwrap();
    assert_eq!(told["state"], "restart");
    let same: serde_json::Value = serde_json::from_str(&w.ask(&["scheduler", "repin", KEY]).unwrap()).unwrap();
    assert_eq!(same["state"], "unchanged");
    assert!(w.ask(&["scheduler", "running", "SPIRA_RUNTIME_STALL_LIMIT"]).unwrap_err().is_refused());
}

/// Answers every observe with an empty world and keeps the pool size each probe was handed.
struct Probes(Mutex<Vec<String>>);
impl runtime::host::EffectRunner for Probes {
    fn run(&self, _: &str, spec: &runtime::EffectSpec) -> Result<String, String> {
        if spec.name == OBSERVE {
            let pinned = serde_json::from_str::<ObserveIn>(&spec.input).unwrap().pinned;
            self.0.lock().unwrap().push(pinned[KEY].clone());
        }
        Ok(serde_json::to_string(&Snapshot::default()).unwrap())
    }
}

#[tokio::test]
async fn the_served_scheduler_takes_a_change_at_the_start_of_its_next_turn_with_no_restart() {
    use std::time::Duration;
    let boot: BTreeMap<String, String> = [(KEY.to_string(), "0".to_string())].into();
    let settings = spira_runtime::Settings::from_lookup(&lookup_with(&boot)).unwrap();
    let file: File = Arc::new(Mutex::new(boot.clone()));
    let f = file.clone();
    let fresh = Arc::new(move |keys: &[String]| Ok(keys.iter().map(|k| (k.clone(), f.lock().unwrap().get(k).cloned().unwrap_or_default())).collect()));
    let repin = Arc::new(Repin::new(settings.clone(), fresh, Arc::new(lookup_with(&boot))));
    let task = spira_runtime::tasks::all().find(|t| t.name == NAME).unwrap();
    let board = Arc::new(StatusBoard::default());
    let engine = Engine::open(MemJournal::new(), Arc::new(runtime::ports::WallClock), vec![(task.build)(&settings)], 1).unwrap().with_status_board(board.clone());
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let dir = testkit::TempDir::new("repin-served");
    let sock = dir.path().join("control.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let control = Control::new(board, spira_runtime::control_submit(tx.clone()), Arc::new(runtime::ports::WallClock)).with_repin(repin.clone());
    std::thread::spawn(move || answer(&listener, &|argv| control.respond(argv)));
    let probes = Arc::new(Probes(Mutex::default()));
    let hook = repin.clone();
    let opts = runtime::host::ServeOpts { before_turn: Some(Arc::new(move |e| hook.apply(e))), ..Default::default() };
    let serving = tokio::spawn(runtime::host::serve_with(engine, Arc::new(runtime::ports::WallClock), probes.clone(), runtime::pools::PoolCaps::default(), rx, opts));

    let tick = |id: &'static str| runtime::host::Submit::new(id, STREAM, EventKind::new(TICK, ""));
    tx.send(tick("tick-1")).await.unwrap();
    let wait = |n: usize| {
        let p = probes.clone();
        async move {
            for _ in 0..500 {
                if p.0.lock().unwrap().len() >= n {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
            }
            panic!("no probe number {n}");
        }
    };
    wait(1).await;
    file.lock().unwrap().insert(KEY.into(), "4".into());
    let s = sock.clone();
    tokio::task::spawn_blocking(move || lc_call(&s, &["scheduler", "repin", KEY]).unwrap()).await.unwrap();
    for _ in 0..500 {
        let s = sock.clone();
        if tokio::task::spawn_blocking(move || lc_call(&s, &["scheduler", "running", KEY]).unwrap()).await.unwrap() == "4" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    tx.send(tick("tick-2")).await.unwrap();
    wait(2).await;
    drop(tx);
    let seen = probes.0.lock().unwrap().clone();
    serving.abort();
    assert_eq!(seen[..2], ["0".to_string(), "4".to_string()], "the probe after the change carries the new pool");
}
