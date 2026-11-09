//! The service's `unit.ran` source, as `main.rs` registers it, under the virtual driver: a booted
//! task's run lands on that task's own stream, so the handler that started the unit hears its
//! end there and its answer is journaled there too; a unit no booted task owns stays on `units`.

use std::sync::Arc;

use runtime::conformance::trace;
use runtime::host::{EffectRunner, Submit, VirtualDriver};
use runtime::journal::Journal;
use runtime::pools::EffectClass;
use runtime::ports::{Clock, VirtualClock};
use runtime::sources::{ScriptedSource, Sources, UNITS_STREAM};
use runtime::units::{self, Exit, ScriptedUnits, UnitRan, UnitSpec, UNIT_RAN, UNIT_START};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration};

/// Stands in for a booted task: answers only what arrives on its own stream.
struct Owned {
    task: String,
    stream: String,
}

impl Handler for Owned {
    fn interests(&self) -> &[EventKindTag] {
        &["go", UNIT_RAN]
    }

    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != self.stream {
            return vec![];
        }
        match ev.kind.tag.as_str() {
            "go" => {
                let spec = UnitSpec { task: self.task.clone(), key: "k".into(), argv: vec!["/bin/true".into()], env: vec![], cwd: None };
                vec![Command::Effect(EffectSpec::by_input(UNIT_START, serde_json::to_string(&spec).unwrap(), 5_000))]
            }
            UNIT_RAN => UnitRan::from_event_kind(&ev.kind).and_then(|r| r.key_for(&self.task)).map(|k| vec![Command::Emit(EventKind::new("done", &k))]).unwrap_or_default(),
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

#[test]
fn a_booted_tasks_unit_ran_lands_on_its_own_stream_and_moves_only_it() {
    let mut booted = spira_runtime::tasks::all().filter(|t| t.name != "heartbeat");
    let (one, other) = (booted.next().expect("a booted task"), booted.next().expect("a second booted task"));
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let scripted = Arc::new(ScriptedUnits::new(c.clone()));
    scripted.script(one.name, 5_000, Exit::Code(0));
    scripted.script("planted.no_such_task", 6_000, Exit::Code(0));
    units::effect(&*scripted, UNIT_START, r#"{"task":"planted.no_such_task","key":"g","argv":["/bin/g"],"env":[],"cwd":null}"#).unwrap();
    let regs = [one, other].iter().map(|t| Registration::new(t.name, Owned { task: t.name.into(), stream: t.stream().into() })).collect();
    let engine = Engine::open(j.clone(), c.clone(), regs, 1).unwrap();
    let mut sources = Sources::default();
    sources.register(Box::new(ScriptedSource::new("go", c.clone(), vec![(100, Submit::new("go", one.stream(), EventKind::new("go", "")))]))).unwrap();
    sources.register(Box::new(spira_runtime::sources::units::scripted(scripted.clone(), c.clone()))).unwrap();
    let mut v = VirtualDriver::new(engine, clock.clone(), Arc::new(UnitRunner(scripted))).with_sources(sources).unwrap();
    v.run_until(60_000).unwrap();

    let t = trace(&*j);
    let ran: Vec<&str> = t.iter().filter(|e| e.2 == UNIT_RAN).map(|e| e.1.as_str()).collect();
    assert_eq!(ran, [one.stream(), UNITS_STREAM], "the booted task's run on its stream; the unowned one on units");
    let done: Vec<(&str, &str)> = t.iter().filter(|e| e.2 == "done").map(|e| (e.1.as_str(), e.3.as_str())).collect();
    assert_eq!(done, [(one.stream(), "k")], "{} moved on its own stream; {} did not", one.name, other.name);
}
