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
