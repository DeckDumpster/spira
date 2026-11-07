//! sp-rkb6wl: the scheduler's journal costs one store round-trip per engine turn, however many
//! handlers consume the turn's event, over the real socket client against a store that speaks
//! spira-lc's `journal` verbs. The conformance suite runs over the same client.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use runtime::conformance::{run_all, InProcessRig, Rig};
use runtime::engine::{Engine, Executor, Outcome};
use runtime::journal::{Journal, MemJournal, Poison, ScheduleRow};
use runtime::ports::{Clock, VirtualClock};
use runtime::socket::{answer, SocketJournal};
use runtime::*;
use testkit::TempDir;

/// A store answering `journal <op>` over a `MemJournal`, counting requests. `batch` is spira-lc's
/// verb for staged writes plus one op; `knows_batch` false stands in for an older service.
struct Store {
    mem: Arc<MemJournal>,
    calls: AtomicUsize,
    knows_batch: bool,
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
        assert_eq!(argv.first().map(String::as_str), Some("journal"));
        self.op(&argv[1..])
    }

    fn op(&self, a: &[String]) -> Result<String> {
        let j = &*self.mem;
        let n = |i: usize| a[i].parse::<u64>().unwrap();
        Ok(match a.first().map(String::as_str).unwrap_or("") {
            "batch" if self.knows_batch => {
                let staged: serde_json::Value = de(&a[1])?;
                for p in staged["applied"].as_array().cloned().unwrap_or_default() {
                    j.mark_applied(p[0].as_str().unwrap(), &EventId(p[1].as_str().unwrap().into()))?;
                }
                if let Some(c) = staged["cursor"].as_u64() {
                    j.set_cursor(c)?;
                }
                return if a.len() > 2 { self.op(&a[2..]) } else { Ok(String::new()) };
            }
            "append" => j.append(&de::<Event>(&a[1])?)?.to_string(),
            "read" => json(&j.read_from(n(1), n(2) as usize)?),
            "stream" => json(&j.read_stream(&StreamKey(a[1].clone()), n(2), n(3) as usize)?),
            "end" => j.end()?.to_string(),
            "open-effects" => json(&j.unresolved_effects()?),
            "applied" => (j.applied(&a[1], &EventId(a[2].clone()))? as u8).to_string(),
            "mark-applied" => j.mark_applied(&a[1], &EventId(a[2].clone())).map(|_| String::new())?,
            "cursor" => j.cursor()?.to_string(),
            "set-cursor" => j.set_cursor(n(1)).map(|_| String::new())?,
            "schedule" => j.schedule(&de::<ScheduleRow>(&a[1])?).map(|_| String::new())?,
            "due" => json(&j.pending_schedules()?),
            "mark-fired" => j.mark_fired(&a[1]).map(|_| String::new())?,
            "poison" => j.poison(&de::<Poison>(&a[1])?).map(|_| String::new())?,
            "poisoned" => json(&j.poisoned(&StreamKey(a[1].clone()))?),
            op => return Err(RuntimeError::refused(format!("journal: unknown op {op:?}"))),
        })
    }
}

/// The store, its socket, and the directory holding it (removed when dropped).
fn serve(knows_batch: bool) -> (Arc<Store>, PathBuf, TempDir) {
    let dir = TempDir::new("rkb6wl");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let store = Arc::new(Store { mem: MemJournal::new(), calls: AtomicUsize::new(0), knows_batch });
    let s = Arc::clone(&store);
    std::thread::spawn(move || answer(&listener, &|argv| s.respond(argv)));
    (store, path, dir)
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
        vec![Command::Effect(EffectSpec { name: "observe".into(), input: String::new(), deadline_ms: 5_000 }), Command::Schedule { at: ev.at + 5_000, kind: EventKind::new("tick", "") }]
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

/// The store calls of each turn in which an outcome arrives: the driver's `complete` and the
/// `run_until_idle` that dispatches it to every consumer.
fn outcome_turns(store: &Store, path: &std::path::Path, watchers: usize) -> Vec<usize> {
    let clock = VirtualClock::new(0);
    let mut x = Engine::open(Arc::new(SocketJournal::new(path)), Arc::new(clock.clone()), regs(watchers), 1).unwrap();
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
fn an_outcome_turn_costs_a_fixed_number_of_store_calls_however_many_handlers_consume_it() {
    for watchers in [7, 0] {
        let (store, path, _dir) = serve(true);
        let turns = outcome_turns(&store, &path, watchers);
        assert!(turns.len() >= 700, "a virtual hour of 5 s ticks: {} outcome turns", turns.len());
        let worst = turns.iter().copied().max().unwrap();
        assert!(worst <= 3, "with {watchers} watchers an outcome turn made {worst} store calls (want <= 3: the outcome's append, the turn's read, its per-pump poison check); turns {:?}", &turns[..5]);
    }
}

#[test]
fn a_clean_stop_leaves_the_cursor_and_marks_durable_and_a_crash_replays_nothing_twice() {
    let (store, path, _dir) = serve(true);
    outcome_turns(&store, &path, 3);
    let (cursor, end) = (store.mem.cursor().unwrap(), store.mem.end().unwrap());
    assert_eq!(cursor, end, "the engine's drop sent what it staged");

    let (store, path, _dir) = serve(true);
    let clock = VirtualClock::new(0);
    let mut x = Engine::open(Arc::new(SocketJournal::new(&path)), Arc::new(clock.clone()), regs(3), 1).unwrap();
    x.submit("boot", "task.ticker", EventKind::new("tick", "")).unwrap();
    x.run_until_idle().unwrap();
    let key = x.pending_effects()[0].key.clone();
    x.complete(&key, Outcome::Succeeded("ok".into())).unwrap();
    x.run_until_idle().unwrap();
    std::mem::forget(x);
    let events = store.mem.end().unwrap();
    let mut y = Engine::open(Arc::new(SocketJournal::new(&path)), Arc::new(clock.clone()), regs(3), 1).unwrap();
    y.run_until_idle().unwrap();
    assert!(y.pending_effects().iter().all(|p| p.key != key), "an answered effect is not issued again");
    assert_eq!(store.mem.end().unwrap(), events, "the replay appends nothing");
}

#[test]
fn an_older_store_without_batch_still_gets_every_write() {
    let (store, path, _dir) = serve(false);
    let turns = outcome_turns(&store, &path, 3);
    assert!(!turns.is_empty());
    assert_eq!(store.mem.cursor().unwrap(), store.mem.end().unwrap());
}

/// Every conformance scenario over the socket client: crash and restart still re-issue an
/// unresolved effect exactly once, and replays add nothing.
#[test]
fn the_conformance_suite_holds_over_the_socket_journal() {
    let dirs: Arc<Mutex<Vec<TempDir>>> = Arc::default();
    let rig = InProcessRig(move || {
        let (_, path, dir) = serve(true);
        dirs.lock().unwrap().push(dir);
        Arc::new(SocketJournal::new(path)) as Arc<dyn Journal>
    });
    run_all(&rig as &dyn Rig);
}
