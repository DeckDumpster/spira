//! The halt gate: while the scheduler is halted every task's writes are held in order and its
//! observation goes on; resuming releases each held write once, in order; the state is
//! journaled, so a restart while halted stays halted.

use std::sync::Arc;

use runtime::conformance::{trace, InProcessRig, Rig};
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

fn regs() -> Vec<Registration> {
    vec![Registration::new("task", Task)]
}

fn count(j: &dyn Journal, tag: &str) -> usize {
    trace(j).iter().filter(|e| e.2 == tag).count()
}

fn writes(x: &dyn Executor) -> Vec<String> {
    let mut w: Vec<_> = x.pending_effects().into_iter().filter(|p| p.spec.name == "write").collect();
    w.sort_by_key(|p| p.requested_at);
    w.into_iter().map(|p| p.spec.input).collect()
}

fn answer_probes(x: &mut dyn Executor) {
    for p in x.pending_effects().into_iter().filter(|p| p.spec.name == "read") {
        x.complete(&p.key, Outcome::Succeeded(p.spec.input.clone())).unwrap();
    }
    x.run_until_idle().unwrap();
}

pub fn halt_holds_writes_runs_probes_and_resume_releases_once_in_order(rig: &dyn Rig) {
    let (j, clock) = (rig.journal(), VirtualClock::new(0));
    let mut x = rig.boot(j.clone(), &clock, 1, regs()).unwrap();
    x.submit("h1", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "test")).unwrap();
    x.run_until_idle().unwrap();
    for (i, body) in ["a", "b", "c"].iter().enumerate() {
        clock.advance(10);
        x.submit(&format!("s{i}"), "s", EventKind::new("start", body)).unwrap();
        x.run_until_idle().unwrap();
    }
    assert_eq!(count(&*j, EFFECT_HELD), 3, "every write is held while halted");
    assert_eq!(count(&*j, EFFECT_REQUESTED), 0, "none is requested");
    assert!(writes(&*x).is_empty());
    answer_probes(&mut *x);
    assert_eq!(count(&*j, "observed"), 3, "probes run while halted");
    drop(x);

    let mut y = rig.boot(j.clone(), &clock, 1, regs()).unwrap();
    y.submit("s9", "s", EventKind::new("start", "d")).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(count(&*j, EFFECT_HELD), 4, "a restart while halted stays halted");
    assert!(writes(&*y).is_empty());

    clock.advance(10);
    y.submit("r1", HALT_STREAM, EventKind::new(SCHEDULER_RESUMED, "")).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(writes(&*y), vec!["a", "b", "c", "d"], "resume releases every held write");
    let released: Vec<String> = trace(&*j).into_iter().filter(|e| e.2 == EFFECT_REQUESTED).map(|e| e.3).collect();
    assert_eq!(released.len(), 4);
    for (r, want) in released.iter().zip(["a", "b", "c", "d"]) {
        assert!(r.contains(&format!("\"input\":\"{want}\"")), "released in the order held: {released:?}");
    }
    drop(y);

    j.set_cursor(0).unwrap();
    let mut z = rig.boot(j.clone(), &clock, 1, regs()).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count(&*j, EFFECT_REQUESTED), 4, "a replay of the whole journal releases nothing twice");
    z.submit("s10", "s", EventKind::new("start", "e")).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count(&*j, EFFECT_REQUESTED), 5, "resumed, a write is requested at once");
    assert_eq!(count(&*j, EFFECT_HELD), 4);

    z.submit("h2", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "")).unwrap();
    z.submit("s11", "s", EventKind::new("start", "f")).unwrap();
    z.submit("r2", HALT_STREAM, EventKind::new(SCHEDULER_RESUMED, "")).unwrap();
    z.run_until_idle().unwrap();
    let inputs: Vec<String> = z.pending_effects().into_iter().filter(|p| p.spec.name == "write").map(|p| p.spec.input).collect();
    assert_eq!(inputs.iter().filter(|i| ["a", "b", "c", "d"].contains(&i.as_str())).count(), 4, "a second resume releases only what the second halt held");
    assert_eq!(count(&*j, EFFECT_REQUESTED), 6);
}

#[test]
fn halt_on_the_memory_journal() {
    halt_holds_writes_runs_probes_and_resume_releases_once_in_order(&InProcessRig(|| MemJournal::new() as Arc<dyn Journal>));
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
