//! `TaskRig`: one task's registration booted alone on a `MemJournal` and a `VirtualClock`, its
//! effects answered by scripted doubles. Every per-task acceptance test is written against it.
//!
//! The contract:
//!
//! - `TaskRig::new(seed, reg)` boots the engine; nothing is handed to the handler until a tick.
//!   The registration's mode holds: an `Observe` task's writes are journaled `effect.shadowed`,
//!   never handed to a double, and listed by `shadowed()`; its probes run.
//! - `double(name, f)` scripts the answer to effect `name`: `Ok` completes it `Succeeded`, `Err`
//!   completes it `Failed`. An effect with no double is completed `Failed` with the production
//!   refusal text, `no runner for effect {name}`, and listed in `refused()`.
//! - Every effect and probe is answered synchronously by `run_until_idle`, so after any call
//!   returns none is pending and virtual time has not moved on its account.
//! - `tick(stream, kind)` hands `kind` to the stream's handlers the way the service boots it
//!   (memory-only, never journaled), then runs until idle. The first tick on a stream also
//!   records `kind` as that stream's tick for `assert_silent_tick`.
//! - `advance(ms)` moves the clock forward, stopping at every timer, schedule row and deadline
//!   due on the way and running until idle at each, so a cadence fires as often as it would.
//! - `journaled()` is every event the journal holds; `requested()` is every effect and probe the
//!   handler asked for, in the order the rig answered them, with whether it was journaled.
//! - Same seed and same calls give the same `trace()` and the same `requested()`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::conformance::trace;
use crate::engine::{Engine, Executor, Outcome};
use crate::event::*;
use crate::journal::MemJournal;
use crate::ports::{Clock, VirtualClock};
use crate::Result;

pub type Double = Box<dyn Fn(&str) -> std::result::Result<String, String> + Send + Sync>;

/// One effect or probe as the rig answered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requested {
    pub name: String,
    pub key: String,
    pub input: String,
    pub at: u64,
    /// `true` for a `Command::Effect` (an `effect.requested` row in the journal); `false` for a
    /// memory-only `Command::Probe`.
    pub journaled: bool,
}

pub type Trace = Vec<(String, String, String, String, u64)>;

pub const DAY_MS: u64 = 86_400_000;
const HOUR_MS: u64 = 3_600_000;

pub struct TaskRig {
    pub journal: Arc<MemJournal>,
    pub clock: VirtualClock,
    engine: Engine,
    doubles: BTreeMap<String, Double>,
    requested: Vec<Requested>,
    refused: Vec<String>,
    ticks: BTreeMap<String, EventKind>,
    read_to: usize,
    journaled_requests: BTreeSet<String>,
    timing_out: bool,
    hung: BTreeSet<String>,
    parked: BTreeSet<String>,
}

impl TaskRig {
    pub fn new(seed: u64, reg: Registration) -> Result<Self> {
        let journal = MemJournal::new();
        let clock = VirtualClock::new(0);
        let engine = Engine::open(journal.clone(), Arc::new(clock.clone()), vec![reg], seed)?;
        Ok(TaskRig { journal, clock, engine, doubles: BTreeMap::new(), requested: vec![], refused: vec![], ticks: BTreeMap::new(), read_to: 0, journaled_requests: BTreeSet::new(), timing_out: false, hung: BTreeSet::new(), parked: BTreeSet::new() })
    }

    /// Stop the engine and open a new one on the same journal and clock: a runtime restart.
    pub fn reopen(&mut self, reg: Registration) -> Result<()> {
        self.engine = Engine::open(self.journal.clone(), Arc::new(self.clock.clone()), vec![reg], 0)?;
        self.parked.clear();
        self.hung.clear();
        Ok(())
    }

    /// Every effect and probe times out instead of being answered.
    pub fn timing_out(mut self) -> Self {
        self.timing_out = true;
        self
    }

    /// Effects named `name` are requested and never answered, until a [`reopen`](Self::reopen)
    /// finds them open and ends them lost; the reopened runtime answers them again.
    pub fn hanging(mut self, name: &str) -> Self {
        self.hung.insert(name.into());
        self
    }

    pub fn double(mut self, name: &str, f: impl Fn(&str) -> std::result::Result<String, String> + Send + Sync + 'static) -> Self {
        self.doubles.insert(name.into(), Box::new(f));
        self
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    pub fn tick(&mut self, stream: &str, kind: EventKind) -> Result<()> {
        self.ticks.entry(stream.into()).or_insert_with(|| kind.clone());
        self.engine.start(stream, kind);
        self.run_until_idle()
    }

    pub fn advance(&mut self, ms: u64) -> Result<()> {
        let target = self.now().saturating_add(ms);
        self.run_until_idle()?;
        while let Some(at) = self.engine.next_timer_at().filter(|&t| t <= target) {
            self.clock.advance_to(at);
            self.run_until_idle()?;
        }
        self.clock.advance_to(target);
        self.run_until_idle()
    }

    pub fn run_until_idle(&mut self) -> Result<()> {
        loop {
            self.engine.run_until_idle()?;
            let pending: Vec<_> = self.engine.pending_effects().into_iter().filter(|p| !self.parked.contains(&p.key)).collect();
            if pending.is_empty() {
                return Ok(());
            }
            let log = self.journal.log();
            for e in &log[self.read_to..] {
                if e.kind.tag == EFFECT_REQUESTED {
                    self.journaled_requests.insert(e.id.0.clone());
                }
            }
            self.read_to = log.len();
            for p in pending {
                let journaled = self.journaled_requests.contains(&p.key);
                self.requested.push(Requested { name: p.spec.name.clone(), key: p.spec.key.clone(), input: p.spec.input.clone(), at: p.requested_at, journaled });
                if self.hung.contains(&p.spec.name) {
                    self.parked.insert(p.key);
                    continue;
                }
                let outcome = match self.doubles.get(&p.spec.name).filter(|_| !self.timing_out) {
                    Some(f) => match f(&p.spec.input) {
                        Ok(o) => Outcome::Succeeded(o),
                        Err(e) => Outcome::Failed(e),
                    },
                    None if self.timing_out => Outcome::TimedOut,
                    None => {
                        self.refused.push(p.spec.name.clone());
                        Outcome::Failed(format!("no runner for effect {}", p.spec.name))
                    }
                };
                self.engine.complete(&p.key, outcome)?;
            }
        }
    }

    pub fn journaled(&self) -> Vec<Event> {
        self.journal.log()
    }

    pub fn trace(&self) -> Trace {
        trace(&*self.journal)
    }

    pub fn requested(&self) -> &[Requested] {
        &self.requested
    }

    /// The specs of the writes the engine shadowed instead of requesting, in journal order.
    pub fn shadowed(&self) -> Vec<EffectSpec> {
        #[derive(serde::Deserialize)]
        struct Body {
            spec: EffectSpec,
        }
        self.journaled().into_iter().filter(|e| e.kind.tag == EFFECT_SHADOWED).filter_map(|e| serde_json::from_str::<Body>(&e.kind.body).ok()).map(|b| b.spec).collect()
    }

    /// Effect names the handler asked for that had no double.
    pub fn refused(&self) -> &[String] {
        &self.refused
    }
}

/// Each journaled effect the task asked for again under the same key and input after the first asked
/// for it, with no probe between the two asks: a retry that did not look at the world first.
/// Run it on a rig that [`TaskRig::timing_out`], where every ask ends in a timeout.
pub fn retries_without_observing(requested: &[Requested]) -> Vec<String> {
    let mut found = vec![];
    for (i, first) in requested.iter().enumerate().filter(|(_, r)| r.journaled) {
        let unobserved = requested[i + 1..].iter().take_while(|r| r.journaled);
        if unobserved.into_iter().any(|r| r.name == first.name && r.key == first.key && r.input == first.input) {
            found.push(format!("{} {:?}", first.name, first.input));
        }
    }
    found.sort();
    found.dedup();
    found
}

/// T2: tick `stream` once to settle (a first tick may record a baseline), then tick it again and
/// require that the second appended nothing to the journal and asked for nothing but at most one
/// memory-only probe. Panics naming what the idle tick did.
pub fn assert_silent_tick(rig: &mut TaskRig, stream: &str) {
    let kind = rig.ticks.get(stream).cloned().unwrap_or_else(|| panic!("no tick recorded for stream {stream}: call rig.tick first"));
    rig.tick(stream, kind.clone()).expect("settling tick");
    let (journal_before, asked_before) = (rig.trace().len(), rig.requested().len());
    rig.tick(stream, kind).expect("idle tick");
    let appended = &rig.trace()[journal_before..];
    assert!(appended.is_empty(), "an idle tick on {stream} journaled {appended:?}");
    let asked = &rig.requested()[asked_before..];
    assert!(asked.iter().all(|r| !r.journaled) && asked.len() <= 1, "an idle tick on {stream} asked for more than its observe probe: {asked:?}");
}

/// T6: two rigs from `make(seed)`, each advanced `days` virtual days, must leave the same journal
/// trace and the same requested effects. Returns the first run's rig for budget checks.
pub fn assert_deterministic(make: impl Fn(u64) -> TaskRig, seed: u64, days: u64) -> TaskRig {
    let run = || {
        let mut rig = make(seed);
        rig.advance(days * DAY_MS).expect("advance");
        rig
    };
    let (a, b) = (run(), run());
    assert_eq!(a.trace(), b.trace(), "same seed, different journal trace");
    assert_eq!(a.requested(), b.requested(), "same seed, different requested effects");
    a
}

/// S11: journaled events per hour of virtual time elapsed, over the whole run, at most `ceiling`.
pub fn assert_journal_budget(rig: &TaskRig, ceiling: u64) {
    let hours = rig.now().div_ceil(HOUR_MS).max(1);
    let n = rig.trace().len() as u64;
    assert!(n <= ceiling * hours, "{n} journaled events in {hours} h exceeds {ceiling}/h");
}

/// Planted by `effects_runnable` to prove its matcher can report a missing runner.
pub const PLANTED_UNKNOWN: &str = "conformance.planted-unknown-effect";

/// T4 (first half): every name in `declared` has a runner in `real` and a double in `sim`. The
/// matcher is first run on a planted unknown name and must refuse it, so an empty answer means
/// something. `Err` lists each missing pair as `name: real` or `name: sim`.
pub fn effects_runnable(declared: &[&str], real: &[&str], sim: &[&str]) -> std::result::Result<(), String> {
    let missing = |names: &[&str]| -> Vec<String> {
        let mut out = vec![];
        for n in names {
            if !real.contains(n) {
                out.push(format!("{n}: real"));
            }
            if !sim.contains(n) {
                out.push(format!("{n}: sim"));
            }
        }
        out
    };
    if missing(&[PLANTED_UNKNOWN]).len() != 2 {
        return Err(format!("positive control: the planted name {PLANTED_UNKNOWN} was not refused"));
    }
    match missing(declared) {
        m if m.is_empty() => Ok(()),
        m => Err(m.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "rig.s";
    const TICK: EventKindTag = "rig.tick";
    const SEEN: EventKindTag = "rig.seen";

    /// Ticks every `every` ms, probes `look`, emits `rig.seen` when the answer changes, and
    /// requests a journaled `say` effect when the answer is "loud".
    struct Toy {
        every: u64,
    }

    impl Handler for Toy {
        fn interests(&self) -> &[EventKindTag] {
            &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED]
        }
        fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
            if ev.kind.tag == TICK {
                return vec![Command::Probe(EffectSpec { name: "look".into(), input: String::new(), deadline_ms: 100, key: "look".into() }), Command::Timer { at: ev.at + self.every, kind: EventKind::new(TICK, "") }];
            }
            if ev.kind.tag != EFFECT_SUCCEEDED {
                return vec![];
            }
            let last = view.events(&ev.stream).iter().rev().find(|e| e.kind.tag == SEEN).map(|e| e.kind.body.clone());
            let mut cmds = vec![];
            if last.as_deref() != Some(ev.kind.body.as_str()) && ev.kind.body != "said" {
                cmds.push(Command::Emit(EventKind::new(SEEN, &ev.kind.body)));
            }
            if ev.kind.body == "loud" {
                cmds.push(Command::Effect(EffectSpec { name: "say".into(), input: "hi".into(), deadline_ms: 100, key: "say".into() }));
            }
            cmds
        }
    }

    fn rig(answer: &'static str) -> TaskRig {
        TaskRig::new(7, Registration::new("toy", Toy { every: 60_000 })).unwrap().double("look", move |_| Ok(answer.into())).double("say", |_| Ok("said".into()))
    }

    #[test]
    fn a_quiet_task_passes_the_silence_check() {
        let mut r = rig("quiet");
        r.tick(S, EventKind::new(TICK, "")).unwrap();
        assert_silent_tick(&mut r, S);
    }

    #[test]
    #[should_panic(expected = "journaled")]
    fn a_task_that_writes_every_tick_fails_the_silence_check() {
        let mut r = rig("loud");
        r.tick(S, EventKind::new(TICK, "")).unwrap();
        assert_silent_tick(&mut r, S);
    }

    #[test]
    fn advance_fires_the_cadence_and_records_probes_as_unjournaled() {
        let mut r = rig("quiet");
        r.tick(S, EventKind::new(TICK, "")).unwrap();
        r.advance(10 * 60_000).unwrap();
        assert_eq!(r.requested().len(), 11);
        assert!(r.requested().iter().all(|q| q.name == "look" && !q.journaled));
        assert_eq!(r.requested().last().unwrap().at, 600_000);
        assert_eq!(r.journaled().iter().filter(|e| e.kind.tag == SEEN).count(), 1);
    }

    #[test]
    fn an_effect_without_a_double_is_refused_with_the_production_text() {
        let mut r = TaskRig::new(1, Registration::new("toy", Toy { every: 60_000 })).unwrap().double("look", |_| Ok("loud".into()));
        r.tick(S, EventKind::new(TICK, "")).unwrap();
        assert_eq!(r.refused(), ["say"]);
        assert!(r.requested().iter().any(|q| q.name == "say" && q.journaled));
        assert!(r.journaled().iter().any(|e| e.kind.tag == EFFECT_FAILED && e.kind.body == "no runner for effect say"));
    }

    #[test]
    fn two_runs_on_one_seed_give_one_trace_and_the_budget_reads_it() {
        let a = assert_deterministic(
            |seed| {
                let mut r = TaskRig::new(seed, Registration::new("toy", Toy { every: 60_000 })).unwrap().double("look", |_| Ok("loud".into())).double("say", |_| Ok("said".into()));
                r.tick(S, EventKind::new(TICK, "")).unwrap();
                r
            },
            3,
            1,
        );
        assert!(a.trace().len() > 1000, "a loud task journals every tick");
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| assert_journal_budget(&a, 10))).is_err(), "the budget must catch it");
        assert_journal_budget(&a, 1000);
    }

    #[test]
    fn an_observe_task_runs_its_probes_and_its_writes_are_shadowed_not_run() {
        let reg = Registration::new("toy", Toy { every: 60_000 }).with_mode(Mode::Observe);
        let mut r = TaskRig::new(1, reg).unwrap().double("look", |_| Ok("loud".into())).double("say", |_| panic!("an observe task's write must not run"));
        r.tick(S, EventKind::new(TICK, "")).unwrap();
        assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), ["look"]);
        assert_eq!(r.shadowed(), [EffectSpec { name: "say".into(), input: "hi".into(), deadline_ms: 100, key: "say".into() }]);
    }

    #[test]
    fn effects_runnable_refuses_the_planted_name_and_names_what_is_missing() {
        assert_eq!(effects_runnable(&["a"], &["a"], &["a"]), Ok(()));
        assert_eq!(effects_runnable(&["a", "b"], &["a", "b"], &["a"]), Err("b: sim".into()));
        assert_eq!(effects_runnable(&[PLANTED_UNKNOWN], &[], &[]), Err(format!("{PLANTED_UNKNOWN}: real, {PLANTED_UNKNOWN}: sim")));
    }
}
