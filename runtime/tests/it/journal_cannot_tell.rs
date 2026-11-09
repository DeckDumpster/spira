//! A journal that cannot tell (a slow or lost reply from spira-lc) is waited out and retried
//! inside `serve_with`; the host does not return an error, so the process does not exit.

use std::sync::Arc;
use std::time::Duration;

use runtime::host::{serve_with, EffectRunner, ServeOpts, Submit};
use runtime::journal::{FaultyJournal, Journal};
use runtime::pools::PoolCaps;
use runtime::ports::WallClock;
use runtime::{Command, Ctx, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, MemSink, Projection, Registration};
use tokio::sync::mpsc;

struct Echo;
impl Handler for Echo {
    fn interests(&self) -> &[EventKindTag] {
        &["ping"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        vec![Command::Emit(EventKind::new("pong", ""))]
    }
}

struct NoEffects;
impl EffectRunner for NoEffects {
    fn run(&self, _: &str, _: &runtime::EffectSpec) -> Result<String, String> {
        Ok(String::new())
    }
}

fn pongs(j: &dyn Journal) -> usize {
    runtime::conformance::trace(j).into_iter().filter(|t| t.2 == "pong").count()
}

#[tokio::test]
async fn a_journal_read_that_cannot_tell_is_retried_not_returned_as_an_exit() {
    let mem: Arc<dyn Journal> = MemJournal::new();
    let faulty = FaultyJournal::new(mem.clone());
    let engine = Engine::open(faulty.clone(), Arc::new(WallClock), vec![Registration::new("echo", Echo)], 1).unwrap();
    let sink = MemSink::new();
    let opts = ServeOpts { sink: Some(sink.clone()), ..ServeOpts::default() };
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(NoEffects), PoolCaps::default(), rx, opts));
    faulty.cannot_tell_next("commit");
    tx.send(Submit::new("p1", "s", EventKind::new("ping", ""))).await.unwrap();
    for _ in 0..300 {
        if pongs(&*mem) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    assert!(!serving.is_finished(), "serve returned on a journal that could not tell");
    assert_eq!(pongs(&*mem), 1, "the turn ran again once the journal answered");
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), serving).await.expect("the host drains").unwrap().expect("a clean stop");
    let retries: Vec<_> = sink.records().into_iter().filter(|r| r.get("unit").is_some_and(|u| *u == "journal.retry".into())).collect();
    assert_eq!(retries.len(), 1, "each retry is counted: {retries:?}");
}

#[test]
fn a_dropped_spira_lc_reply_is_a_cannot_tell_and_a_missing_socket_is_too() {
    let dir = testkit::TempDir::new("cannot-tell");
    let path = dir.path().join("lc.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let hang_up = std::thread::spawn(move || drop(listener.accept()));
    let lost = runtime::socket::lc_call(&path, &["journal", "state"]).unwrap_err();
    hang_up.join().unwrap();
    assert!(lost.is_cannot_tell(), "{lost}");
    let down = runtime::socket::lc_call(&dir.path().join("absent.sock"), &["journal", "state"]).unwrap_err();
    assert!(down.is_cannot_tell(), "{down}");
}

#[test]
fn a_spira_lc_reply_that_says_cannot_tell_is_a_cannot_tell_and_survives_the_wire_both_ways() {
    let dir = testkit::TempDir::new("cannot-tell-reply");
    let path = dir.path().join("lc.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let service = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        runtime::socket::answer_one(stream, &|_| Err(runtime::RuntimeError::cannot_tell("cannot tell: deadline"))).unwrap();
    });
    let err = runtime::socket::lc_call(&path, &["journal", "state"]).unwrap_err();
    service.join().unwrap();
    assert!(err.is_cannot_tell(), "a journal call spira-lc could not answer in time must be retried, not returned: {err:?}");
    assert!(err.0.contains("deadline"), "{err}");
}

#[tokio::test]
async fn a_spira_lc_deadline_on_the_socket_journal_is_retried_by_serve_not_returned() {
    let (store, path, _dir) = crate::journal_calls::serve();
    let journal = runtime::socket::SocketJournal::new(path, spira_otel::vlogs::Shipper::start("http://127.0.0.1:9"));
    let engine = Engine::open(Arc::new(journal), Arc::new(WallClock), vec![Registration::new("echo", Echo)], 1).unwrap();
    let (tx, rx) = mpsc::channel::<Submit>(4);
    let serving = tokio::spawn(serve_with(engine, Arc::new(WallClock), Arc::new(NoEffects), PoolCaps::default(), rx, ServeOpts::default()));
    store.deadline_next.store(true, std::sync::atomic::Ordering::SeqCst);
    tx.send(Submit::new("p1", "s", EventKind::new("ping", ""))).await.unwrap();
    for _ in 0..300 {
        if !store.mem.commits().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await; // clock-safe: polling a real host
    }
    assert!(!serving.is_finished(), "serve returned on a store call that hit spira-lc's deadline");
    assert_eq!(store.mem.commits().len(), 1, "the turn was committed once the store answered");
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), serving).await.expect("the host drains").unwrap().expect("a clean stop");
}
