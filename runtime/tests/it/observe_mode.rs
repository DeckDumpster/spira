//! Observe mode: a task's writes are recorded as `effect.shadowed` and never run, its probes
//! run, and switching it to `Act` after a restart runs nothing it shadowed.

use std::sync::Arc;

use runtime::conformance::trace;
use runtime::journal::MemJournal;
use runtime::ports::VirtualClock;
use runtime::*;

/// On `start`: one write and one probe. On the probe's answer: a carried-state note.
struct Task;
impl Handler for Task {
    fn interests(&self) -> &[EventKindTag] {
        &["start", EFFECT_SUCCEEDED]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "start" {
            vec![
                Command::Effect(EffectSpec { name: "write".into(), input: ev.kind.body.clone(), deadline_ms: 1000, key: "write".into() }),
                Command::Probe(EffectSpec { name: "read".into(), input: String::new(), deadline_ms: 1000, key: "read".into() }),
            ]
        } else {
            vec![Command::Emit(EventKind::new("observed", &ev.kind.body))]
        }
    }
}

fn regs(mode: Mode) -> Vec<Registration> {
    vec![Registration::new("task", Task).with_mode(mode)]
}

fn count(j: &dyn Journal, tag: &str) -> usize {
    trace(j).iter().filter(|e| e.2 == tag).count()
}

#[test]
fn act_mode_requests_the_write() {
    let (j, clock) = (MemJournal::new(), VirtualClock::new(0));
    let mut e = Engine::open(j.clone(), Arc::new(clock), regs(Mode::Act), 1).unwrap();
    e.submit("s1", "s", EventKind::new("start", "p")).unwrap();
    e.run_until_idle().unwrap();
    assert_eq!(count(&*j, EFFECT_REQUESTED), 1);
    assert_eq!(count(&*j, EFFECT_SHADOWED), 0);
}
