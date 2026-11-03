//! Observe mode: a task's writes are recorded as `effect.shadowed` and never run, its probes
//! run, and switching it to `Act` after a restart runs nothing it shadowed.

use std::sync::Arc;

use runtime::conformance::{trace, InProcessRig, Rig};
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
                Command::Effect(EffectSpec { name: "write".into(), input: ev.kind.body.clone(), deadline_ms: 1000 }),
                Command::Probe(EffectSpec { name: "read".into(), input: String::new(), deadline_ms: 1000 }),
            ]
        } else {
            vec![Command::Emit(EventKind::new("observed", &ev.kind.body))]
        }
    }
}

fn regs(mode: Mode) -> Vec<Registration> {
    vec![Registration::new("task", Task).with_mode(mode)]
}

#[derive(serde::Deserialize)]
struct Shadowed {
    spec: EffectSpec,
    deadline_at: u64,
}

pub fn observe_shadows_writes_runs_probes_and_never_replays_them(rig: &dyn Rig) {
    let (j, clock) = (rig.journal(), VirtualClock::new(7));
    let mut x = rig.boot(j.clone(), &clock, 1, regs(Mode::Observe)).unwrap();
    x.submit("s1", "s", EventKind::new("start", "payload")).unwrap();
    x.run_until_idle().unwrap();

    let t = trace(&*j);
    let shadowed: Vec<_> = t.iter().filter(|e| e.2 == EFFECT_SHADOWED).collect();
    assert_eq!(shadowed.len(), 1, "the write is recorded as shadowed: {t:?}");
    assert_eq!(t.iter().filter(|e| e.2 == EFFECT_REQUESTED).count(), 0, "and never requested");
    assert_eq!(shadowed[0].0, derive_id("s1/task/0".into()), "with the id effect.requested would have had");
    let body: Shadowed = serde_json::from_str(&shadowed[0].3).expect("the shadowed body is the request body");
    assert_eq!(body.spec, EffectSpec { name: "write".into(), input: "payload".into(), deadline_ms: 1000 }, "the spec round-trips");
    assert_eq!(body.deadline_at, 1007);

    let pending = x.pending_effects();
    assert_eq!(pending.iter().map(|p| p.spec.name.as_str()).collect::<Vec<_>>(), vec!["read"], "only the probe waits to run");
    assert!(x.complete(&pending[0].key, Outcome::Succeeded("seen".into())).unwrap());
    x.run_until_idle().unwrap();
    assert_eq!(trace(&*j).iter().filter(|e| e.2 == "observed").count(), 1, "the probe's answer reaches the task");
    drop(x);

    let mut y = rig.boot(j.clone(), &clock, 1, regs(Mode::Act)).unwrap();
    assert!(y.pending_effects().is_empty(), "a shadowed write is not an open effect after a switch to act");
    y.run_until_idle().unwrap();
    assert!(y.pending_effects().is_empty());
    assert_eq!(count(&*j, EFFECT_REQUESTED), 0, "nothing shadowed is requested for real");

    j.set_cursor(0).unwrap();
    let mut z = rig.boot(j.clone(), &clock, 1, regs(Mode::Act)).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count(&*j, EFFECT_REQUESTED), 0, "nor on a replay of the whole journal");
    assert!(z.pending_effects().is_empty());

    z.submit("s2", "s", EventKind::new("start", "now")).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(z.pending_effects().iter().filter(|p| p.spec.name == "write").count(), 1, "in act the same task's next write is requested");
    assert_eq!(count(&*j, EFFECT_SHADOWED), 1);
}

fn count(j: &dyn Journal, tag: &str) -> usize {
    trace(j).iter().filter(|e| e.2 == tag).count()
}

#[test]
fn observe_mode_on_the_memory_journal() {
    observe_shadows_writes_runs_probes_and_never_replays_them(&InProcessRig(|| MemJournal::new() as Arc<dyn Journal>));
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
