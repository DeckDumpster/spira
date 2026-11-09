//! `Engine::status`: what each task last did and has waiting, and how full each effector class is.

use std::sync::Arc;

use runtime::engine::{LastOutcome, StatusBoard};
use runtime::journal::MemJournal;
use runtime::ports::VirtualClock;
use runtime::*;

/// `arm` schedules a tick at 100; each tick emits its carried state and schedules the next.
struct Ticker;
impl Handler for Ticker {
    fn interests(&self) -> &[EventKindTag] {
        &["arm", "tick"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        let next = if ev.kind.tag == "arm" { 100 } else { ev.at + 1000 };
        let mut cmds = vec![Command::Schedule { at: next, kind: EventKind::new("tick", "") }];
        if ev.kind.tag == "tick" {
            cmds.push(Command::Emit(EventKind::new("ticked", "seven")));
        }
        cmds
    }
}

struct Builder;
impl Handler for Builder {
    fn interests(&self) -> &[EventKindTag] {
        &["build"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        vec![Command::Effect(EffectSpec { name: ev.kind.body.clone(), input: String::new(), deadline_ms: 10_000, key: ev.kind.body.clone() })]
    }
}

fn engine(j: Arc<MemJournal>, clock: &VirtualClock) -> Engine {
    let regs = vec![Registration::new("ticker", Ticker), Registration::new("builder", Builder).with_mode(Mode::Act)];
    Engine::open(j, Arc::new(clock.clone()), regs, 1).unwrap().with_classifier(Arc::new(|s: &EffectSpec| if s.name == "suite" { "container".into() } else { "host".into() }))
}

#[test]
fn status_reflects_a_fired_tick() {
    let (j, clock) = (MemJournal::new(), VirtualClock::new(0));
    let mut e = engine(j, &clock);
    e.submit("a1", "t", EventKind::new("arm", "")).unwrap();
    e.run_until_idle().unwrap();
    let before = e.status();
    let t = before.tasks.iter().find(|t| t.name == "ticker").unwrap();
    assert_eq!(t.next_due_at, Some(100), "the armed tick is due");
    assert_eq!(t.mode, Mode::Act);

    clock.advance_to(130);
    e.run_until_idle().unwrap();
    let s = e.status();
    let t = s.tasks.iter().find(|t| t.name == "ticker").unwrap();
    assert_eq!((t.last_due_at, t.last_fired_at, t.lag_ms), (Some(100), Some(130), Some(30)), "fired 30 ms after it was due");
    assert_eq!(t.next_due_at, Some(1100));
    let state = t.state.as_ref().expect("the tick carried state");
    assert_eq!((state.tag.as_str(), state.body_len), ("ticked", 5));
    let b = s.tasks.iter().find(|t| t.name == "builder").unwrap();
    assert_eq!((b.last_fired_at, b.next_due_at), (None, None), "a task nothing woke shows nothing");
    assert_eq!(s.at, 130);
}

#[test]
fn status_reflects_an_in_flight_effect_its_outcome_and_a_deferral() {
    let (j, clock) = (MemJournal::new(), VirtualClock::new(0));
    let board = Arc::new(StatusBoard::default());
    let mut e = engine(j, &clock).with_status_board(board.clone());
    e.submit("b1", "b", EventKind::new("build", "suite")).unwrap();
    e.submit("b2", "b", EventKind::new("build", "mail")).unwrap();
    e.run_until_idle().unwrap();
    let s = board.get().expect("a turn publishes the status");
    let b = s.tasks.iter().find(|t| t.name == "builder").unwrap();
    assert_eq!(b.in_flight.len(), 2, "both effects are in flight: {b:?}");
    let depth = |s: &runtime::engine::SchedulerStatus, c: &str| s.classes.iter().find(|k| k.class == c).map(|k| (k.depth, k.refusals));
    assert_eq!(depth(&s, "container"), Some((1, 0)));
    assert_eq!(depth(&s, "host"), Some((1, 0)));

    let pending = e.pending_effects();
    let suite = pending.iter().find(|p| p.spec.name == "suite").unwrap().key.clone();
    let mail = pending.iter().find(|p| p.spec.name == "mail").unwrap().key.clone();
    e.note_deferred(&suite, "container");
    e.run_until_idle().unwrap();
    let s = board.get().unwrap();
    let b = s.tasks.iter().find(|t| t.name == "builder").unwrap();
    assert_eq!(b.last_outcome, Some(LastOutcome::Deferred));
    assert_eq!(depth(&s, "container"), Some((1, 1)), "a deferred effect is still in flight and its refusal counts");

    e.complete(&mail, Outcome::Failed("no".into())).unwrap();
    e.run_until_idle().unwrap();
    let s = board.get().unwrap();
    let b = s.tasks.iter().find(|t| t.name == "builder").unwrap();
    assert_eq!(b.last_outcome, Some(LastOutcome::Failed));
    assert_eq!(b.in_flight, vec![suite.clone()]);
    assert_eq!(depth(&s, "host"), Some((0, 0)));
}

#[test]
fn status_shows_the_halt() {
    let (j, clock) = (MemJournal::new(), VirtualClock::new(0));
    let mut e = engine(j, &clock);
    e.submit("h1", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "")).unwrap();
    e.submit("b1", "b", EventKind::new("build", "mail")).unwrap();
    e.run_until_idle().unwrap();
    let s = e.status();
    assert!(s.halted);
    assert_eq!(s.held, 1);
    e.submit("r1", HALT_STREAM, EventKind::new(SCHEDULER_RESUMED, "")).unwrap();
    e.run_until_idle().unwrap();
    let s = e.status();
    assert!(!s.halted);
    assert_eq!(s.held, 0);
}
