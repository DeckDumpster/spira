//! The service's own task table, booted as the service boots it, run for virtual hours in the
//! simulator with every effect answered by the quiet world's doubles: one seed gives one trace,
//! and no process is spawned.

use std::sync::Arc;

use runtime::ports::{CountingProc, Proc, ProcSpec};
use runtime::{Command, Ctx, EffectSpec, Event, EventKindTag, Handler, Projection, Registration};
use sim::Sim;
use spira_runtime::{registrations, tasks};

use crate::common;
use common::settings;

/// Virtual hours, not days: with all 82 rows booted a virtual hour costs about 7 s of wall time
/// in a debug build (every effect outcome is dispatched to every handler interested in its tag,
/// each dispatch building a wide event), so days would take minutes per run.
const HOUR: u64 = 3_600_000;

struct Run {
    trace: Vec<(String, String, String, String, u64)>,
    asked: Vec<String>,
    spawned: u64,
}

fn run(seed: u64, hours: u64, extra: Option<Registration>) -> Run {
    let proc = CountingProc::refusing();
    let forks = proc.clone();
    let doubles = Arc::new(sim::doubles::service().double("fork", move |_| {
        let spec = ProcSpec { prog: "planted".into(), args: vec![], stdin: vec![], deadline: std::time::Duration::from_secs(1) };
        forks.run(&spec).map(|_| String::new())
    }));
    let mut sim = Sim::new(seed);
    for r in registrations(&settings()) {
        sim.add_workflow(r);
    }
    if let Some(r) = extra {
        sim.add_workflow(r);
        sim.submit(HOUR / 2, "plant", "plant", "plant", "");
    }
    sim.set_effects(doubles.clone());
    for (_, stream, kind) in tasks::boots() {
        sim.boot_workflow(0, stream, &kind.tag, &kind.body);
    }
    sim.run_until(hours * HOUR);
    assert!(sim.effects_run() > 0);
    Run { trace: sim.workflow_trace(), asked: doubles.asked(), spawned: proc.spawned() }
}

#[test]
fn the_task_table_runs_virtual_hours_deterministically_and_spawns_nothing() {
    let regs = registrations(&settings()).len();
    assert!(regs >= 15, "the service's tasks: {regs}");
    assert_eq!(tasks::boots().len(), regs, "every task is booted");
    let a = run(3, 2, None);
    let b = run(3, 2, None);
    assert_eq!(a.trace, b.trace, "same seed, same journal");
    assert_eq!(a.asked, b.asked, "same seed, same effects in the same order");
    assert!(!a.asked.is_empty());
    let distinct: std::collections::BTreeSet<&String> = a.asked.iter().collect();
    assert!(distinct.len() >= 10, "most tasks reached their observe: {distinct:?}");
    assert_eq!(a.spawned, 0, "no process is spawned for an in-process task");
}

/// The positive control for the count: an effect whose double forks is seen.
struct Plant;
impl Handler for Plant {
    fn interests(&self) -> &[EventKindTag] {
        &["plant"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
        vec![Command::Effect(EffectSpec { name: "fork".into(), input: String::new(), deadline_ms: 1000 })]
    }
}

#[test]
fn a_double_that_forks_is_counted() {
    let r = run(3, 1, Some(Registration::new("plant", Plant)));
    assert_eq!(r.spawned, 1);
    assert!(r.trace.iter().any(|t| t.2 == runtime::EFFECT_FAILED), "the refused fork is answered as a failure");
}
