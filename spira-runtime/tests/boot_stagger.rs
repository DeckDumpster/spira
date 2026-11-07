//! The service's tasks take their first ticks one after another at boot, `BOOT_STAGGER_MS`
//! apart in `tasks::all()` order, never all at the same instant.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::host::{EffectRunner, VirtualDriver};
use runtime::journal::Journal;
use runtime::ports::{Clock, VirtualClock};
use runtime::{Command, Ctx, EffectSpec, Engine, Event, EventKindTag, Handler, MemJournal, Projection, Registration};
use spira_runtime::{start_tasks, tasks, BOOT_STAGGER_MS};

/// Hears every task's boot tick and records when each stream first heard one.
struct FirstTicks {
    tags: Vec<EventKindTag>,
    at: Arc<Mutex<BTreeMap<String, u64>>>,
}

impl Handler for FirstTicks {
    fn interests(&self) -> &[EventKindTag] {
        &self.tags
    }
    fn handle(&self, ctx: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        self.at.lock().unwrap().entry(ev.stream.0.clone()).or_insert(ctx.now());
        vec![]
    }
}

struct NoEffects;
impl EffectRunner for NoEffects {
    fn run(&self, _: &str, spec: &EffectSpec) -> Result<String, String> {
        Err(format!("no effect expected: {}", spec.name))
    }
}

#[test]
fn every_task_first_ticks_in_turn_boot_stagger_apart() {
    let boots = tasks::boots();
    assert!(boots.len() > 10, "the service boots its tasks: {}", boots.len());
    let mut tags: Vec<EventKindTag> = boots.iter().map(|(_, _, k)| -> EventKindTag { Box::leak(k.tag.clone().into_boxed_str()) }).collect();
    tags.sort();
    tags.dedup();
    let at = Arc::new(Mutex::new(BTreeMap::new()));
    let (j, clock): (Arc<dyn Journal>, _) = (MemJournal::new(), VirtualClock::new(1_000));
    let c: Arc<dyn Clock> = Arc::new(clock.clone());
    let mut engine = Engine::open(j, c, vec![Registration::new("first-ticks", FirstTicks { tags, at: at.clone() })], 1).unwrap();
    start_tasks(&mut engine);
    let mut v = VirtualDriver::new(engine, clock, Arc::new(NoEffects));
    v.run_until(1_000 + boots.len() as u64 * BOOT_STAGGER_MS + 1).unwrap();
    let at = at.lock().unwrap();
    let want: BTreeMap<String, u64> = boots.iter().enumerate().map(|(i, (_, s, _))| (s.to_string(), 1_000 + i as u64 * BOOT_STAGGER_MS)).collect();
    assert_eq!(*at, want, "each task's first tick lands at its own instant, in order");
}
