//! Effect doubles: the runner a simulated host answers effects with. A name with no double is
//! refused with the production runner's own text, so the handler sees what it would see live.

use std::collections::BTreeMap;
use std::sync::Mutex;

use runtime::host::EffectRunner;
use runtime::EffectSpec;

pub type Double = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

#[derive(Default)]
pub struct Doubles {
    by_name: BTreeMap<String, Double>,
    asked: Mutex<Vec<String>>,
}

impl Doubles {
    pub fn double(mut self, name: &str, f: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> Self {
        self.by_name.insert(name.into(), Box::new(f));
        self
    }

    /// Every effect name this runner has a double for, sorted.
    pub fn names(&self) -> Vec<String> {
        self.by_name.keys().cloned().collect()
    }

    /// Every effect name asked for, in the order it was run.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl EffectRunner for Doubles {
    fn run(&self, _key: &str, spec: &EffectSpec) -> Result<String, String> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).push(spec.name.clone());
        match self.by_name.get(&spec.name) {
            Some(f) => f(&spec.input),
            None => Err(format!("no runner for effect {}", spec.name)),
        }
    }
}

/// Every effect the service's task table (spira-runtime's `tasks::TASKS`) can ask for. Named
/// here, not read from the table, so a task that gains an effect fails T4 until the simulator
/// can answer it.
pub const SERVICE_EFFECTS: &[&str] = &[
    "heartbeat.ping",
    "inbox.append",
    "concierge.wake",
    "operator.note",
    "round-duty.observe",
    "inbox-keeper.observe",
    "pr-notify.observe",
    "asks.observe",
    "design-asks.observe",
    "brain-dirty.observe",
    "supersede-duty.observe",
    "supersede-duty.act",
    "bead-sweep.observe",
    "bead-sweep.repair",
    "publish-backlog.observe",
    "repo-events.observe",
    "overrides.apply",
    "view.observe",
    "view.flip",
    "view.forget",
    "reconciler.observe",
    "reconciler.record",
    "reconciler.remedy",
    "reconciler.mail",
    "reconciler-flow.observe",
    "reconciler-flow.status",
    "reconciler-flow.mail",
];

/// The service's task table in a quiet world: every effect succeeds with an empty object. An
/// act does nothing; an observe whose snapshot needs fields reads as no answer, so the task
/// stays idle. A scenario that needs a world replaces the doubles it cares about.
pub fn service() -> Doubles {
    SERVICE_EFFECTS.iter().fold(Doubles::default(), |d, name| d.double(name, |_| Ok("{}".to_string())))
}
