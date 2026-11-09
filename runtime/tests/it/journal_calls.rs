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

/// What the store speaks: `group` (sp-4du857) is spira-lc's verb for every staged write plus one
/// op, `batch` (sp-rkb6wl) its older verb for marks and the cursor only; a store at a lower level
/// stands in for an older service.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Plain,
    Batch,
    Group,
}

/// A store answering `journal <op>` over a `MemJournal`, counting requests and the write
/// transactions among them: a request that writes anything is one transaction, as spira-lc runs it.
pub(crate) struct Store {
    pub(crate) mem: Arc<MemJournal>,
    calls: AtomicUsize,
    txns: AtomicUsize,
    wrote: std::sync::atomic::AtomicBool,
    level: Level,
    /// Told after each request is answered.
    heard: Mutex<Option<std::sync::mpsc::Sender<()>>>,
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
        self.wrote.store(false, Ordering::SeqCst);
        let r = self.op(&argv[1..]);
        if self.wrote.load(Ordering::SeqCst) {
            self.txns.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(tx) = &*self.heard.lock().unwrap() {
            let _ = tx.send(());
        }
        r
    }

    fn write(&self) {
        self.wrote.store(true, Ordering::SeqCst);
    }

    fn staged(&self, staged: &serde_json::Value) -> Result<()> {
        let j = &*self.mem;
        for w in staged["writes"].as_array().cloned().unwrap_or_default() {
            self.write();
            if let Some(ev) = w.get("append") {
                j.append(&de::<Event>(&ev.to_string())?)?;
            } else if let Some(row) = w.get("schedule") {
                j.schedule(&de::<ScheduleRow>(&row.to_string())?)?;
            } else if let Some(id) = w.get("fired").and_then(|v| v.as_str()) {
                j.mark_fired(id)?;
            } else {
                return Err(RuntimeError::refused(format!("journal: unknown write {w}")));
            }
        }
        for p in staged["applied"].as_array().cloned().unwrap_or_default() {
            self.write();
            j.mark_applied(p[0].as_str().unwrap(), &EventId(p[1].as_str().unwrap().into()))?;
        }
        if let Some(c) = staged["cursor"].as_u64() {
            self.write();
            j.set_cursor(c)?;
        }
        Ok(())
    }

    fn op(&self, a: &[String]) -> Result<String> {
        let j = &*self.mem;
        let n = |i: usize| a[i].parse::<u64>().unwrap();
        Ok(match a.first().map(String::as_str).unwrap_or("") {
            op @ ("batch" | "group") if self.level >= if op == "group" { Level::Group } else { Level::Batch } => {
                let staged: serde_json::Value = de(&a[1])?;
                assert!(op == "group" || staged.get("writes").is_none(), "batch carries marks and the cursor only");
                self.staged(&staged)?;
                return if a.len() > 2 { self.op(&a[2..]) } else { Ok(String::new()) };
            }
            "append" => {
                self.write();
                j.append(&de::<Event>(&a[1])?)?.to_string()
            }
            "read" => json(&j.read_from(n(1), n(2) as usize)?),
            "stream" => json(&j.read_stream(&StreamKey(a[1].clone()), n(2), n(3) as usize)?),
            "end" => j.end()?.to_string(),
            "open-effects" => json(&j.unresolved_effects()?),
            "applied" => (j.applied(&a[1], &EventId(a[2].clone()))? as u8).to_string(),
            "mark-applied" => {
                self.write();
                j.mark_applied(&a[1], &EventId(a[2].clone())).map(|_| String::new())?
            }
            "cursor" => j.cursor()?.to_string(),
            "set-cursor" => {
                self.write();
                j.set_cursor(n(1)).map(|_| String::new())?
            }
            "schedule" => {
                self.write();
                j.schedule(&de::<ScheduleRow>(&a[1])?).map(|_| String::new())?
            }
            "due" => json(&j.pending_schedules()?),
            "mark-fired" => {
                self.write();
                j.mark_fired(&a[1]).map(|_| String::new())?
            }
            "poison" => {
                self.write();
                j.poison(&de::<Poison>(&a[1])?).map(|_| String::new())?
            }
            "poisoned" => json(&j.poisoned(&StreamKey(a[1].clone()))?),
            op => return Err(RuntimeError::refused(format!("journal: unknown op {op:?}"))),
        })
    }
}

/// The store, its socket, and the directory holding it (removed when dropped).
pub(crate) fn serve(level: Level) -> (Arc<Store>, PathBuf, TempDir) {
    let dir = TempDir::new("rkb6wl");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let store = Arc::new(Store { mem: MemJournal::new(), calls: AtomicUsize::new(0), txns: AtomicUsize::new(0), wrote: Default::default(), level, heard: Mutex::new(None), deadline_next: Default::default() });
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
        let (store, path, _dir) = serve(Level::Group);
        let turns = outcome_turns(&store, &path, watchers);
        assert!(turns.len() >= 700, "a virtual hour of 5 s ticks: {} outcome turns", turns.len());
        let worst = turns.iter().copied().max().unwrap();
        assert!(worst <= 2, "with {watchers} watchers an outcome turn made {worst} store calls (want <= 2: the turn's read, carrying the outcome, and its per-pump poison check); turns {:?}", &turns[..5]);
    }
}

#[test]
fn a_clean_stop_leaves_the_cursor_and_marks_durable_and_a_crash_replays_nothing_twice() {
    let (store, path, _dir) = serve(Level::Group);
    outcome_turns(&store, &path, 3);
    let (cursor, end) = (store.mem.cursor().unwrap(), store.mem.end().unwrap());
    assert_eq!(cursor, end, "the engine's drop sent what it staged");

    let (store, path, _dir) = serve(Level::Group);
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
fn an_older_store_without_group_or_batch_still_gets_every_write() {
    for level in [Level::Batch, Level::Plain] {
        let (store, path, _dir) = serve(level);
        let turns = outcome_turns(&store, &path, 3);
        assert!(turns.len() >= 700);
        assert_eq!(store.mem.cursor().unwrap(), store.mem.end().unwrap());
        assert_eq!(store.mem.pending_schedules().unwrap().len(), 1, "every tick fired but the next");
    }
}

/// Every conformance scenario over the socket client: crash and restart still re-issue an
/// unresolved effect exactly once, and replays add nothing.
#[test]
fn the_conformance_suite_holds_over_the_socket_journal() {
    let dirs: Arc<Mutex<Vec<TempDir>>> = Arc::default();
    let rig = InProcessRig(move || {
        let (_, path, dir) = serve(Level::Group);
        dirs.lock().unwrap().push(dir);
        Arc::new(SocketJournal::new(path)) as Arc<dyn Journal>
    });
    run_all(&rig as &dyn Rig);
}

fn ev(i: usize) -> Event {
    Event { id: EventId(format!("e{i}")), stream: StreamKey("s".into()), kind: EventKind::new("t", "b"), at: i as u64, cause: None, correlation: CorrelationId(format!("c{i}")) }
}

fn row(id: &str) -> ScheduleRow {
    ScheduleRow { id: id.into(), at: 9, stream: StreamKey("s".into()), kind: EventKind::new("tick", ""), cause: None, correlation: CorrelationId("c".into()) }
}

/// sp-4du857: the records, schedule rows and marks staged between two requests commit in one
/// transaction, in the order they were made, and a clean stop sends what is still staged.
#[test]
fn records_staged_between_requests_commit_as_one_transaction_in_order() {
    let (store, path, _dir) = serve(Level::Group);
    let j = SocketJournal::new(&path);
    for i in 0..10 {
        j.record(&ev(i)).unwrap();
    }
    j.schedule(&row("r1")).unwrap();
    j.schedule(&row("r2")).unwrap();
    j.mark_fired("r1").unwrap();
    j.mark_applied("h", &EventId("e0".into())).unwrap();
    j.set_cursor(3).unwrap();
    assert_eq!(store.calls.load(Ordering::SeqCst), 0, "nothing is sent until a request or a flush");
    j.flush().unwrap();
    assert_eq!((store.calls.load(Ordering::SeqCst), store.txns.load(Ordering::SeqCst)), (1, 1), "one request, one transaction");
    let ids: Vec<String> = store.mem.read_from(0, 100).unwrap().into_iter().map(|(_, e)| e.id.0).collect();
    assert_eq!(ids, (0..10).map(|i| format!("e{i}")).collect::<Vec<_>>(), "journal order is record order");
    assert_eq!(store.mem.pending_schedules().unwrap(), vec![row("r2")], "r1 was scheduled before it was fired");
    assert!(store.mem.applied("h", &EventId("e0".into())).unwrap());
    assert_eq!(store.mem.cursor().unwrap(), 3);

    j.record(&ev(10)).unwrap();
    let read = j.read_from(0, 100).unwrap();
    assert_eq!(read.len(), 11, "a read sees what was recorded before it");
    assert_eq!(store.txns.load(Ordering::SeqCst), 2, "the read carried the record in its own transaction");

    j.record(&ev(11)).unwrap();
    drop(j);
    assert_eq!(store.mem.read_from(0, 100).unwrap().len(), 12, "dropping the journal sends what it staged");
}

/// A record the store would refuse (here an id past its 255 characters) is never staged: it is
/// sent at once, so its refusal reaches the caller that made it.
#[test]
fn a_record_past_the_stores_widths_is_sent_alone_at_once() {
    let (store, path, _dir) = serve(Level::Group);
    let j = SocketJournal::new(&path);
    j.record(&Event { id: EventId("i".repeat(256)), ..ev(0) }).unwrap();
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);
    j.record(&ev(1)).unwrap();
    assert_eq!(store.calls.load(Ordering::SeqCst), 1);
}

/// The crash-loss window: a staged write with no request after it is sent by the flusher, not
/// left for a request that may never come. When it is due is `Staged::due` (socket.rs's tests).
#[test]
fn a_staged_write_reaches_the_store_from_the_flusher_with_no_request_after_it() {
    let (store, path, _dir) = serve(Level::Group);
    let (tx, rx) = std::sync::mpsc::channel();
    *store.heard.lock().unwrap() = Some(tx);
    let j = SocketJournal::flushing(&path, std::time::Duration::from_millis(20));
    j.record(&ev(0)).unwrap();
    j.mark_applied("h", &EventId("e0".into())).unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the flusher sent a request"); // clock-safe: a cap on the wait for the store's own signal
    assert_eq!(store.mem.end().unwrap(), 1, "the flusher sent the record");
    assert!(store.mem.applied("h", &EventId("e0".into())).unwrap(), "in the same transaction");
    assert_eq!(store.txns.load(Ordering::SeqCst), 1);
}

/// The scheduler's turns over the socket: an outcome turn commits once, a tick turn (its schedule
/// fired, an effect asked for, the next tick scheduled) twice, where each write was its own
/// transaction before.
#[test]
fn an_engine_turn_commits_its_writes_in_at_most_one_transaction_per_read() {
    let (store, path, _dir) = serve(Level::Group);
    let clock = VirtualClock::new(0);
    let mut x = Engine::open(Arc::new(SocketJournal::new(&path)), Arc::new(clock.clone()), regs(3), 1).unwrap();
    x.submit("boot", "task.ticker", EventKind::new("tick", "")).unwrap();
    x.run_until_idle().unwrap();
    let txns = || store.txns.load(Ordering::SeqCst);
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
    assert!(tick.iter().all(|&n| n <= 2), "tick turns: {:?}", &tick[..10]);
    drop(x);
    assert_eq!(store.mem.cursor().unwrap(), store.mem.end().unwrap(), "a clean stop sent the last turn's marks and cursor");
}
