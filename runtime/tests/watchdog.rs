//! The in-flight marker: a handler call that never returns is a stall; the first restart
//! dispatches it again, the second quarantines its stream, and other streams carry on.

use std::sync::Arc;

use runtime::heartbeat::{Heartbeat, HEARTBEAT, PING, STREAM};
use runtime::ports::VirtualClock;
use runtime::*;

struct Blocker;
impl Handler for Blocker {
    fn interests(&self) -> &[EventKindTag] {
        &["block"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        loop {
            std::thread::park();
        }
    }
}

struct Fine;
impl Handler for Fine {
    fn interests(&self) -> &[EventKindTag] {
        &["fine"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        vec![Command::Emit(EventKind::new("fine.done", &ev.id.0))]
    }
}

fn regs() -> Vec<Registration> {
    vec![Registration::new("blocker", Blocker), Registration::new("fine", Fine)]
}

fn boot(journal: &Arc<dyn Journal>, marker: &Arc<dyn Marker>, sink: &Arc<MemSink>) -> Engine {
    Engine::open(journal.clone(), Arc::new(VirtualClock::new(10)), regs(), 3)
        .unwrap()
        .with_sink(sink.clone())
        .with_marker(marker.clone())
        .unwrap()
}

/// Runs the loop on a thread that never comes back, as a hung process does; returns once the
/// marker shows the handler is inside its call.
fn run_until_hung(mut e: Engine, marker_path: &std::path::Path) {
    std::thread::spawn(move || {
        let _ = e.run_until_idle();
    });
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker_path.exists() {
        assert!(std::time::Instant::now() < end, "the handler never started"); // clock-safe: cap on the wait for the marker file
        std::thread::sleep(std::time::Duration::from_millis(2)); // clock-safe: polls for the marker file the blocked handler writes
    }
}

#[test]
fn a_stalled_handler_is_restarted_once_then_its_stream_is_quarantined_and_others_continue() {
    let dir = testkit::TempDir::new("runtime-watchdog");
    let path = dir.path().join("inflight.json");
    let (journal, marker, sink): (Arc<dyn Journal>, Arc<dyn Marker>, _) = (MemJournal::new(), Arc::new(FileMarker(path.clone())), MemSink::new());

    let mut first = boot(&journal, &marker, &sink);
    first.submit("b1", "bad", EventKind::new("block", "")).unwrap();
    first.submit("f1", "good", EventKind::new("fine", "")).unwrap();
    run_until_hung(first, &path);
    assert!(journal.poisoned(&StreamKey("bad".into())).unwrap().is_none());

    let second = boot(&journal, &marker, &sink);
    assert!(sink.records().iter().any(|r| r.get("unit") == Some(&Value::Str("watchdog.restart".into()))), "a restart is a wide event");
    run_until_hung(second, &path);
    assert!(journal.poisoned(&StreamKey("bad".into())).unwrap().is_none(), "one stall is a restart, not a quarantine");

    let mut third = boot(&journal, &marker, &sink);
    third.run_until_idle().unwrap();
    let p = journal.poisoned(&StreamKey("bad".into())).unwrap().expect("the second stall quarantines the stream");
    assert_eq!(p.event.0, "b1");
    assert!(p.message.contains("stalled 2 times"), "{}", p.message);
    assert!(!path.exists(), "no marker left once nothing is in flight");

    third.submit("f2", "good", EventKind::new("fine", "")).unwrap();
    third.submit("b2", "bad", EventKind::new("block", "")).unwrap();
    third.run_until_idle().unwrap();
    let done: Vec<_> = journal.read_from(0, 100).unwrap().into_iter().filter(|(_, e)| e.kind.tag == "fine.done").map(|(_, e)| e.stream.0).collect();
    assert_eq!(done, ["good", "good"], "the other stream kept going, the poisoned one is skipped");
    let q: Vec<_> = sink.records().into_iter().filter(|r| r.get("unit") == Some(&Value::Str("quarantine".into()))).collect();
    assert_eq!(q.len(), 1);
}

fn heartbeat_engine(journal: &Arc<dyn Journal>, clock: &VirtualClock) -> Engine {
    Engine::open(journal.clone(), Arc::new(clock.clone()), vec![Heartbeat { interval_ms: 1000, ping_deadline_ms: 500 }.registration()], 1).unwrap()
}

/// Answers every ping the engine has asked for and returns how many there were.
fn pings(e: &mut Engine) -> usize {
    let asked: Vec<_> = e.pending_effects().into_iter().filter(|p| p.spec.name == PING).collect();
    for p in &asked {
        e.complete(&p.key, Outcome::Succeeded(String::new())).unwrap();
    }
    e.run_until_idle().unwrap();
    asked.len()
}

#[test]
fn the_heartbeat_pings_only_after_the_engine_has_seen_other_events() {
    let (journal, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let mut e = heartbeat_engine(&journal, &clock);
    let (_, stream, kind) = Heartbeat::boot();
    e.start(stream, kind);
    e.run_until_idle().unwrap();
    assert_eq!(pings(&mut e), 0, "nothing else has happened: no ping");

    e.submit("other", "elsewhere", EventKind::new("anything", "")).unwrap();
    clock.advance(1000);
    e.run_until_idle().unwrap();
    assert_eq!(pings(&mut e), 1, "a foreign event advanced the journal");

    clock.advance(1000);
    e.run_until_idle().unwrap();
    assert_eq!(pings(&mut e), 0, "its own ping is not progress");
    assert_eq!(journal.read_from(0, 1000).unwrap().len(), 1, "an idle heartbeat writes nothing to the journal");
    assert_eq!(STREAM, "runtime.heartbeat");
    assert_eq!(HEARTBEAT, "heartbeat");
}
