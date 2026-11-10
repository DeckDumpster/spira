//! Timers, probes and notes live in the engine and never reach the journal; a fact a handler
//! must not lose is emitted.

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
                Command::Probe(EffectSpec { name: "look".into(), input: String::new(), deadline_ms: 500, key: "look".into() }),
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
    assert!(journal.log().is_empty());
    assert!(journal.commits().is_empty() && journal.boot().unwrap().schedules.is_empty());
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

struct Noter;

impl Handler for Noter {
    fn interests(&self) -> &[EventKindTag] {
        &["tick", EFFECT_SUCCEEDED]
    }

    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        match ev.kind.tag.as_str() {
            "tick" => vec![
                Command::Probe(EffectSpec { name: "look".into(), input: String::new(), deadline_ms: 500, key: "look".into() }),
                Command::Timer { at: ev.at + 1000, kind: EventKind::new("tick", "") },
            ],
            _ => vec![Command::Note(EventKind::new("noted", ""))],
        }
    }
}

#[test]
fn a_probe_still_waiting_keeps_its_request_in_the_projection() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let mut e = Engine::open(journal.clone(), Arc::new(clock.clone()), vec![Registration::new("noter", Noter)], 0).unwrap();
    e.start("s", EventKind::new("tick", ""));
    e.run_until_idle().unwrap();
    let waiting = e.pending_effects().remove(0).key;
    for i in 0..(runtime::engine::LOCAL_KEEP * 2) {
        e.start("s", EventKind::new("other", &i.to_string()));
        e.run_until_idle().unwrap();
    }
    assert!(e.projection().events(&StreamKey("s".into())).iter().any(|ev| ev.id.0 == waiting), "the unanswered request was evicted");
}

struct Keeper {
    read: Arc<Mutex<Vec<String>>>,
}

impl Handler for Keeper {
    fn interests(&self) -> &[EventKindTag] {
        &["boot", "chatter"]
    }

    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        match ev.kind.tag.as_str() {
            "boot" => vec![Command::Note(EventKind::new("state", "carried"))],
            _ => {
                let state = view.events(&ev.stream).iter().rev().find(|e| e.kind.tag == "state").map(|e| e.kind.body.clone());
                self.read.lock().unwrap().push(state.unwrap_or_default());
                vec![]
            }
        }
    }
}

#[test]
fn the_newest_note_of_a_kind_outlives_any_amount_of_later_local_chatter() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let read = Arc::new(Mutex::new(vec![]));
    let mut e = Engine::open(journal, Arc::new(clock), vec![Registration::new("keeper", Keeper { read: read.clone() })], 0).unwrap();
    e.start("s", EventKind::new("boot", ""));
    e.run_until_idle().unwrap();
    for i in 0..(runtime::engine::LOCAL_KEEP * 3) {
        e.start("s", EventKind::new("chatter", &i.to_string()));
    }
    e.run_until_idle().unwrap();
    let read = read.lock().unwrap();
    assert_eq!(read.len(), runtime::engine::LOCAL_KEEP * 3);
    assert!(read.iter().all(|s| s == "carried"), "a handler's carried state was evicted");
}

struct Emitter {
    read: Arc<Mutex<Vec<String>>>,
}

impl Handler for Emitter {
    fn interests(&self) -> &[EventKindTag] {
        &["boot", "again"]
    }

    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "boot" {
            return vec![Command::Emit(EventKind::new("state", "journaled"))];
        }
        let state = view.events(&ev.stream).iter().rev().find(|e| e.kind.tag == "state").map(|e| e.kind.body.clone());
        self.read.lock().unwrap().push(state.unwrap_or_default());
        vec![]
    }
}

#[test]
fn a_restarted_engine_shows_a_memory_only_first_event_the_state_the_last_run_journaled() {
    let (journal, clock) = (MemJournal::new(), VirtualClock::new(0));
    let read = Arc::new(Mutex::new(vec![]));
    let open = || Engine::open(journal.clone(), Arc::new(clock.clone()), vec![Registration::new("emitter", Emitter { read: read.clone() })], 0).unwrap();
    let mut first = open();
    first.start("s", EventKind::new("boot", ""));
    first.run_until_idle().unwrap();
    let mut second = open();
    second.start("s", EventKind::new("again", ""));
    second.run_until_idle().unwrap();
    assert_eq!(*read.lock().unwrap(), ["journaled"]);
}
