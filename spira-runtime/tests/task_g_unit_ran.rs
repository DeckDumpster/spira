//! Source `unit.ran` (`wiki/projects/spira/designs/tasks/source-unit-ran.md`) on the
//! VirtualDriver: a `ScriptedSource` standing in submits the runtime's `unit.ran` shape and a stub
//! P2 consumer recognises its own run by `UnitRan::key_for` and not another task's; the real
//! D-Bus source is refused under virtual time. Red until the port lands: the real source's `run`
//! is `unimplemented!`.

use std::sync::Arc;

use runtime::conformance::trace;
use runtime::host::{EffectRunner, Submit, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::{Clock, VirtualClock};
use runtime::sources::{ScriptedSource, Source, Sources};
use runtime::units::{Exit, UnitHandle};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKind, EventKindTag, Handler, MemJournal, Projection, Registration};
use spira_runtime::tasks::g::unit_ran::*;
use tokio::sync::mpsc;

/// A P2 task's stand-in: hears every `unit.ran`, acts on its own.
struct Consumer {
    task: &'static str,
}
impl Handler for Consumer {
    fn interests(&self) -> &[EventKindTag] {
        &[UNIT_RAN]
    }
    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        match UnitRan::from_event_kind(&ev.kind).and_then(|r| r.key_for(self.task).map(|k| (k, r.exit))) {
            Some((k, exit)) => vec![Command::Emit(EventKind::new("consumed", &format!("{}|{}:{k}:{exit:?}@{}", ev.stream.0, self.task, ctx.now())))],
            None => vec![],
        }
    }
}

struct NoEffects;
impl EffectRunner for NoEffects {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        Err(format!("no runner for effect {}", spec.name))
    }
}

fn driver(clock: &VirtualClock, j: Arc<dyn Journal>, sources: Sources) -> runtime::Result<VirtualDriver> {
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let regs = vec![Registration::new("ops.stub", Consumer { task: "persona.ops" }), Registration::new("archivist.stub", Consumer { task: "persona.archivist" })];
    let engine = Engine::open(j, c, regs, 1).unwrap();
    VirtualDriver::new(engine, clock.clone(), Arc::new(NoEffects)).with_sources(sources)
}

fn ended(task: &str, key: &str, exit: Exit, at: u64) -> (u64, Submit) {
    let r = UnitRan { handle: UnitHandle::of(task, key).unwrap(), exit, duration_ms: 1_000 };
    (at, Submit::new(&format!("{UNIT_RAN}/{}/{at}", r.handle.name), UNITS_STREAM, r.event_kind()))
}

fn scripted(clock: &VirtualClock, script: Vec<(u64, Submit)>) -> Sources {
    let mut s = Sources::default();
    s.register(Box::new(ScriptedSource::new("unit.ran.script", Arc::new(clock.clone()), script))).unwrap();
    s
}

#[test]
fn the_declared_event_reaches_the_task_that_started_the_unit_and_only_it() {
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(0));
    let script = vec![ended("persona.ops", "r1", Exit::Code(0), 100), ended("persona.archivist", "r2-s1", Exit::Code(124), 200), ended("someone.else", "x", Exit::Code(1), 300)];
    let mut v = driver(&clock, j.clone(), scripted(&clock, script)).unwrap();
    v.run_until(1_000).unwrap();
    let consumed: Vec<String> = trace(&*j).into_iter().filter(|t| t.2 == "consumed").map(|t| t.3).collect();
    assert_eq!(consumed, ["units|persona.ops:r1:Code(0)@100", "units|persona.archivist:r2-s1:Code(124)@200"]);
}

#[test]
fn the_real_source_is_refused_under_virtual_time_naming_it() {
    let clock = VirtualClock::new(0);
    let mut s = scripted(&clock, vec![]);
    s.register(Box::new(UnitRanDbus)).unwrap();
    let err = driver(&clock, MemJournal::new(), s).err().expect("a source that reads the world is refused");
    assert!(err.0.contains(NAME) && !err.0.contains("unit.ran.script"), "{err}");
    assert_eq!(BACKEND, "systemd-dbus");
}

#[test]
fn the_real_source_returns_once_its_channel_is_closed() {
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    Box::new(UnitRanDbus).run(tx);
}
