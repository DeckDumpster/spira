//! The conformance suite: what any executor on any journal must do. Each scenario takes a
//! `Rig`, which supplies a fresh journal and boots an executor on a journal — booting again on
//! the same journal is how a restart is staged: it finds what the store kept and nothing else.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::engine::{Engine, Executor, Outcome};
use crate::event::*;
use crate::journal::{FaultyJournal, Journal};
use crate::ports::VirtualClock;
use crate::Result;

pub trait Rig {
    /// An empty journal.
    fn journal(&self) -> Arc<dyn Journal>;
    fn boot(&self, journal: Arc<dyn Journal>, clock: &VirtualClock, seed: u64, regs: Vec<Registration>) -> Result<Box<dyn Executor>>;
}

/// The in-process executor over whatever journal the caller supplies.
pub struct InProcessRig<F: Fn() -> Arc<dyn Journal>>(pub F);

impl<F: Fn() -> Arc<dyn Journal>> Rig for InProcessRig<F> {
    fn journal(&self) -> Arc<dyn Journal> {
        (self.0)()
    }

    fn boot(&self, journal: Arc<dyn Journal>, clock: &VirtualClock, seed: u64, regs: Vec<Registration>) -> Result<Box<dyn Executor>> {
        Ok(Box::new(Engine::open(journal, Arc::new(clock.clone()), regs, seed)?))
    }
}

type Counter = Arc<AtomicUsize>;

struct Pinger(Counter);
impl Handler for Pinger {
    fn interests(&self) -> &[EventKindTag] {
        &["ping"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        self.0.fetch_add(1, Ordering::SeqCst);
        vec![Command::Emit(EventKind::new("pong", &ev.id.0))]
    }
}

struct Worker;
impl Handler for Worker {
    fn interests(&self) -> &[EventKindTag] {
        &["start", EFFECT_SUCCEEDED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "start" {
            vec![Command::Effect(EffectSpec { name: "work".into(), input: ev.kind.body.clone(), deadline_ms: 1000, key: format!("work/{}", ev.kind.body) })]
        } else {
            let key = ev.cause.as_ref().map_or(String::new(), |c| c.0.clone());
            vec![Command::Emit(EventKind::new(&format!("done:{}", ev.kind.tag), &key))]
        }
    }
}

struct Roller;
impl Handler for Roller {
    fn interests(&self) -> &[EventKindTag] {
        &["roll", "tick"]
    }
    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "tick" {
            return vec![Command::Emit(EventKind::new("ticked", &format!("{}@{}", ev.kind.body, ctx.now())))];
        }
        let mut cmds = vec![Command::Emit(EventKind::new("rolled", &ctx.rand_u64().to_string()))];
        cmds.extend((0..5).map(|i| Command::Schedule { at: ev.at + 10, kind: EventKind::new("tick", &i.to_string()) }));
        cmds
    }
}

struct Boom;
impl Handler for Boom {
    fn interests(&self) -> &[EventKindTag] {
        &["boom"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        panic!("boom handler panicked")
    }
}

struct Alarm;
impl Handler for Alarm {
    fn interests(&self) -> &[EventKindTag] {
        &["arm", "alarm"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "arm" {
            vec![Command::Schedule { at: 50, kind: EventKind::new("alarm", "") }]
        } else {
            vec![Command::Emit(EventKind::new("rang", ""))]
        }
    }
}

fn all(count: &Counter) -> Vec<Registration> {
    vec![
        Registration::new("pinger", Pinger(count.clone())),
        Registration::new("worker", Worker),
        Registration::new("roller", Roller),
        Registration::new("boom", Boom),
        Registration::new("alarm", Alarm),
    ]
}

/// What the journal was handed, in order: the executor-independent record two runs are compared on.
pub fn trace(j: &dyn Journal) -> Vec<(String, String, String, String, u64)> {
    j.history().into_iter().map(|e| (e.id.0, e.stream.0, e.kind.tag, e.kind.body, e.at)).collect()
}

fn count_tag(j: &dyn Journal, tag: &str) -> usize {
    trace(j).iter().filter(|t| t.2 == tag).count()
}

fn ping(x: &mut dyn Executor, id: &str, stream: &str) {
    x.submit(id, stream, EventKind::new("ping", "")).expect("submit");
}

pub fn a_duplicate_submit_dispatches_once(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    ping(&mut *x, "e1", "s");
    ping(&mut *x, "e1", "s");
    x.run_until_idle().unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1, "a duplicate submit must not run the handler twice");
    assert_eq!(count_tag(&*j, "ping"), 1);
    assert_eq!(count_tag(&*j, "pong"), 1);
}

pub fn a_failed_commit_is_sent_again_and_the_turn_lands_once(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let faulty = FaultyJournal::new(j.clone());
    let mut x = rig.boot(faulty.clone(), &clock, 1, all(&n)).unwrap();
    ping(&mut *x, "e1", "s");
    faulty.cannot_tell_next("commit");
    assert!(x.run_until_idle().is_err(), "the injected fault must surface");
    x.run_until_idle().unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1, "the handler ran once, not again for the retried commit");
    assert_eq!(count_tag(&*j, "pong"), 1);
}

pub fn a_restart_finds_an_unanswered_effect_and_does_not_request_it_again(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    x.submit("s1", "s", EventKind::new("start", "payload")).unwrap();
    x.run_until_idle().unwrap();
    let key = x.pending_effects().remove(0).key;
    drop(x);

    let mut y = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    let pending = y.pending_effects();
    assert_eq!(pending.len(), 1, "the unanswered effect must survive a restart");
    assert_eq!(pending[0].key, key, "and keep its idempotency key");
    assert_eq!(pending[0].spec.input, "payload");
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 1, "a restart must not request the effect again");
    assert!(y.complete(&key, Outcome::Succeeded("ok".into())).unwrap());
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "done:effect.succeeded"), 1);
    assert!(!y.complete(&key, Outcome::Succeeded("ok".into())).unwrap(), "a second outcome is dropped");
}

pub fn out_of_order_effect_outcomes(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    x.submit("s1", "s", EventKind::new("start", "a")).unwrap();
    x.submit("s2", "s", EventKind::new("start", "b")).unwrap();
    x.run_until_idle().unwrap();
    let keys: Vec<String> = x.pending_effects().into_iter().map(|p| p.key).collect();
    assert_eq!(keys.len(), 2);
    x.complete(&keys[1], Outcome::Succeeded("b".into())).unwrap();
    x.run_until_idle().unwrap();
    x.complete(&keys[0], Outcome::Succeeded("a".into())).unwrap();
    x.run_until_idle().unwrap();
    let done: Vec<String> = trace(&*j).into_iter().filter(|t| t.2 == "done:effect.succeeded").map(|t| t.3).collect();
    assert_eq!(done, vec![keys[1].clone(), keys[0].clone()], "each outcome is handled once, in the order it arrived");
    assert!(x.pending_effects().is_empty());
}

pub fn effect_deadline_times_out(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    x.submit("s1", "s", EventKind::new("start", "")).unwrap();
    x.run_until_idle().unwrap();
    let key = x.pending_effects().remove(0).key;
    clock.advance_to(999);
    x.run_until_idle().unwrap();
    assert_eq!(x.pending_effects().len(), 1, "not before the deadline");
    clock.advance_to(1000);
    x.run_until_idle().unwrap();
    assert!(x.pending_effects().is_empty());
    assert_eq!(count_tag(&*j, "done:effect.timed_out"), 1);
    assert!(!x.complete(&key, Outcome::Succeeded("late".into())).unwrap(), "a late success after a timeout is dropped");
    x.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, EFFECT_SUCCEEDED), 0);
}

fn roll_trace(rig: &dyn Rig, seed: u64) -> Vec<(String, String, String, String, u64)> {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, seed, all(&n)).unwrap();
    x.submit("r1", "s", EventKind::new("roll", "")).unwrap();
    x.run_until_idle().unwrap();
    clock.advance_to(10);
    x.run_until_idle().unwrap();
    trace(&*j)
}

pub fn determinism_under_one_seed(rig: &dyn Rig) {
    let first = roll_trace(rig, 5);
    assert_eq!(count_tag_in(&first, "ticked"), 5);
    assert_eq!(first, roll_trace(rig, 5), "same seed, same event trace");
    assert!((6..40).any(|s| roll_trace(rig, s) != first), "the seed must reach tie order and randomness");
}

pub fn a_fired_schedule_row_is_not_fired_by_the_next_process(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 5, all(&n)).unwrap();
    x.submit("r1", "s", EventKind::new("roll", "")).unwrap();
    x.run_until_idle().unwrap();
    clock.advance_to(10);
    x.run_until_idle().unwrap();
    drop(x);
    let mut y = rig.boot(j.clone(), &clock, 5, all(&n)).unwrap();
    clock.advance_to(20);
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "ticked"), 5, "a restart fires no row that already fired");
}

fn count_tag_in(t: &[(String, String, String, String, u64)], tag: &str) -> usize {
    t.iter().filter(|e| e.2 == tag).count()
}

pub fn handler_panic_quarantines_its_stream_only(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    x.submit("b1", "bad", EventKind::new("boom", "")).unwrap();
    ping(&mut *x, "p-bad", "bad");
    ping(&mut *x, "p-ok", "ok");
    x.run_until_idle().unwrap();
    let pairs: Vec<(String, String)> = trace(&*j).into_iter().filter(|t| t.2 == "pong").map(|t| (t.1, t.3)).collect();
    assert_eq!(pairs, vec![("ok".to_string(), "p-ok".to_string())], "only the healthy stream is served");
    let p = j.poisoned(&StreamKey("bad".into())).unwrap().expect("the stream is marked poisoned");
    assert_eq!((p.event.0.as_str(), p.handler.as_str()), ("b1", "boom"));
    assert!(p.message.contains("boom handler panicked"));
    assert!(j.poisoned(&StreamKey("ok".into())).unwrap().is_none());

    drop(x);
    let mut y = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    ping(&mut *y, "p-bad2", "bad");
    ping(&mut *y, "p-ok2", "ok");
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "pong"), 2, "the quarantine outlives a restart");
}

pub fn schedule_rows_survive_restart_and_fire_once(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    x.submit("a1", "s", EventKind::new("arm", "")).unwrap();
    x.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "rang"), 0);
    drop(x);

    let faulty = FaultyJournal::new(j.clone());
    let mut y = rig.boot(faulty.clone(), &clock, 1, all(&n)).unwrap();
    clock.advance_to(50);
    faulty.cannot_tell_next("commit");
    assert!(y.run_until_idle().is_err());
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "alarm"), 1, "a row whose commit failed is not fired again by the process that fired it");
    drop(y);

    let mut z = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "alarm"), 1, "a row fired before the restart is not fired again");
    assert_eq!(count_tag(&*j, "rang"), 1);
}

/// On `start`: one write and one probe, both carrying the body. On the probe's answer: `observed`.
struct Writer;
impl Handler for Writer {
    fn interests(&self) -> &[EventKindTag] {
        &["start", EFFECT_SUCCEEDED]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "start" {
            vec![
                Command::Effect(EffectSpec { name: "write".into(), input: ev.kind.body.clone(), deadline_ms: 1000, key: format!("write/{}", ev.kind.body) }),
                Command::Probe(EffectSpec { name: "read".into(), input: ev.kind.body.clone(), deadline_ms: 1000, key: format!("read/{}", ev.kind.body) }),
            ]
        } else {
            vec![Command::Emit(EventKind::new("observed", &ev.kind.body))]
        }
    }
}

fn writer(mode: Mode) -> Vec<Registration> {
    vec![Registration::new("task", Writer).with_mode(mode)]
}

#[derive(serde::Deserialize)]
struct Shadowed {
    spec: EffectSpec,
    deadline_at: u64,
}

pub fn observe_shadows_writes_and_runs_probes(rig: &dyn Rig) {
    let (j, clock) = (rig.journal(), VirtualClock::new(7));
    let mut x = rig.boot(j.clone(), &clock, 1, writer(Mode::Observe)).unwrap();
    x.submit("s1", "s", EventKind::new("start", "payload")).unwrap();
    x.run_until_idle().unwrap();

    let t = trace(&*j);
    let shadowed: Vec<_> = t.iter().filter(|e| e.2 == EFFECT_SHADOWED).collect();
    assert_eq!(shadowed.len(), 1, "the write is recorded as shadowed: {t:?}");
    assert_eq!(t.iter().filter(|e| e.2 == EFFECT_REQUESTED).count(), 0, "and never requested");
    assert_eq!(shadowed[0].0, derive_id("s1/task/0".into()), "with the id effect.requested would have had");
    let body: Shadowed = serde_json::from_str(&shadowed[0].3).expect("the shadowed body is the request body");
    assert_eq!(body.spec, EffectSpec { name: "write".into(), input: "payload".into(), deadline_ms: 1000, key: "write/payload".into() }, "the spec round-trips");
    assert_eq!(body.deadline_at, 1007);

    let pending = x.pending_effects();
    assert_eq!(pending.iter().map(|p| p.spec.name.as_str()).collect::<Vec<_>>(), vec!["read"], "only the probe waits to run");
    assert!(x.complete(&pending[0].key, Outcome::Succeeded("seen".into())).unwrap());
    x.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "observed"), 1, "the probe's answer reaches the task");
    drop(x);

    let mut y = rig.boot(j.clone(), &clock, 1, writer(Mode::Act)).unwrap();
    assert!(y.pending_effects().is_empty(), "a shadowed write is not an open effect after a switch to act");
    y.run_until_idle().unwrap();
    assert!(y.pending_effects().is_empty());
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 0, "nothing shadowed is requested for real");
    y.submit("s2", "s", EventKind::new("start", "now")).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(y.pending_effects().iter().filter(|p| p.spec.name == "write").count(), 1, "in act the same task's next write is requested");
    assert_eq!(count_tag(&*j, EFFECT_SHADOWED), 1);
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
    let mut x = rig.boot(j.clone(), &clock, 1, writer(Mode::Act)).unwrap();
    x.submit("h1", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "test")).unwrap();
    x.run_until_idle().unwrap();
    for (i, body) in ["a", "b", "c"].iter().enumerate() {
        clock.advance(10);
        x.submit(&format!("s{i}"), "s", EventKind::new("start", body)).unwrap();
        x.run_until_idle().unwrap();
    }
    assert_eq!(count_tag(&*j, EFFECT_HELD), 3, "every write is held while halted");
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 0, "none is requested");
    assert!(writes(&*x).is_empty());
    answer_probes(&mut *x);
    assert_eq!(count_tag(&*j, "observed"), 3, "probes run while halted");
    drop(x);

    let mut y = rig.boot(j.clone(), &clock, 1, writer(Mode::Act)).unwrap();
    y.submit("s9", "s", EventKind::new("start", "d")).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, EFFECT_HELD), 4, "a restart while halted stays halted");
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
    let z = &mut y;
    z.submit("s10", "s", EventKind::new("start", "e")).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 5, "resumed, a write is requested at once");
    assert_eq!(count_tag(&*j, EFFECT_HELD), 4);

    z.submit("h2", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "")).unwrap();
    z.submit("s11", "s", EventKind::new("start", "f")).unwrap();
    z.submit("r2", HALT_STREAM, EventKind::new(SCHEDULER_RESUMED, "")).unwrap();
    z.run_until_idle().unwrap();
    let inputs: Vec<String> = z.pending_effects().into_iter().filter(|p| p.spec.name == "write").map(|p| p.spec.input).collect();
    assert_eq!(inputs.iter().filter(|i| ["a", "b", "c", "d"].contains(&i.as_str())).count(), 4, "a second resume releases only what the second halt held");
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 6);
}

/// On `wake`, wherever it lands: a `nudge` onto stream `y`, carrying the waking event's id.
struct Sender;
impl Handler for Sender {
    fn interests(&self) -> &[EventKindTag] {
        &["wake"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        vec![Command::EmitTo { stream: StreamKey("y".into()), kind: EventKind::new("nudge", &ev.id.0) }]
    }
}

/// On `nudge`: counts it and answers `heard` on the stream it arrived on.
struct Hearer(Counter);
impl Handler for Hearer {
    fn interests(&self) -> &[EventKindTag] {
        &["nudge"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        self.0.fetch_add(1, Ordering::SeqCst);
        vec![Command::Emit(EventKind::new("heard", &ev.kind.body))]
    }
}

fn cross(heard: &Counter, mode: Mode) -> Vec<Registration> {
    vec![Registration::new("sender", Sender).with_mode(mode), Registration::new("hearer", Hearer(heard.clone()))]
}

/// Stream `y` exists before anything is sent to it, as a task's own stream does.
fn wake(x: &mut dyn Executor, id: &str) {
    x.submit("y0", "y", EventKind::new("y.boot", "")).unwrap();
    x.submit(id, "x", EventKind::new("wake", "")).unwrap();
}

pub fn emit_to_wakes_another_stream_once(rig: &dyn Rig) {
    let (j, clock, heard) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 3, cross(&heard, Mode::Act)).unwrap();
    wake(&mut *x, "w1");
    x.run_until_idle().unwrap();
    let t = trace(&*j);
    let nudges: Vec<_> = t.iter().filter(|e| e.2 == "nudge").collect();
    assert_eq!(nudges.len(), 1, "one nudge: {t:?}");
    assert_eq!((nudges[0].0.as_str(), nudges[0].1.as_str(), nudges[0].3.as_str()), (derive_id("w1/sender/0>y".into()).as_str(), "y", "w1"), "on the target stream, its id from the cause and the target");
    assert_eq!(heard.load(Ordering::SeqCst), 1, "the target stream's handler hears it once");
    assert_eq!(t.iter().filter(|e| e.2 == "heard").map(|e| e.1.as_str()).collect::<Vec<_>>(), vec!["y"]);
}

pub fn emit_to_is_shadowed_under_observe_and_runs_under_halt(rig: &dyn Rig) {
    let (j, clock, heard) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, cross(&heard, Mode::Observe)).unwrap();
    wake(&mut *x, "w1");
    x.run_until_idle().unwrap();
    let t = trace(&*j);
    let shadowed: Vec<_> = t.iter().filter(|e| e.2 == EMIT_SHADOWED).collect();
    assert_eq!(shadowed.len(), 1, "an observing task's cross-stream emit is recorded: {t:?}");
    assert_eq!((shadowed[0].0.as_str(), shadowed[0].1.as_str()), (derive_id("w1/sender/0>y".into()).as_str(), "x"), "on its own stream, with the id the emit would have had");
    let body: ShadowedEmit = serde_json::from_str(&shadowed[0].3).expect("the shadowed body names the target");
    assert_eq!(body, ShadowedEmit { stream: StreamKey("y".into()), kind: EventKind::new("nudge", "w1") });
    assert_eq!(count_tag(&*j, "nudge"), 0, "and never lands on the target");
    assert_eq!(heard.load(Ordering::SeqCst), 0);
    drop(x);

    let (k, heard) = (rig.journal(), Counter::default());
    let mut y = rig.boot(k.clone(), &clock, 1, cross(&heard, Mode::Act)).unwrap();
    y.submit("h1", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "test")).unwrap();
    wake(&mut *y, "w1");
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*k, "nudge"), 1, "a halt holds effects, not events");
    assert_eq!(heard.load(Ordering::SeqCst), 1);
    assert_eq!(count_tag(&*k, EFFECT_HELD), 0);
}

/// Counts on its stream: each `bump` and each probe answer emits `count` one past the newest
/// `count` it sees; `start` asks two probes.
struct Tally;
impl Handler for Tally {
    fn interests(&self) -> &[EventKindTag] {
        &["bump", "start", EFFECT_SUCCEEDED]
    }
    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "start" {
            return (0..2).map(|i| Command::Probe(EffectSpec { name: "read".into(), input: i.to_string(), deadline_ms: 1000, key: format!("read/{i}") })).collect();
        }
        let seen: u64 = view.events(&ev.stream).iter().rev().find(|e| e.kind.tag == "count").map_or(0, |e| e.kind.body.parse().unwrap_or(0));
        vec![Command::Emit(EventKind::new("count", &(seen + 1).to_string()))]
    }
}

fn tally() -> Vec<Registration> {
    vec![Registration::new("tally", Tally)]
}

fn counts(j: &dyn Journal) -> Vec<String> {
    trace(j).into_iter().filter(|t| t.2 == "count").map(|t| t.3).collect()
}

/// Two bumps queued together.
fn bump_twice(x: &mut dyn Executor) {
    x.submit("b1", "t", EventKind::new("bump", "")).unwrap();
    x.submit("b2", "t", EventKind::new("bump", "")).unwrap();
    x.run_until_idle().unwrap();
}

/// Two probe answers in memory together.
fn answer_twice(x: &mut dyn Executor) {
    x.submit("go", "t", EventKind::new("start", "")).unwrap();
    x.run_until_idle().unwrap();
    for p in x.pending_effects() {
        x.complete(&p.key, Outcome::Succeeded(p.spec.input.clone())).unwrap();
    }
    x.run_until_idle().unwrap();
}

/// Per-stream sequential consistency: a handler's commands for one event are in the projection
/// before it sees the next event on its stream, whether the two were queued together or
/// answered in memory together.
pub fn a_handlers_writes_are_seen_before_its_streams_next_event(rig: &dyn Rig) {
    let (j, clock) = (rig.journal(), VirtualClock::new(0));
    let mut x = rig.boot(j.clone(), &clock, 1, tally()).unwrap();
    bump_twice(&mut *x);
    assert_eq!(counts(&*j), ["1", "2"], "the second bump sees the first bump's count, though it was queued before it");
    answer_twice(&mut *x);
    assert_eq!(counts(&*j), ["1", "2", "3", "4"], "the second probe answer sees the first answer's count");
}

/// On `ask`, one write and one probe. Every outcome it is offered becomes `heard`, whose body
/// names the hearer and the outcome's body (`<requester>:<effect>`).
struct Asker(&'static str, bool);
impl Handler for Asker {
    fn interests(&self) -> &[EventKindTag] {
        &["ask", EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.kind.tag == "ask" {
            let spec = |name: &str| EffectSpec { name: name.into(), input: self.0.into(), deadline_ms: 1000, key: name.into() };
            return vec![Command::Effect(spec("write")), Command::Probe(spec("read"))];
        }
        vec![Command::Emit(EventKind::new("heard", &format!("{}<-{}", self.0, ev.kind.body)))]
    }
    fn watches_foreign_outcomes(&self) -> bool {
        self.1
    }
}

fn askers() -> Vec<Registration> {
    vec![Registration::new("asker", Asker("asker", false)), Registration::new("other", Asker("other", false)), Registration::new("watcher", Asker("watcher", true))]
}

fn heard(j: &dyn Journal) -> Vec<String> {
    let mut h: Vec<String> = trace(j).into_iter().filter(|t| t.2 == "heard").map(|t| t.3).collect();
    h.sort();
    h
}

/// Answer every waiting effect (`only`: those named so) with `<requester>:<effect>`.
fn answer(x: &mut dyn Executor, only: Option<&str>) {
    for p in x.pending_effects().into_iter().filter(|p| only.is_none_or(|n| p.spec.name == n)) {
        x.complete(&p.key, Outcome::Succeeded(format!("{}:{}", p.spec.input, p.spec.name))).unwrap();
    }
    x.run_until_idle().unwrap();
}

/// An effect's outcome, journaled or memory-only, is offered to the task that asked for it and
/// to no other task listening on its stream, except one that opts in to every outcome. The
/// routing holds after a restart between the request and its outcome.
pub fn an_outcome_is_offered_only_to_the_task_that_asked(rig: &dyn Rig) {
    let (j, clock) = (rig.journal(), VirtualClock::new(0));
    let mut x = rig.boot(j.clone(), &clock, 1, askers()).unwrap();
    x.submit("a1", "s", EventKind::new("ask", "")).unwrap();
    x.run_until_idle().unwrap();
    assert_eq!(x.pending_effects().len(), 6, "each task's write and probe wait");
    answer(&mut *x, None);
    let mut want: Vec<String> = vec![];
    for asker in ["asker", "other", "watcher"] {
        for effect in ["read", "write"] {
            want.push(format!("watcher<-{asker}:{effect}"));
            if asker != "watcher" {
                want.push(format!("{asker}<-{asker}:{effect}"));
            }
        }
    }
    want.sort();
    assert_eq!(heard(&*j), want, "each outcome reaches its requester, and the watcher hears them all");
    drop(x);

    let k = rig.journal();
    let mut x = rig.boot(k.clone(), &clock, 1, askers()).unwrap();
    x.submit("a1", "s", EventKind::new("ask", "")).unwrap();
    x.run_until_idle().unwrap();
    drop(x);
    let mut y = rig.boot(k.clone(), &clock, 1, askers()).unwrap();
    answer(&mut *y, Some("write"));
    let writes: Vec<String> = want.iter().filter(|w| w.ends_with(":write")).cloned().collect();
    assert_eq!(heard(&*k), writes, "after a restart the kept request still names the task that asked");
}

/// On `start`, a `page` and a `write`.
struct Pager;
impl Handler for Pager {
    fn interests(&self) -> &[EventKindTag] {
        &["start"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        ["page", "write"].iter().map(|n| Command::Effect(EffectSpec { name: n.to_string(), input: ev.kind.body.clone(), deadline_ms: 1000, key: n.to_string() })).collect()
    }
}

fn planes() -> Vec<Registration> {
    vec![
        Registration::new("work", Writer),
        Registration::new("signal", Writer).with_plane(Plane::Signal),
        Registration::new("mixed", Pager).with_signal_effects(&["page"]),
    ]
}

/// Each waiting effect as `<task>.<effect>`, sorted.
fn writers(x: &dyn Executor) -> Vec<String> {
    let mut w: Vec<String> = x.pending_effects().into_iter().filter(|p| p.spec.name != "read").map(|p| format!("{}.{}", p.key.split('/').nth(1).unwrap_or_default(), p.spec.name)).collect();
    w.sort();
    w
}

/// While halted, a `Signal` task's write runs, and so does a `Work` task's `Signal` effect;
/// a `Work` effect is held (the `Work` task's write and the mixed task's write), and the resume
/// releases each once, also across a restart.
pub fn a_halt_holds_work_and_lets_signals_through(rig: &dyn Rig) {
    let (j, clock) = (rig.journal(), VirtualClock::new(0));
    let mut x = rig.boot(j.clone(), &clock, 1, planes()).unwrap();
    x.submit("h1", HALT_STREAM, EventKind::new(SCHEDULER_HALTED, "test")).unwrap();
    x.run_until_idle().unwrap();
    clock.advance(10);
    x.submit("s1", "s", EventKind::new("start", "now")).unwrap();
    x.run_until_idle().unwrap();
    assert_eq!(writers(&*x), ["mixed.page", "signal.write"], "the signal effects are requested under the halt");
    assert_eq!(count_tag(&*j, EFFECT_HELD), 2, "and the work effects are held, the mixed task's write among them");
    drop(x);

    let mut y = rig.boot(j.clone(), &clock, 1, planes()).unwrap();
    assert_eq!(writers(&*y), ["mixed.page", "signal.write"], "a restart while halted holds nothing more");
    clock.advance(10);
    y.submit("r1", HALT_STREAM, EventKind::new(SCHEDULER_RESUMED, "")).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(writers(&*y), ["mixed.page", "mixed.write", "signal.write", "work.write"], "the resume releases the work effects");
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 4, "each effect is requested once");
}

pub fn run_all(rig: &dyn Rig) {
    a_duplicate_submit_dispatches_once(rig);
    a_failed_commit_is_sent_again_and_the_turn_lands_once(rig);
    a_restart_finds_an_unanswered_effect_and_does_not_request_it_again(rig);
    out_of_order_effect_outcomes(rig);
    effect_deadline_times_out(rig);
    determinism_under_one_seed(rig);
    a_fired_schedule_row_is_not_fired_by_the_next_process(rig);
    handler_panic_quarantines_its_stream_only(rig);
    schedule_rows_survive_restart_and_fire_once(rig);
    observe_shadows_writes_and_runs_probes(rig);
    halt_holds_writes_runs_probes_and_resume_releases_once_in_order(rig);
    emit_to_wakes_another_stream_once(rig);
    emit_to_is_shadowed_under_observe_and_runs_under_halt(rig);
    a_handlers_writes_are_seen_before_its_streams_next_event(rig);
    an_outcome_is_offered_only_to_the_task_that_asked(rig);
    a_halt_holds_work_and_lets_signals_through(rig);
}
