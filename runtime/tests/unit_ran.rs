//! Completions as a source: a task that starts a unit hears its own `unit.ran` through the
//! journal, on the virtual clock, and recognises it by the unit's name, never another task's.

use std::sync::Arc;

use runtime::conformance::trace;
use runtime::host::{EffectRunner, Submit, VirtualDriver};
use runtime::journal::Journal;
use runtime::pools::EffectClass;
use runtime::ports::{Clock, VirtualClock};
use runtime::sources::{ScriptedSource, Sources, UnitRanSource, UNITS_STREAM};
use runtime::units::{self, Exit, ScriptedUnits, UnitRan, UnitSpec, UNIT_RAN, UNIT_START};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration, EFFECT_SUCCEEDED};

/// A P2-shaped stub: `go <task>` starts one unit under the naming rule; its `unit.ran` ends it.
struct Machine {
    task: &'static str,
    key: &'static str,
}

impl Handler for Machine {
    fn interests(&self) -> &[EventKindTag] {
        &["go", UNIT_RAN, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        match ev.kind.tag.as_str() {
            "go" if ev.kind.body == self.task => {
                let spec = UnitSpec { task: self.task.into(), key: self.key.into(), argv: vec![format!("/bin/{}", self.task)], env: vec![], cwd: None };
                vec![Command::Effect(EffectSpec { name: UNIT_START.into(), input: serde_json::to_string(&spec).unwrap(), deadline_ms: 5_000 })]
            }
            UNIT_RAN => match UnitRan::from_event_kind(&ev.kind).and_then(|r| r.key_for(self.task).map(|k| (k, r.exit))) {
                Some((k, exit)) => vec![Command::Emit(EventKind::new("done", &format!("{}:{k}:{exit:?}@{}", self.task, ctx.now())))],
                None => vec![],
            },
            _ => vec![],
        }
    }
}

struct UnitRunner(Arc<ScriptedUnits>);

impl EffectRunner for UnitRunner {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        units::effect(&*self.0, &spec.name, &spec.input)
    }

    fn class(&self, _: &EffectSpec) -> EffectClass {
        EffectClass::Unit
    }
}

fn tags(j: &dyn Journal, tag: &str) -> Vec<(String, String, u64)> {
    trace(j).into_iter().filter(|t| t.2 == tag).map(|t| (t.1, t.3, t.4)).collect()
}

#[test]
fn each_task_hears_its_own_units_end_and_not_another_tasks() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let scripted = Arc::new(ScriptedUnits::new(c.clone()));
    scripted.script("alpha", 5_000, Exit::Code(0));
    scripted.script("beta", 9_000, Exit::Code(3));
    let regs = vec![Registration::new("alpha", Machine { task: "alpha", key: "a1" }), Registration::new("beta", Machine { task: "beta", key: "b1" })];
    let engine = Engine::open(j.clone(), c.clone(), regs, 1).unwrap();
    let mut sources = Sources::default();
    let go = |at: u64, task: &str| (at, Submit::new(&format!("go-{task}"), "s", EventKind::new("go", task)));
    sources.register(Box::new(ScriptedSource::new("go", c.clone(), vec![go(100, "alpha"), go(200, "beta")]))).unwrap();
    sources.register(Box::new(UnitRanSource::scripted("unit-ran", scripted.clone(), c.clone()))).unwrap();
    let mut v = VirtualDriver::new(engine, clock.clone(), Arc::new(UnitRunner(scripted.clone()))).with_sources(sources).unwrap();
    v.run_until(60_000).unwrap();

    assert_eq!(scripted.calls(), ["start spira-run-alpha-a1.service", "start spira-run-beta-b1.service"]);
    let ran = tags(&*j, UNIT_RAN);
    assert_eq!(ran.iter().map(|(s, _, at)| (s.as_str(), *at)).collect::<Vec<_>>(), [(UNITS_STREAM, 5_100), (UNITS_STREAM, 9_200)], "each end submitted once, when the virtual clock reaches it");
    let done: Vec<String> = tags(&*j, "done").into_iter().map(|(_, b, _)| b).collect();
    assert_eq!(done, ["alpha:a1:Code(0)@5100", "beta:b1:Code(3)@9200"], "both handlers saw both ends; each acted only on its own");
}

#[test]
fn a_polled_source_does_not_run_under_virtual_time() {
    let clock = VirtualClock::new(0);
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut sources = Sources::default();
    sources.register(Box::new(UnitRanSource::polled("unit-ran", Arc::new(ScriptedUnits::new(c.clone())), c.clone(), std::time::Duration::from_millis(5)))).unwrap();
    let engine = Engine::open(MemJournal::new(), c.clone(), vec![], 1).unwrap();
    let err = VirtualDriver::new(engine, clock, Arc::new(UnitRunner(Arc::new(ScriptedUnits::new(c))))).with_sources(sources).err().expect("refused");
    assert!(err.0.contains("unit-ran"), "{err}");
}

#[test]
fn a_polled_source_on_its_own_thread_submits_each_end_once() {
    let clock = VirtualClock::new(0);
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let scripted = Arc::new(ScriptedUnits::new(c.clone()));
    scripted.script("alpha", 10, Exit::Code(0));
    units::effect(&*scripted, UNIT_START, r#"{"task":"alpha","key":"k","argv":["/bin/a"],"env":[],"cwd":null}"#).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let mut sources = Sources::default();
    sources.register(Box::new(UnitRanSource::polled("unit-ran", scripted, c, std::time::Duration::from_millis(5)))).unwrap();
    let handles = sources.start(tx).unwrap();
    clock.advance_to(10);
    let s = rx.blocking_recv().expect("the end arrives");
    assert_eq!((s.id.as_str(), s.stream.as_str()), ("unit.ran/spira-run-alpha-k.service/10", UNITS_STREAM));
    assert_eq!(UnitRan::from_event_kind(&s.kind).and_then(|r| r.key_for("alpha")).as_deref(), Some("k"));
    std::thread::sleep(std::time::Duration::from_millis(30)); // clock-safe: a second drain must find nothing new
    assert!(rx.try_recv().is_err(), "an end is submitted once");
    drop(rx);
    handles.into_iter().for_each(|h| h.join().unwrap());
}
