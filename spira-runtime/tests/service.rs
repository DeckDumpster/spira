//! The service wiring against its real parts: the heartbeat fires and pings a stub URL, wide
//! events reach the collector's file, and a hung loop stops kicking the watchdog.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use runtime::heartbeat::{Heartbeat, NAME};
use runtime::host::{serve_with, ServeOpts, Submit};
use runtime::ports::WallClock;
use runtime::*;
use spira_runtime::notify::Notifier;
use spira_runtime::sink::OtlpSink;
use spira_runtime::{open_engine, registrations, Effects, Settings};
use tokio::sync::mpsc;

fn settings(url: &str, run: &std::path::Path) -> Settings {
    let get = |k: &str| -> std::result::Result<String, String> {
        Ok(match k {
            "SPIRA_RUNTIME_HEARTBEAT_URL" => url.to_string(),
            "SPIRA_RUNTIME_HEARTBEAT_SECS" => "1".into(),
            "SPIRA_RUNTIME_STALL_LIMIT" => "2".into(),
            "SPIRA_RUN" => run.display().to_string(),
            "SPIRA_LC_SOCKET" => "/unused".into(),
            "SPIRA_RUNTIME_INBOX_KEEPER_SECS" | "SPIRA_CONCIERGE_INBOX_STALL" | "SPIRA_CONCIERGE_INBOX_BACKOFF" | "SPIRA_RUNTIME_ROUND_DUTY_SECS" | "SPIRA_ROUND_MIN" | "SPIRA_ROUND_MAX_OPEN" | "SPIRA_ROUND_DUE_REMIND" | "SPIRA_ROUND_IDLE_CUT" => "5".into(),
            "SPIRA_QUEUE_DIR" | "SPIRA_REPO_MAP" | "SPIRA_CONCIERGE_INBOX" => "/unused".into(),
            "SPIRA_HOME_REPO" => "spira".into(),
            _ => return Err(k.to_string()),
        })
    };
    Settings::from_lookup(&get).unwrap()
}

fn stub_endpoint() -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/ping", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let mut c = c;
            let mut buf = [0u8; 512];
            let _ = c.read(&mut buf);
            h.fetch_add(1, Ordering::SeqCst);
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (url, hits)
}

#[test]
fn the_heartbeat_registration_is_part_of_the_service() {
    let d = testkit::TempDir::new("svc-reg");
    let names: Vec<_> = registrations(&settings("", d.path())).into_iter().map(|r| r.name).collect();
    assert_eq!(names, [NAME, "round-duty", "inbox-keeper"], "removing a registration removes its schedule");
}

#[tokio::test]
async fn the_heartbeat_schedule_fires_and_pings_after_the_journal_advances() {
    let d = testkit::TempDir::new("svc-hb");
    let (url, hits) = stub_endpoint();
    let s = settings(&url, d.path());
    let journal: Arc<dyn Journal> = MemJournal::new();
    let sink = Arc::new(OtlpSink(spira_otel::logs::LogSink::new(s.otel_dir(), "spira-runtime")));
    let mut engine = open_engine(&s, journal.clone(), Arc::new(WallClock), Arc::new(NullMarker), sink, Arc::new(|| {}), vec![]).unwrap();
    let (id, stream, kind) = Heartbeat::boot();
    engine.submit(id, stream, kind).unwrap();
    let (tx, rx) = mpsc::channel(4);
    tx.send(Submit { id: "other".into(), stream: "elsewhere".into(), kind: EventKind::new("anything", "") }).await.unwrap();
    let host = tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(Effects { heartbeat_url: url, ..Effects::default() }), 2, rx, ServeOpts::default()));
    let end = Instant::now() + Duration::from_secs(8);
    while hits.load(Ordering::SeqCst) == 0 && Instant::now() < end { // clock-safe: cap on the wait for the first heartbeat ping
        tokio::time::sleep(Duration::from_millis(50)).await; // clock-safe: polls for the heartbeat ping counter
    }
    assert!(hits.load(Ordering::SeqCst) >= 1, "the stub URL was never pinged");
    drop(tx);
    host.abort();
    let recs = spira_otel::logs::read_all(&s.otel_dir(), "spira-runtime");
    let has = |k: &str, v: &str| recs.iter().any(|r| r.iter().any(|(n, val)| n == k && val["stringValue"] == v));
    assert!(has("handler", "heartbeat") && has("unit", "dispatch"), "the heartbeat dispatch is a wide event");
    assert!(has("effect.kind", runtime::heartbeat::PING), "the ping effect is a wide event");
    assert!(recs.iter().all(|r| r.iter().any(|(n, _)| n == "config.hash")), "every record carries the config hash");
}

struct Hang;
impl Handler for Hang {
    fn interests(&self) -> &[EventKindTag] {
        &["hang"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        loop {
            std::thread::park();
        }
    }
}

#[test]
fn a_hung_loop_stops_kicking_the_watchdog() {
    let d = testkit::TempDir::new("svc-wd");
    let sock = d.path().join("notify.sock");
    let rx = UnixDatagram::bind(&sock).unwrap();
    let n = Arc::new(Notifier::new(sock.to_str(), Some(600_000)).unwrap());
    let s = settings("", d.path());
    let kick: Arc<dyn Fn() + Send + Sync> = {
        let n = n.clone();
        Arc::new(move || n.kick())
    };
    let mut e = open_engine(&s, MemJournal::new(), Arc::new(WallClock), Arc::new(NullMarker), Arc::new(NullSink), kick, vec![Registration::new("hang", Hang)]).unwrap();
    e.submit("ok1", "a", EventKind::new("anything", "")).unwrap();
    e.submit("h1", "b", EventKind::new("hang", "")).unwrap();
    std::thread::spawn(move || {
        let _ = e.run_until_idle();
    });
    rx.set_read_timeout(Some(Duration::from_millis(800))).unwrap();
    let mut buf = [0u8; 16];
    let n1 = rx.recv(&mut buf).expect("the loop kicked before it hung");
    assert_eq!(&buf[..n1], b"WATCHDOG=1");
    assert!(rx.recv(&mut buf).is_err(), "no kick comes from a loop stuck in a handler");
}

/// A journal that refuses, as the store does, any append whose id starts with `bad`.
struct Refusing(Arc<MemJournal>);

impl Journal for Refusing {
    fn append(&self, ev: &Event) -> runtime::Result<Offset> {
        if ev.id.0.starts_with("sched/bad") {
            return Err(RuntimeError::refused(format!("journal: append refused: event_id is {} chars (max 255); id={}", ev.id.0.len(), ev.id.0)));
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

fn due_row(id: &str) -> ScheduleRow {
    ScheduleRow {
        id: id.into(),
        at: 0,
        stream: StreamKey(runtime::heartbeat::STREAM.into()),
        kind: EventKind::new(runtime::heartbeat::HEARTBEAT, "0"),
        cause: None,
        correlation: CorrelationId("heartbeat/boot".into()),
    }
}

async fn serve_over(d: &testkit::TempDir, journal: Arc<dyn Journal>, sink: Arc<MemSink>, armed: impl FnOnce()) -> tokio::task::JoinHandle<runtime::Result<()>> {
    let s = settings("", d.path());
    let mut engine = open_engine(&s, journal, Arc::new(WallClock), Arc::new(NullMarker), sink, Arc::new(|| {}), vec![]).unwrap();
    let (id, stream, kind) = Heartbeat::boot();
    engine.submit(id, stream, kind).unwrap();
    armed();
    let (tx, rx) = mpsc::channel(4);
    std::mem::forget(tx);
    tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(Effects::default()), 2, rx, ServeOpts::default()))
}

#[tokio::test]
async fn a_due_row_the_journal_refuses_is_quarantined_and_the_service_keeps_serving() {
    let d = testkit::TempDir::new("svc-refused");
    let mem = MemJournal::new();
    let id = format!("bad{}", "x".repeat(252));
    mem.schedule(&due_row(&id)).unwrap();
    let journal: Arc<dyn Journal> = Arc::new(Refusing(mem.clone()));
    let sink = MemSink::new();
    let host = serve_over(&d, journal, sink.clone(), || {}).await;
    tokio::time::sleep(Duration::from_millis(400)).await; // clock-safe: lets the first loop turns run before asserting the service is still up
    assert!(!host.is_finished(), "a refusal must not end the service: {:?}", host.is_finished());
    let p = mem.poisoned(&StreamKey("runtime.heartbeat".into())).unwrap().expect("the stream is quarantined");
    assert!(p.event.0.starts_with("sched/bad") && p.message.contains("refused"), "{p:?}");
    assert!(mem.pending_schedules().unwrap().is_empty(), "the row is marked fired");
    assert!(sink.records().iter().any(|r| r.iter().any(|(k, v)| k == "unit" && *v == Value::from("quarantine"))), "a quarantine wide event is emitted");
    host.abort();
}

#[tokio::test]
async fn a_fault_from_the_journal_still_ends_the_service() {
    let d = testkit::TempDir::new("svc-fault");
    let mem = MemJournal::new();
    mem.schedule(&due_row("r")).unwrap();
    let faulty = FaultyJournal::new(mem);
    let host = serve_over(&d, faulty.clone(), MemSink::new(), || faulty.fail_next("append")).await;
    let r = tokio::time::timeout(Duration::from_secs(5), host).await.expect("serve returns").unwrap(); // clock-safe: cap on the wait for serve to fail
    assert!(r.is_err(), "a cannot-tell fault is fatal");
}
