//! The halt gate: while the scheduler is halted every task's writes are held in order and its
//! observation goes on; resuming releases each held write once, in order; the state is
//! journaled, so a restart while halted stays halted.

use std::sync::Arc;

use runtime::conformance::trace;
use runtime::journal::MemJournal;
use runtime::ports::VirtualClock;
use runtime::*;

/// On `start`: one write named by the body, and one probe.
struct Task;
impl Handler for Task {
    fn interests(&self) -> &[EventKindTag] {
        &["start", EFFECT_SUCCEEDED]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "start" {
            vec![
                Command::Effect(EffectSpec { name: "write".into(), input: ev.kind.body.clone(), deadline_ms: 1000 }),
                Command::Probe(EffectSpec { name: "read".into(), input: ev.kind.body.clone(), deadline_ms: 1000 }),
            ]
        } else {
            vec![Command::Emit(EventKind::new("observed", &ev.kind.body))]
        }
    }
}

fn count(j: &dyn Journal, tag: &str) -> usize {
    trace(j).iter().filter(|e| e.2 == tag).count()
}

#[test]
fn an_observe_task_shadows_rather_than_holds_while_halted() {
    let (j, clock) = (MemJournal::new(), VirtualClock::new(0));
    let mut e = Engine::open(j.clone(), Arc::new(clock), vec![Registration::new("task", Task).with_mode(Mode::Observe)], 1).unwrap();
    e.submit("h1", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "")).unwrap();
    e.submit("s1", "s", EventKind::new("start", "a")).unwrap();
    e.submit("r1", HALT_STREAM, EventKind::new(SCHEDULER_RESUMED, "")).unwrap();
    e.run_until_idle().unwrap();
    assert_eq!((count(&*j, EFFECT_SHADOWED), count(&*j, EFFECT_HELD), count(&*j, EFFECT_REQUESTED)), (1, 0, 0));
}

#[test]
fn halting_at_boot_holds_the_first_write_once_and_survives_a_reopen() {
    let (j, clock) = (MemJournal::new(), VirtualClock::new(0));
    let open = || Engine::open(j.clone(), Arc::new(clock.clone()), vec![Registration::new("task", Task)], 1).unwrap();
    let mut e = open();
    e.halt_at_boot("stamp").unwrap();
    e.halt_at_boot("stamp").unwrap();
    e.run_until_idle().unwrap();
    assert!(e.is_halted());
    assert_eq!(count(&*j, SCHEDULER_HALTED), 1, "a second call while halted journals nothing");
    e.submit("s1", "s", EventKind::new("start", "a")).unwrap();
    e.run_until_idle().unwrap();
    assert_eq!((count(&*j, EFFECT_HELD), count(&*j, EFFECT_REQUESTED)), (1, 0));
    assert!(open().is_halted(), "the journaled halt outlives the process");
}
