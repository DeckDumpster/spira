//! The conformance suite: what any executor on any journal must do. Each scenario takes a
//! `Rig`, which supplies a fresh journal and boots an executor on a journal — booting again on
//! the same journal is how a crash and restart is staged.

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
            vec![Command::Effect(EffectSpec { name: "work".into(), input: ev.kind.body.clone(), deadline_ms: 1000 })]
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

/// What the journal holds, in order: the executor-independent record two runs are compared on.
pub fn trace(j: &dyn Journal) -> Vec<(String, String, String, String, u64)> {
    j.read_from(0, usize::MAX)
        .expect("read journal")
        .into_iter()
        .map(|(_, e)| (e.id.0, e.stream.0, e.kind.tag, e.kind.body, e.at))
        .collect()
}

fn count_tag(j: &dyn Journal, tag: &str) -> usize {
    trace(j).iter().filter(|t| t.2 == tag).count()
}

fn ping(x: &mut dyn Executor, id: &str, stream: &str) {
    x.submit(id, stream, EventKind::new("ping", "")).expect("submit");
}

pub fn redelivery_of_any_event(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    ping(&mut *x, "e1", "s");
    ping(&mut *x, "e1", "s");
    x.run_until_idle().unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1, "a duplicate submit must not run the handler twice");
    assert_eq!(count_tag(&*j, "ping"), 1);
    assert_eq!(count_tag(&*j, "pong"), 1);

    let before = trace(&*j);
    j.set_cursor(0).unwrap();
    let mut y = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1, "a replayed event must not run an applied handler again");
    assert_eq!(trace(&*j), before);
}

pub fn crash_between_append_and_dispatch(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    j.append(&Event {
        id: EventId("e1".into()),
        stream: StreamKey("s".into()),
        kind: EventKind::new("ping", ""),
        at: 0,
        cause: None,
        correlation: CorrelationId("e1".into()),
    })
    .unwrap();
    let mut x = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    x.run_until_idle().unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1, "an event appended and never dispatched must be dispatched after restart");
    assert_eq!(count_tag(&*j, "pong"), 1);
}

pub fn crash_during_dispatch_appends_nothing_twice(rig: &dyn Rig) {
    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let faulty = FaultyJournal::new(j.clone());
    let mut x = rig.boot(faulty.clone(), &clock, 1, all(&n)).unwrap();
    ping(&mut *x, "e1", "s");
    faulty.fail_next("mark_applied");
    assert!(x.run_until_idle().is_err(), "the injected fault must surface");
    drop(x);
    let mut y = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "pong"), 1, "the replayed handler must append the same event id, not a second event");
}

pub fn crash_between_effect_request_and_outcome(rig: &dyn Rig) {
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

pub fn determinism_under_replay(rig: &dyn Rig) {
    let first = roll_trace(rig, 5);
    assert_eq!(count_tag_in(&first, "ticked"), 5);
    assert_eq!(first, roll_trace(rig, 5), "same seed, same event trace");
    assert!((6..40).any(|s| roll_trace(rig, s) != first), "the seed must reach tie order and randomness");

    let (j, clock, n) = (rig.journal(), VirtualClock::new(0), Counter::default());
    let faulty = FaultyJournal::new(j.clone());
    let mut x = rig.boot(faulty.clone(), &clock, 5, all(&n)).unwrap();
    x.submit("r1", "s", EventKind::new("roll", "")).unwrap();
    x.run_until_idle().unwrap();
    clock.advance_to(10);
    faulty.fail_next("mark_fired");
    assert!(x.run_until_idle().is_err());
    drop(x);
    let mut y = rig.boot(j.clone(), &clock, 5, all(&n)).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(trace(&*j), first, "a crash and restart must land the same trace as an uninterrupted run");

    j.set_cursor(0).unwrap();
    let mut z = rig.boot(j.clone(), &clock, 5, all(&n)).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(trace(&*j), first, "replaying the whole journal must add nothing");
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
    faulty.fail_next("mark_fired");
    assert!(y.run_until_idle().is_err());
    drop(y);

    let mut z = rig.boot(j.clone(), &clock, 1, all(&n)).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, "alarm"), 1, "a row fired before the crash is not fired again");
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
                Command::Effect(EffectSpec { name: "write".into(), input: ev.kind.body.clone(), deadline_ms: 1000 }),
                Command::Probe(EffectSpec { name: "read".into(), input: ev.kind.body.clone(), deadline_ms: 1000 }),
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

pub fn observe_shadows_writes_runs_probes_and_never_replays_them(rig: &dyn Rig) {
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
    assert_eq!(body.spec, EffectSpec { name: "write".into(), input: "payload".into(), deadline_ms: 1000 }, "the spec round-trips");
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
    drop(y);

    j.set_cursor(0).unwrap();
    let mut z = rig.boot(j.clone(), &clock, 1, writer(Mode::Act)).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 0, "nor on a replay of the whole journal");
    assert!(z.pending_effects().is_empty());

    z.submit("s2", "s", EventKind::new("start", "now")).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(z.pending_effects().iter().filter(|p| p.spec.name == "write").count(), 1, "in act the same task's next write is requested");
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
    drop(y);

    j.set_cursor(0).unwrap();
    let mut z = rig.boot(j.clone(), &clock, 1, writer(Mode::Act)).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(count_tag(&*j, EFFECT_REQUESTED), 4, "a replay of the whole journal releases nothing twice");
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

pub fn emit_to_wakes_another_stream_once_across_a_crash(rig: &dyn Rig) {
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
    let first = t;
    drop(x);

    let (k, again) = (rig.journal(), Counter::default());
    let faulty = FaultyJournal::new(k.clone());
    let mut x = rig.boot(faulty.clone(), &clock, 3, cross(&again, Mode::Act)).unwrap();
    wake(&mut *x, "w1");
    faulty.fail_next("mark_applied");
    assert!(x.run_until_idle().is_err(), "the crash lands after the nudge is appended and before it is dispatched");
    assert_eq!(count_tag(&*k, "nudge"), 1);
    assert_eq!(again.load(Ordering::SeqCst), 0);
    drop(x);
    let mut y = rig.boot(k.clone(), &clock, 3, cross(&again, Mode::Act)).unwrap();
    y.run_until_idle().unwrap();
    assert_eq!(count_tag(&*k, "nudge"), 1, "the replayed sender appends the same id, not a second nudge");
    assert_eq!(again.load(Ordering::SeqCst), 1, "and the target hears it once");
    assert_eq!(trace(&*k), first, "a crash and restart land the trace of an uninterrupted run");
    drop(y);

    k.set_cursor(0).unwrap();
    let mut z = rig.boot(k.clone(), &clock, 3, cross(&again, Mode::Act)).unwrap();
    z.run_until_idle().unwrap();
    assert_eq!(trace(&*k), first, "replaying the whole journal adds nothing");
    assert_eq!(again.load(Ordering::SeqCst), 1);
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

pub fn run_all(rig: &dyn Rig) {
    redelivery_of_any_event(rig);
    crash_between_append_and_dispatch(rig);
    crash_during_dispatch_appends_nothing_twice(rig);
    crash_between_effect_request_and_outcome(rig);
    out_of_order_effect_outcomes(rig);
    effect_deadline_times_out(rig);
    determinism_under_replay(rig);
    handler_panic_quarantines_its_stream_only(rig);
    schedule_rows_survive_restart_and_fire_once(rig);
    observe_shadows_writes_runs_probes_and_never_replays_them(rig);
    halt_holds_writes_runs_probes_and_resume_releases_once_in_order(rig);
    emit_to_wakes_another_stream_once_across_a_crash(rig);
    emit_to_is_shadowed_under_observe_and_runs_under_halt(rig);
}
