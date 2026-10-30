//! Timers, probes and notes live in the engine and never reach the journal; a fact a handler
//! must not lose is emitted, and the journal is still read for what other writers append.

use std::sync::{Arc, Mutex};

use runtime::ports::VirtualClock;
use runtime::*;

struct Poller {
    seen: Arc<Mutex<Vec<String>>>,
}

impl Handler for Poller {
    fn interests(&self) -> &[EventKindTag] {
        &["tick", "foreign", EFFECT_SUCCEEDED]
    }

    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        self.seen.lock().unwrap().push(ev.kind.tag.clone());
        match ev.kind.tag.as_str() {
            "tick" => vec![
                Command::Probe(EffectSpec { name: "look".into(), input: String::new(), deadline_ms: 500 }),
                Command::Timer { at: ev.at + 1000, kind: EventKind::new("tick", "") },
            ],
            EFFECT_SUCCEEDED => vec![Command::Note(EventKind::new("noted", ""))],
            _ => vec![],
        }
    }
}

fn engine(journal: &Arc<MemJournal>, clock: &VirtualClock) -> (Engine, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(vec![]));
    let regs = vec![Registration::new("poller", Poller { seen: seen.clone() })];
    (Engine::open(journal.clone(), Arc::new(clock.clone()), regs, 0).unwrap(), seen)
}

fn answer_probes(e: &mut Engine) {
    for p in e.pending_effects() {
        e.complete(&p.key, Outcome::Succeeded(String::new())).unwrap();
    }
    e.run_until_idle().unwrap();
}

#[test]
fn a_probe_and_its_outcome_a_note_and_a_timer_leave_the_journal_empty() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let (mut e, seen) = engine(&journal, &clock);
    e.start("s", EventKind::new("tick", ""));
    for _ in 0..3 {
        e.run_until_idle().unwrap();
        answer_probes(&mut e);
        clock.advance(1000);
    }
    assert_eq!(seen.lock().unwrap().iter().filter(|t| *t == "tick").count(), 3, "the timer re-arms itself");
    assert!(seen.lock().unwrap().iter().any(|t| t == EFFECT_SUCCEEDED), "the probe's outcome reaches the handler");
    assert!(journal.read_from(0, 100).unwrap().is_empty());
    assert!(journal.pending_schedules().unwrap().is_empty());
}

#[test]
fn arming_a_timer_again_replaces_the_one_waiting_and_a_restart_forgets_it() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let (mut e, seen) = engine(&journal, &clock);
    e.start("s", EventKind::new("tick", ""));
    e.start("s", EventKind::new("tick", ""));
    e.run_until_idle().unwrap();
    clock.advance(1000);
    e.run_until_idle().unwrap();
    assert_eq!(seen.lock().unwrap().iter().filter(|t| *t == "tick").count(), 3, "two starts, then one firing: the chains merged");
    let (mut again, seen) = engine(&journal, &clock);
    clock.advance(10_000);
    again.run_until_idle().unwrap();
    assert!(seen.lock().unwrap().is_empty() && again.next_timer_at().is_none(), "a new process starts with no timers");
}

#[test]
fn another_writers_event_is_read_within_the_poll_interval_and_not_before() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let (e, seen) = engine(&journal, &clock);
    let mut e = e.with_foreign_poll(30_000);
    e.run_until_idle().unwrap();
    let foreign = Event { id: EventId("f1".into()), stream: StreamKey("other".into()), kind: EventKind::new("foreign", ""), at: 0, cause: None, correlation: CorrelationId("f1".into()) };
    journal.append(&foreign).unwrap();
    clock.advance(29_000);
    e.run_until_idle().unwrap();
    assert!(seen.lock().unwrap().is_empty(), "not read before the interval");
    clock.advance(1_000);
    e.run_until_idle().unwrap();
    assert_eq!(*seen.lock().unwrap(), ["foreign"]);
}

#[test]
fn an_event_this_engine_appended_is_read_at_once_whatever_the_poll_interval() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let (e, seen) = engine(&journal, &clock);
    let mut e = e.with_foreign_poll(30_000);
    e.run_until_idle().unwrap();
    e.submit("mine", "s", EventKind::new("foreign", "")).unwrap();
    e.run_until_idle().unwrap();
    assert_eq!(*seen.lock().unwrap(), ["foreign"]);
}

struct Panicker;

impl Handler for Panicker {
    fn interests(&self) -> &[EventKindTag] {
        &["tick"]
    }

    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        panic!("a handler that always panics");
    }
}

#[test]
fn a_handler_that_panics_on_a_memory_only_event_does_not_wedge_the_pump() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let regs = vec![Registration::new("panicker", Panicker)];
    let mut e = Engine::open(journal.clone(), Arc::new(clock.clone()), regs, 0).unwrap();
    e.start("s", EventKind::new("tick", ""));
    for _ in 0..20 {
        if e.run_until_idle().is_ok() {
            return;
        }
    }
    panic!("the pump never went idle: the failing local event is retried forever");
}
