//! What every family-A task shares: the tick half of the P1 shape (one observe probe, the next
//! tick armed), the carried state read back from its own stream, decide's acts turned into
//! commands, the config each task pins at boot, and the checks its acceptance tests share.
//!
//! A task module keeps its own contract: its snapshot, its carry, its `decide`, its act table.
//! This file holds only the plumbing that would otherwise be copied thirteen times.

use std::collections::BTreeMap;

use runtime::calendar::Calendar;
use runtime::event::*;
use runtime::journal::Journal;
use runtime::taskrig::TaskRig;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::watchers::{answers_observe, carried};

/// One act `decide` asks for: the effect by its contract name, the idempotency key the
/// contract's Act table gives it, and its input.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Act {
    pub effect: String,
    pub key: String,
    pub input: String,
}

impl Act {
    pub fn new(effect: &str, key: impl Into<String>) -> Self {
        Act { effect: effect.into(), key: key.into(), input: String::new() }
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }
}

/// The fixed names of one task: its stream, its tick, the tag its carry is emitted under, and
/// its observe probe with that probe's deadline.
pub struct Shape {
    pub name: &'static str,
    pub stream: &'static str,
    pub tick: EventKindTag,
    pub state: EventKindTag,
    pub observe: &'static str,
    pub observe_deadline_ms: u64,
}

/// When the next tick falls: a fixed interval, or the next instant a calendar spec elapses (S8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cadence {
    Every(u64),
    Calendar(Calendar),
}

impl Cadence {
    pub fn next_after(&self, at: u64) -> u64 {
        match self {
            Cadence::Every(ms) => at + ms,
            Cadence::Calendar(c) => c.next_after(at),
        }
    }
}

/// The P1 tick: one memory-only observe probe carrying `input`, and the next tick armed.
pub fn on_tick(shape: &Shape, input: String, next_at: u64) -> Vec<Command> {
    vec![
        Command::Probe(EffectSpec { name: shape.observe.into(), input, deadline_ms: shape.observe_deadline_ms }),
        Command::Timer { at: next_at, kind: EventKind::new(shape.tick, "") },
    ]
}

/// The snapshot `ev` carries when it is the success of this task's observe probe.
pub fn observed<S: DeserializeOwned>(shape: &Shape, view: &Projection, ev: &Event) -> Option<S> {
    if ev.stream.0 != shape.stream || ev.kind.tag != EFFECT_SUCCEEDED || !answers_observe(view, ev, shape.observe) {
        return None;
    }
    serde_json::from_str(&ev.kind.body).ok()
}

/// The task's carried state: the body of its latest `shape.state` event, or the default.
pub fn carry<C: DeserializeOwned + Default>(shape: &Shape, view: &Projection) -> C {
    carried(view, &StreamKey(shape.stream.into()), shape.state)
}

/// The deadline the contract's Act table gives `effect`.
pub fn deadline(acts: &[(&str, u64)], effect: &str) -> Option<u64> {
    acts.iter().find(|(n, _)| *n == effect).map(|(_, d)| *d)
}

/// `decide`'s answer as commands: the carry emitted only when it changed, an act named in `emits` as
/// an `Emit` of that tag (a hand-off such as `call.wanted`), and every other act as a
/// journaled effect under the deadline its Act table row gives. An act the table does not name
/// is a bug in `decide`, so it panics naming it rather than running under a made-up deadline.
pub fn commands<C: Serialize + PartialEq>(shape: &Shape, table: &[(&str, u64)], emits: &[EventKindTag], old: &C, new: &C, acts: Vec<Act>) -> Vec<Command> {
    let mut out = vec![];
    if old != new {
        out.push(Command::Emit(EventKind::new(shape.state, &serde_json::to_string(new).unwrap_or_default())));
    }
    for a in acts {
        if emits.contains(&a.effect.as_str()) {
            out.push(Command::Emit(EventKind::new(&a.effect, &a.input)));
            continue;
        }
        let deadline_ms = deadline(table, &a.effect).unwrap_or_else(|| panic!("{}: decide asked for {}, which its Act table does not name", shape.name, a.effect));
        out.push(Command::Effect(EffectSpec { name: a.effect, input: a.input, deadline_ms }));
    }
    out
}

/// Every key in `keys` read through `get`, refusing on the first that does not resolve.
pub fn pinned(keys: &[&str], get: &dyn Fn(&str) -> Result<String, String>) -> Result<BTreeMap<String, String>, String> {
    keys.iter().map(|k| get(k).map(|v| (k.to_string(), v))).collect()
}

/// The keys of `task`, pinned at boot through `spira_config::process::cfg`. A key that does not
/// resolve refuses the build naming it: there is no default.
pub fn pin(task: &str, keys: &[&str]) -> BTreeMap<String, String> {
    pinned(keys, &spira_config::process::cfg).unwrap_or_else(|e| panic!("{task} refuses to build: {e}"))
}

/// The unported-handler signal every stub's `decide` raises.
#[macro_export]
macro_rules! not_ported {
    ($task:expr) => {
        unimplemented!("{}: not ported yet", $task)
    };
}

// ---------------------------------------------------------------------------------------------
// What the acceptance tests share.

/// The row named `name` among the tasks the service boots.
pub fn registered(name: &str) -> Option<&'static super::super::Task> {
    super::super::all().find(|t| t.name == name)
}

/// The row named `name` staged in family A until its port lands.
pub fn staged(name: &str) -> &'static super::super::Task {
    super::STAGED.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("{name} has no row in tasks::a::STAGED"))
}

/// Panics with the quarantine message when a handler on `stream` panicked or was refused. A
/// poisoned stream stops handling, so without this check a stub that panics on its first
/// answer would pass every silence and determinism check by doing nothing.
pub fn assert_unpoisoned(rig: &TaskRig, stream: &str) {
    if let Some(p) = rig.journal.poisoned(&StreamKey(stream.into())).expect("read poison") {
        panic!("{stream} was quarantined by {}: {}", p.handler, p.message);
    }
}

/// T3's read-back: the carry last recorded under `state` is the carry the next observe probe was
/// handed. Panics when nothing was recorded, so a task that never records fails it.
pub fn assert_read_equals_record<C: DeserializeOwned + PartialEq + std::fmt::Debug>(rig: &TaskRig, state: &str, observe: &str) {
    let recorded = rig.journaled().into_iter().rev().find(|e| e.kind.tag == state).unwrap_or_else(|| panic!("nothing was recorded under {state}"));
    let recorded: C = serde_json::from_str(&recorded.kind.body).unwrap_or_else(|e| panic!("{state} does not read back: {e}"));
    let read = rig.requested().iter().rev().find(|q| q.name == observe).unwrap_or_else(|| panic!("{observe} was never asked"));
    let read: C = serde_json::from_str(&read.input).unwrap_or_else(|e| panic!("{observe} input does not read back: {e}"));
    assert_eq!(read, recorded, "the carry read is not the carry recorded");
}

/// An observe double answering each of `worlds` in turn for `every` answers, round and round:
/// a world that changes on a fixed beat, the same on every run.
pub fn cycling<S: Serialize + Send + Sync + 'static>(worlds: Vec<S>, every: usize) -> impl Fn(&str) -> Result<String, String> + Send + Sync + 'static {
    let answered = std::sync::atomic::AtomicUsize::new(0);
    move |_| {
        let n = answered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        serde_json::to_string(&worlds[(n / every.max(1)) % worlds.len()]).map_err(|e| e.to_string())
    }
}

/// The lines of `src` that start a process or reach for one: the T4 static scan.
pub fn spawns(src: &str) -> Vec<String> {
    const MARKS: [&str; 4] = ["Command::new", "std::process", "process::Command", "posix_spawn"];
    src.lines().filter(|l| MARKS.iter().any(|m| l.contains(m))).map(|l| l.trim().to_string()).collect()
}

/// The source of the family-A task module `file` (a stem under `src/tasks/a/`).
pub fn module_source(file: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tasks/a").join(format!("{file}.rs"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Each of `keys` that has no registration under `spira/conf.d` in this tree.
pub fn unregistered(keys: &[&str]) -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d");
    keys.iter().filter(|k| !dir.join(k).exists()).map(|k| k.to_string()).collect()
}

/// Every effect name the service routes to a runner: the real half of T4.
pub fn real_names() -> Vec<&'static str> {
    super::super::routes().keys().copied().collect()
}

/// The acceptance tests every family-A task instantiates the same way (§6 T1, T2, T4, T6), and
/// the rig they share, expanded into a `tests/task_a_<task>.rs` file. The file adds its own T3
/// and T5 from its contract, over the `rig`, `idle()` and `busy()` this leaves in scope.
///
/// - `module`: the task's module under `tasks::a`; `registration`: its handler, built without
///   config; `tick`: the kind of an ordinary tick; `first_timer_ms`: when the boot tick arms the
///   next one; `idle`/`busy`: the contract's idle fixture and a world with work in it;
///   `budget`: the contract's idle ceiling in journaled events per hour (the silence law then
///   holds an idle run to zero; a busy run is checked for determinism only).
#[macro_export]
macro_rules! family_a_acceptance {
    (module: $m:ident, registration: $reg:expr, tick: $tick:expr, first_timer_ms: $first:expr, budget: $budget:expr $(,)?) => {
        #[allow(unused_imports)]
        use $crate::tasks::a::$m::*;
        #[allow(unused_imports)]
        use $crate::tasks::a::kit::{self, Act};
        #[allow(unused_imports)]
        use ::runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, DAY_MS, PLANTED_UNKNOWN};

        fn registration() -> ::runtime::event::Registration {
            $reg
        }

        /// A rig whose observe answers through `observe`, every other effect succeeding with
        /// `{}`, after one ordinary tick.
        #[allow(dead_code)]
        fn rig_with(seed: u64, observe: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static) -> TaskRig {
            let mut r = TaskRig::new(seed, registration()).unwrap().double(OBSERVE, observe);
            for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
                r = r.double(e, |_| Ok("{}".into()));
            }
            r.tick(STREAM, $tick).unwrap();
            r
        }

        #[allow(dead_code)]
        fn rig(seed: u64, world: Snapshot) -> TaskRig {
            let world = ::std::sync::Arc::new(::std::sync::Mutex::new(world));
            rig_with(seed, move |_| Ok(::serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        }

        #[test]
        fn t1_registered_boots_and_arms_its_tick() {
            let row = kit::registered(NAME).unwrap_or_else(|| panic!("{} is not in tasks::all(): its row waits in tasks::a::STAGED until the port lands", NAME));
            let (_, stream, kind) = (row.boot)();
            assert_eq!(stream, STREAM);
            let mut r = TaskRig::new(1, registration()).unwrap();
            r.tick(stream, kind).unwrap();
            assert_eq!(r.engine().next_timer_at(), Some($first), "the boot tick arms the next");
        }

        #[test]
        fn t1_the_staged_row_is_this_contract() {
            let row = kit::staged(NAME);
            assert_eq!(row.effects, EFFECTS);
            assert_eq!(row.stream(), STREAM);
            let mut r = TaskRig::new(1, registration()).unwrap();
            let (_, stream, kind) = (row.boot)();
            r.tick(stream, kind).unwrap();
            assert_eq!(r.engine().next_timer_at(), Some($first), "the staged boot arms the next tick");
            assert_eq!(kit::unregistered(KEYS), Vec::<String>::new(), "every pinned key is registered");
            assert_eq!(kit::unregistered(UNREGISTERED).len(), UNREGISTERED.len(), "a key listed as unregistered has since been registered: move it to KEYS");
            assert!(ACTS.iter().all(|(n, _)| EFFECTS.contains(n) || $crate::tasks::SHARED.iter().any(|(s, _)| s == n)), "every act is a declared effect");
        }

        #[test]
        fn t2_an_idle_tick_is_the_probe_only() {
            let mut r = rig(1, idle());
            kit::assert_unpoisoned(&r, STREAM);
            assert_silent_tick(&mut r, STREAM);
            kit::assert_unpoisoned(&r, STREAM);
        }

        #[test]
        fn t4_every_effect_has_a_real_runner_and_a_sim_double() {
            let sim = ::sim::doubles::service().names();
            let sim: Vec<&str> = sim.iter().map(String::as_str).collect();
            let real = kit::real_names();
            assert!(!real.contains(&PLANTED_UNKNOWN) && !sim.contains(&PLANTED_UNKNOWN));
            assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
        }

        #[test]
        fn t4_a_planted_bogus_effect_is_refused_and_the_module_spawns_nothing() {
            let sim = ::sim::doubles::service().names();
            let sim: Vec<&str> = sim.iter().map(String::as_str).collect();
            let bogus = format!("{}.bogus", NAME);
            assert!(effects_runnable(&[bogus.as_str()], &kit::real_names(), &sim).is_err());
            assert_eq!(kit::spawns(&kit::module_source(stringify!($m))), Vec::<String>::new());
        }

        #[test]
        fn t6_two_busy_virtual_days_are_deterministic() {
            let r = assert_deterministic(|seed| rig_with(seed, kit::cycling(vec![idle(), busy()], 30)), 1, 2);
            kit::assert_unpoisoned(&r, STREAM);
        }

        #[test]
        fn t6_two_idle_virtual_days_are_deterministic_under_budget_and_journal_nothing() {
            let r = assert_deterministic(|seed| rig(seed, idle()), 1, 2);
            kit::assert_unpoisoned(&r, STREAM);
            assert_journal_budget(&r, $budget);
            assert_journal_budget(&r, 0);
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spawn_scan_sees_a_planted_command_and_passes_a_clean_line() {
        assert_eq!(spawns("let x = 1;\nstd::process::Command::new(\"git\")"), ["std::process::Command::new(\"git\")"]);
        assert!(spawns("let x = 1;").is_empty());
    }

    #[test]
    fn the_registration_scan_finds_a_registered_key_and_refuses_a_planted_one() {
        assert_eq!(unregistered(&["SPIRA_RUN", "SPIRA_PLANTED_NOT_A_KEY"]), ["SPIRA_PLANTED_NOT_A_KEY"]);
    }

    #[test]
    fn pinned_refuses_naming_the_first_key_that_does_not_resolve() {
        let get = |k: &str| if k == "A" { Ok("1".to_string()) } else { Err(format!("{k} unknown")) };
        assert_eq!(pinned(&["A"], &get).unwrap()["A"], "1");
        assert_eq!(pinned(&["A", "B"], &get), Err("B unknown".into()));
    }

    #[test]
    fn an_act_outside_the_table_is_a_decide_bug_not_a_default_deadline() {
        let shape = Shape { name: "t", stream: "s", tick: "t.tick", state: "t.state", observe: "t.observe", observe_deadline_ms: 1 };
        let ok = commands(&shape, &[("t.do", 5)], &[], &0, &1, vec![Act::new("t.do", "k")]);
        assert!(matches!(&ok[..], [Command::Emit(_), Command::Effect(e)] if e.deadline_ms == 5));
        assert!(commands(&shape, &[], &[], &0, &0, vec![]).is_empty(), "an unchanged carry emits nothing");
        assert!(std::panic::catch_unwind(|| commands(&shape, &[], &[], &0, &0, vec![Act::new("t.bogus", "k")])).is_err());
    }
}
