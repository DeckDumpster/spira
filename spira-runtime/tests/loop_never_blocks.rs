//! The loop never makes a blocking call: a stalled disk or journal slows work down and does not
//! stop the watchdog kicks, the timers or the heartbeat. Drives the real `serve_with`.
// covers: runtime/src/*.rs spira-runtime/src/*.rs spira-otel/src/logs.rs

use std::collections::BTreeSet;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::offload::{self, Pool};
use runtime::ports::WallClock;
use runtime::*;
use spira_runtime::notify::Notifier;
use spira_runtime::sink::OtlpSink;
use spira_runtime::{open_engine, Effects};
use tokio::sync::mpsc;

mod common;
use common::settings;

const WINDOW: Duration = Duration::from_millis(400);

struct Kicks(Arc<Mutex<Vec<Instant>>>);

impl Kicks {
    fn gaps_within(&self, from: Instant, to: Instant, window: Duration) -> std::result::Result<(), String> {
        let at = self.0.lock().unwrap().clone();
        let mut last = from;
        for t in at.iter().filter(|t| **t >= from && **t <= to).chain(std::iter::once(&to)) {
            if t.duration_since(last) > window {
                return Err(format!("no kick for {:?} before {:?}", t.duration_since(last), t.duration_since(from)));
            }
            last = *t;
        }
        Ok(())
    }
}

fn notifier(d: &testkit::TempDir) -> (Arc<Notifier>, Kicks) {
    let sock = d.path().join("notify.sock");
    let rx = UnixDatagram::bind(&sock).unwrap();
    let at = Arc::new(Mutex::new(vec![]));
    let seen = at.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        while let Ok(n) = rx.recv(&mut buf) {
            if &buf[..n] == b"WATCHDOG=1" {
                seen.lock().unwrap().push(Instant::now());
            }
        }
    });
    (Arc::new(Notifier::new(sock.to_str(), Some(600_000)).unwrap()), Kicks(at))
}

fn opts(n: &Arc<Notifier>, sink: Option<Arc<dyn WideSink>>, pool: Option<Arc<Pool>>) -> ServeOpts {
    let k = n.clone();
    ServeOpts { kick: Some(Arc::new(move || k.kick())), max_sleep_ms: n.max_sleep().map(|m| m.as_millis() as u64), sink, pool }
}

struct Counts(Arc<AtomicUsize>);
impl Handler for Counts {
    fn interests(&self) -> &[EventKindTag] {
        &["go"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        self.0.fetch_add(1, Ordering::SeqCst);
        vec![]
    }
}

struct Gate(Mutex<bool>, Condvar);
impl Gate {
    fn new() -> Arc<Gate> {
        Arc::new(Gate(Mutex::new(false), Condvar::new()))
    }
    fn wait(&self) {
        let mut open = self.0.lock().unwrap();
        while !*open {
            open = self.1.wait(open).unwrap();
        }
    }
    fn open(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }
}

/// Parks any append of an event whose id is `park*` until the gate opens.
struct Parking(Arc<MemJournal>, Arc<Gate>);
impl Journal for Parking {
    fn append(&self, ev: &Event) -> runtime::Result<Offset> {
        if ev.id.0.starts_with("park") {
            self.1.wait();
        }
        self.0.append(ev)
    }
    fn read_from(&self, from: Offset, limit: usize) -> runtime::Result<Vec<(Offset, Event)>> {
        self.0.read_from(from, limit)
    }
    fn applied(&self, handler: &str, id: &EventId) -> runtime::Result<bool> {
        self.0.applied(handler, id)
    }
    fn mark_applied(&self, handler: &str, id: &EventId) -> runtime::Result<()> {
        self.0.mark_applied(handler, id)
    }
    fn cursor(&self) -> runtime::Result<Offset> {
        self.0.cursor()
    }
    fn set_cursor(&self, next: Offset) -> runtime::Result<()> {
        self.0.set_cursor(next)
    }
    fn schedule(&self, row: &ScheduleRow) -> runtime::Result<()> {
        self.0.schedule(row)
    }
    fn pending_schedules(&self) -> runtime::Result<Vec<ScheduleRow>> {
        self.0.pending_schedules()
    }
    fn mark_fired(&self, row_id: &str) -> runtime::Result<()> {
        self.0.mark_fired(row_id)
    }
    fn poison(&self, p: &Poison) -> runtime::Result<()> {
        self.0.poison(p)
    }
    fn poisoned(&self, stream: &StreamKey) -> runtime::Result<Option<Poison>> {
        self.0.poisoned(stream)
    }
}

fn go(id: &str) -> Submit {
    Submit { id: id.into(), stream: "s".into(), kind: EventKind::new("go", "") }
}

fn count_of(j: &MemJournal, tag: &str) -> usize {
    j.read_from(0, 10_000).unwrap().iter().filter(|(_, e)| e.kind.tag == tag).count()
}

async fn until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    while !done() && Instant::now() < end { // clock-safe: cap on a wait for the loop under test
        tokio::time::sleep(Duration::from_millis(20)).await; // clock-safe: polls state the loop under test changes
    }
    done()
}

fn today_log(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(format!("spira-runtime.logs.{}.jsonl", spira_otel::utc_date(std::time::SystemTime::now())))
}

#[tokio::test]
async fn a_stalled_telemetry_disk_does_not_stop_kicks_or_the_heartbeat() {
    let d = testkit::TempDir::new("lnb-disk");
    let s = settings("", d.path());
    std::fs::create_dir_all(s.otel_dir()).unwrap();
    let fifo = today_log(&s.otel_dir());
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "{}", std::io::Error::last_os_error());
    let (n, kicks) = notifier(&d);
    let mem = MemJournal::new();
    let sink: Arc<OtlpSink> = Arc::new(OtlpSink::new(spira_otel::logs::LogSink::new(s.otel_dir(), "spira-runtime"), 1));
    let mut engine = open_engine(&s, mem.clone(), Arc::new(WallClock), Arc::new(NullMarker), sink.clone(), vec![]).unwrap();
    let (id, stream, kind) = runtime::heartbeat::Heartbeat::boot();
    engine.submit(id, stream, kind).unwrap();
    let dropped = spira_otel::logs::queue_dropped();
    let (tx, rx) = mpsc::channel(1);
    std::mem::forget(tx);
    let host = tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(Effects::default()), 2, rx, opts(&n, Some(sink), None)));
    let from = Instant::now();
    tokio::time::sleep(Duration::from_secs(3)).await; // clock-safe: the observation window
    let to = Instant::now();
    assert!(!host.is_finished());
    host.abort();
    kicks.gaps_within(from, to, WINDOW).unwrap();
    assert!(count_of(&mem, "heartbeat") >= 3, "the boot beat and at least two scheduled ones reach the journal");
    assert!(spira_otel::logs::queue_dropped() > dropped, "a full telemetry queue drops and counts");
    let _ = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&fifo);
}

use std::os::unix::fs::OpenOptionsExt;

#[tokio::test]
async fn a_stalled_journal_does_not_stop_kicks_and_a_long_turn_leaves_a_record() {
    let d = testkit::TempDir::new("lnb-journal");
    let s = settings("", d.path());
    let (n, kicks) = notifier(&d);
    let gate = Gate::new();
    let journal: Arc<dyn Journal> = Arc::new(Parking(MemJournal::new(), gate.clone()));
    let seen = Arc::new(AtomicUsize::new(0));
    let loop_sink = MemSink::new();
    let engine = open_engine(&s, journal.clone(), Arc::new(WallClock), Arc::new(NullMarker), MemSink::new(), vec![Registration::new("counts", Counts(seen.clone()))]).unwrap();
    let (tx, rx) = mpsc::channel(4);
    let host = tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(Effects::default()), 2, rx, opts(&n, Some(loop_sink.clone()), None)));
    tx.send(go("park1")).await.unwrap();
    let from = Instant::now();
    tokio::time::sleep(Duration::from_secs(2)).await; // clock-safe: the observation window
    kicks.gaps_within(from, Instant::now(), WINDOW).unwrap();
    let overrun = |m: &MemSink| m.records().into_iter().find(|r| r.get("unit") == Some(&Value::from("loop.turn")) && r.get("outcome") == Some(&Value::from("overrun")));
    assert!(until(Duration::from_secs(6), || overrun(&loop_sink).is_some()).await, "a turn past its budget leaves an overrun record");
    let rec = overrun(&loop_sink).unwrap();
    assert!(matches!(rec.get("turn.elapsed_ms"), Some(Value::Int(ms)) if *ms >= 5000), "{rec:?}");
    assert_eq!(rec.get("handler.in_flight"), Some(&Value::Bool(false)), "the stall was in the journal, not a handler");
    kicks.gaps_within(from, Instant::now(), WINDOW).unwrap();
    gate.open();
    assert!(until(Duration::from_secs(5), || seen.load(Ordering::SeqCst) == 1).await, "the stuck event is dispatched once the journal answers");
    assert!(until(Duration::from_secs(5), || loop_sink.records().iter().any(|r| r.get("unit") == Some(&Value::from("loop.turn")) && r.get("outcome") == Some(&Value::from("ok")))).await, "the completed long turn is recorded");
    tokio::time::sleep(Duration::from_millis(300)).await; // clock-safe: a second dispatch would land inside this
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    host.abort();
}

struct Wants(usize, u64);
impl Handler for Wants {
    fn interests(&self) -> &[EventKindTag] {
        &["go"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        (0..self.0).map(|i| Command::Effect(EffectSpec { name: format!("work{i}"), input: String::new(), deadline_ms: self.1 })).collect()
    }
}

struct Blocks {
    gate: Arc<Gate>,
    entered: Mutex<Vec<String>>,
}
impl EffectRunner for Blocks {
    fn run(&self, key: &str, _: &EffectSpec) -> std::result::Result<String, String> {
        self.entered.lock().unwrap().push(key.into());
        self.gate.wait();
        Ok(String::new())
    }
}

#[tokio::test]
async fn the_effect_pool_is_bounded_by_threads_not_by_waiters() {
    let d = testkit::TempDir::new("lnb-pool");
    let s = settings("", d.path());
    let journal = MemJournal::new();
    let engine = open_engine(&s, journal.clone(), Arc::new(WallClock), Arc::new(NullMarker), MemSink::new(), vec![Registration::new("wants", Wants(2, 100))]).unwrap();
    let runner = Arc::new(Blocks { gate: Gate::new(), entered: Mutex::new(vec![]) });
    let (tx, rx) = mpsc::channel(4);
    tx.send(go("e1")).await.unwrap();
    let host = tokio::spawn(serve_with(engine, Arc::new(WallClock), runner.clone(), 1, rx, ServeOpts::default()));
    assert!(until(Duration::from_secs(5), || count_of(&journal, "effect.timed_out") == 2).await, "both effects time out");
    tokio::time::sleep(Duration::from_millis(300)).await; // clock-safe: a late start of the second runner would land inside this
    assert_eq!(runner.entered.lock().unwrap().len(), 1, "the second runner is not entered while the first thread is alive");
    runner.gate.open();
    host.abort();
}

struct Stamped(Mutex<Vec<(Instant, Fields)>>);
impl WideSink for Stamped {
    fn emit(&self, f: &Fields) {
        self.0.lock().unwrap().push((Instant::now(), f.clone()));
    }
}

fn spills(s: &Stamped, what: &str) -> Vec<(Instant, Fields)> {
    s.0.lock().unwrap().iter().filter(|(_, f)| f.get("unit") == Some(&Value::from("offload.spill")) && f.get("what") == Some(&Value::from(what))).cloned().collect()
}

#[tokio::test]
async fn a_full_offload_queue_refuses_backs_off_and_the_turn_is_redriven_once() {
    let d = testkit::TempDir::new("lnb-spill-turn");
    let s = settings("", d.path());
    let (n, kicks) = notifier(&d);
    let pool = Pool::new(1, 1);
    let held = Gate::new();
    let (g, (started_tx, started_rx)) = (held.clone(), std::sync::mpsc::channel());
    let _running = pool
        .offload("fill", None, move || {
            started_tx.send(()).unwrap();
            g.wait()
        })
        .unwrap();
    started_rx.recv().unwrap();
    let g = held.clone();
    let _queued = pool.offload("fill", None, move || g.wait()).unwrap();
    let seen = Arc::new(AtomicUsize::new(0));
    let journal = MemJournal::new();
    let engine = open_engine(&s, journal.clone(), Arc::new(WallClock), Arc::new(NullMarker), MemSink::new(), vec![Registration::new("counts", Counts(seen.clone()))]).unwrap();
    let sink = Arc::new(Stamped(Mutex::new(vec![])));
    let (tx, rx) = mpsc::channel(4);
    tx.send(go("e1")).await.unwrap();
    let before = offload::spilled_total("engine.turn");
    let from = Instant::now();
    let host = tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(Effects::default()), 2, rx, opts(&n, Some(sink.clone()), Some(pool))));
    tokio::time::sleep(Duration::from_millis(1500)).await; // clock-safe: lets the refused turn back off
    kicks.gaps_within(from, Instant::now(), WINDOW).unwrap();
    let sp = spills(&sink, "engine.turn");
    assert!(sp.len() >= 3 && sp.len() <= 8, "a saturated pool is re-offered with a backoff, not in a spin: {} offers", sp.len());
    let gaps: Vec<_> = sp.windows(2).map(|w| w[1].0.duration_since(w[0].0)).collect();
    assert!(gaps.windows(2).take(3).all(|g| g[1] > g[0]), "the backoff grows while the queue stays full: {gaps:?}");
    let rec = &sp[0].1;
    assert_eq!((rec.get("offload.cap"), rec.get("offload.depth")), (Some(&Value::Int(1)), Some(&Value::Int(1))), "{rec:?}");
    assert!(matches!(rec.get("offload.spilled_total"), Some(Value::Int(t)) if *t as u64 > before));
    assert_eq!(seen.load(Ordering::SeqCst), 0, "the refused work stays undone");
    assert_eq!(count_of(&journal, "go"), 0);
    held.open();
    assert!(until(Duration::from_secs(5), || seen.load(Ordering::SeqCst) == 1).await, "the work completes after the queue drains");
    tokio::time::sleep(Duration::from_millis(300)).await; // clock-safe: a redundant dispatch would land inside this
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert_eq!(count_of(&journal, "go"), 1);
    host.abort();
}

#[tokio::test]
async fn a_refused_effect_stays_pending_and_runs_once_after_the_queue_drains() {
    let d = testkit::TempDir::new("lnb-spill-effect");
    let s = settings("", d.path());
    let pool = Pool::new(1, 1);
    let journal = MemJournal::new();
    let engine = open_engine(&s, journal.clone(), Arc::new(WallClock), Arc::new(NullMarker), MemSink::new(), vec![Registration::new("wants", Wants(3, 60_000))]).unwrap();
    let runner = Arc::new(Blocks { gate: Gate::new(), entered: Mutex::new(vec![]) });
    let sink = Arc::new(Stamped(Mutex::new(vec![])));
    let (tx, rx) = mpsc::channel(4);
    tx.send(go("e1")).await.unwrap();
    let o = ServeOpts { sink: Some(sink.clone()), pool: Some(pool), ..ServeOpts::default() };
    let host = tokio::spawn(serve_with(engine, Arc::new(WallClock), runner.clone(), 3, rx, o));
    assert!(until(Duration::from_secs(5), || !spills(&sink, "effect").is_empty()).await, "a refused effect leaves a spill record");
    assert!(spills(&sink, "effect").iter().all(|(_, f)| f.contains_key("offload.cap") && f.contains_key("offload.depth")));
    runner.gate.open();
    assert!(until(Duration::from_secs(10), || count_of(&journal, "effect.succeeded") == 3).await, "every effect completes");
    let entered = runner.entered.lock().unwrap().clone();
    assert_eq!(entered.iter().collect::<BTreeSet<_>>().len(), 3, "each effect ran: {entered:?}");
    assert_eq!(entered.len(), 3, "and exactly once");
    host.abort();
}
