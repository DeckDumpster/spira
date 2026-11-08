//! Family B, reconcilers and alarms, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`.
//!
//! The family's tasks are stubs written red-first: each module under `b/` holds its contract
//! (the module doc), its snapshot and carry types, its effect names, its boot tick and
//! registration, and a `decide` that is not ported yet. Their rows wait in [`PENDING`], not in
//! [`TASKS`], until each task's keys are registered in `spira/conf.d` and the settings fixture,
//! and each task is named in `SPIRA_TASK_MODES`: a row in `TASKS` is read by
//! `Settings::from_lookup` and `modes::apply`, which refuse a key or a task they do not know.
//! Promoting a row is moving it from `PENDING` to `TASKS`.
//!
//! [`kit`] is the shape every family B and C stub shares: the tick, the observe probe, the
//! carried state and the acceptance helpers T1–T6 are written against.

use super::Task;

/// `Stub` and `Handler` for a stub module's handler struct `$ty`, which has fields `cadence`
/// and `cfg`, beside the module's `SPEC`, `KEYS`, `CADENCE_KEY`, `TICK`, `Snapshot`, `Carry`
/// and `decide`.
macro_rules! hosted {
    ($ty:ident) => {
        impl $crate::tasks::b::kit::Stub for $ty {
            type Snapshot = Snapshot;
            type Carry = Carry;
            const SPEC: $crate::tasks::b::kit::Spec = SPEC;

            fn from_cfg(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String> {
                let cfg = $crate::tasks::b::kit::configure(KEYS, get)?;
                Ok($ty { cadence: $crate::tasks::b::kit::cadence(&cfg, CADENCE_KEY, SPEC.calendar)?, cfg })
            }

            fn cadence(&self) -> &$crate::tasks::b::kit::Cadence {
                &self.cadence
            }

            fn cfg(&self) -> &$crate::tasks::b::kit::Cfg {
                &self.cfg
            }

            fn decide(cfg: &$crate::tasks::b::kit::Cfg, carry: &Carry, input: &$crate::tasks::b::kit::Input<Snapshot>, now_ms: u64) -> $crate::tasks::b::kit::Decision<Carry> {
                decide(cfg, carry, input, now_ms)
            }
        }

        impl $ty {
            /// The first tick, handed to `Engine::start` each time the process starts.
            pub fn boot() -> (&'static str, &'static str, runtime::event::EventKind) {
                $crate::tasks::b::kit::boot::<$ty>()
            }

            pub fn registration(self) -> runtime::event::Registration {
                <$ty as $crate::tasks::b::kit::Stub>::registration(self)
            }
        }

        impl runtime::event::Handler for $ty {
            fn interests(&self) -> &[runtime::event::EventKindTag] {
                static INTERESTS: [runtime::event::EventKindTag; 5] = $crate::tasks::b::kit::interests(TICK);
                &INTERESTS
            }

            fn handle(&self, ctx: &runtime::event::Ctx, view: &runtime::event::Projection, ev: &runtime::event::Event) -> Vec<runtime::event::Command> {
                $crate::tasks::b::kit::handle(self, ctx, view, ev)
            }
        }
    };
}
pub(crate) use hosted;

pub mod asks_verify;
pub mod incident_alarms;
pub mod incident_settle;
pub mod mail_reconcile;
pub mod reconcile_content;
pub mod reconcile_continuity;
pub mod reconcile_goodput;
pub mod reconcile_heimdall;
pub mod reconcile_section;

use asks_verify::AsksVerify;
use incident_alarms::IncidentAlarms;
use incident_settle::IncidentSettle;
use kit::{build, cadence_ms};
use mail_reconcile::MailReconcile;
use reconcile_content::ReconcileContent;
use reconcile_continuity::ReconcileContinuity;
use reconcile_goodput::ReconcileGoodput;
use reconcile_heimdall::ReconcileHeimdall;
use reconcile_section::ReconcileSection;

pub static TASKS: &[Task] = &[
    Task { name: reconcile_section::NAME, plane: runtime::Plane::Work, signal: &[], boot: ReconcileSection::boot, cadence_ms: cadence_ms::<ReconcileSection>, effects: reconcile_section::EFFECTS, runners: &[(reconcile_section::PUBLISH, |e, _, _| reconcile_section::publish(&lc(e)))], keys: &[reconcile_section::KEYS], build: build::<ReconcileSection> },
    Task { name: reconcile_continuity::NAME, plane: runtime::Plane::Work, signal: &[], boot: ReconcileContinuity::boot, cadence_ms: cadence_ms::<ReconcileContinuity>, effects: reconcile_continuity::EFFECTS, runners: &[(reconcile_continuity::OBSERVE, |e, _, _| reconcile_continuity::observe(&lc(e)))], keys: &[reconcile_continuity::KEYS], build: build::<ReconcileContinuity> },
    Task {
        name: reconcile_content::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: ReconcileContent::boot,
        cadence_ms: cadence_ms::<ReconcileContent>,
        effects: reconcile_content::EFFECTS,
        runners: &[(reconcile_content::OBSERVE, |e, _, _| reconcile_content::observe(&lc(e), &e.world.run_dir)), (reconcile_content::SKEW_CHECK, |e, _, _| reconcile_content::skew_check(&lc(e))), (reconcile_content::CLOSE, |e, _, s| reconcile_content::close(&lc(e), &s.input))],
        keys: &[reconcile_content::KEYS],
        build: build::<ReconcileContent>,
    },
    Task {
        name: reconcile_goodput::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: ReconcileGoodput::boot,
        cadence_ms: cadence_ms::<ReconcileGoodput>,
        effects: reconcile_goodput::EFFECTS,
        runners: &[
            (reconcile_goodput::OBSERVE, |e, _, _| reconcile_goodput::observe(&lc(e), &e.world.run_dir)),
            (reconcile_goodput::LOCK, |e, _, s| reconcile_goodput::lock(&lc(e), &s.input)),
            (reconcile_goodput::STANDBY, |e, _, s| reconcile_goodput::standby(&lc(e), &s.input)),
            (reconcile_goodput::JUDGE, |e, _, s| reconcile_goodput::judge(&lc(e), &s.input)),
            (reconcile_goodput::PUT, |e, _, s| reconcile_goodput::put(&lc(e), &s.input)),
            (reconcile_goodput::RECORD, |e, _, s| reconcile_goodput::record(&lc(e), &s.input)),
            (reconcile_goodput::RELEASE, |e, _, s| reconcile_goodput::release(&lc(e), &s.input)),
        ],
        keys: &[reconcile_goodput::KEYS],
        build: build::<ReconcileGoodput>,
    },
    Task {
        name: reconcile_heimdall::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: ReconcileHeimdall::boot,
        cadence_ms: cadence_ms::<ReconcileHeimdall>,
        effects: reconcile_heimdall::EFFECTS,
        runners: &[
            (reconcile_heimdall::OBSERVE, |e, _, s| reconcile_heimdall::observe(&lc(e), &e.world.run_dir, &s.input, now_ms())),
            (reconcile_heimdall::RAISE, |e, _, s| reconcile_heimdall::raise(&lc(e), &s.input)),
            (reconcile_heimdall::CLEAR, |e, _, s| reconcile_heimdall::clear(&lc(e), &s.input)),
            (reconcile_heimdall::REFRESH, |e, _, s| reconcile_heimdall::raise(&lc(e), &s.input)),
            (reconcile_heimdall::FALLBACK, |e, _, s| reconcile_heimdall::fallback(&e.world.run_dir, &s.input)),
            (reconcile_heimdall::HEARTBEAT, |e, _, s| reconcile_heimdall::heartbeat(&e.world.run_dir, &s.input)),
        ],
        keys: &[reconcile_heimdall::KEYS],
        build: build::<ReconcileHeimdall>,
    },
    Task {
        name: mail_reconcile::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: MailReconcile::boot,
        cadence_ms: cadence_ms::<MailReconcile>,
        effects: mail_reconcile::EFFECTS,
        runners: &[
            (mail_reconcile::OBSERVE, |e, _, s| mail_reconcile::run(e, s)),
            (mail_reconcile::NOTE, |e, _, s| mail_reconcile::run(e, s)),
            (mail_reconcile::CLOSE, |e, _, s| mail_reconcile::run(e, s)),
            (mail_reconcile::LIFT, |e, _, s| mail_reconcile::run(e, s)),
            (mail_reconcile::ANSWER, |e, _, s| mail_reconcile::run(e, s)),
            (mail_reconcile::INBOX, |e, _, s| mail_reconcile::run(e, s)),
            (mail_reconcile::MARK, |e, _, s| mail_reconcile::run(e, s)),
        ],
        keys: &[mail_reconcile::KEYS],
        build: build::<MailReconcile>,
    },
    Task {
        name: asks_verify::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: AsksVerify::boot,
        cadence_ms: cadence_ms::<AsksVerify>,
        effects: asks_verify::EFFECTS,
        runners: &[
            (asks_verify::OBSERVE, |e, _, _| asks_verify::observe(e, &e.world.ask_label)),
            (asks_verify::CHECK, |e, _, s| asks_verify::run(e, s)),
            (asks_verify::CLOSE, |e, _, s| asks_verify::run(e, s)),
            (asks_verify::WITHDRAW, |e, _, s| asks_verify::run(e, s)),
            (asks_verify::NOTE, |e, _, s| asks_verify::run(e, s)),
        ],
        keys: &[asks_verify::KEYS],
        build: build::<AsksVerify>,
    },
    Task {
        name: incident_settle::NAME,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: IncidentSettle::boot,
        cadence_ms: cadence_ms::<IncidentSettle>,
        effects: incident_settle::EFFECTS,
        runners: &[
            (incident_settle::OBSERVE, |e, _, _| incident_settle::observe(e)),
            (incident_settle::CLOSE, |e, _, s| incident_settle::run(e, s)),
            (incident_settle::NOTE, |e, _, s| incident_settle::run(e, s)),
            (incident_settle::LABEL, |e, _, s| incident_settle::run(e, s)),
            (incident_settle::LOG, |e, _, s| incident_settle::run(e, s)),
        ],
        keys: &[incident_settle::KEYS],
        build: build::<IncidentSettle>,
    },
    Task {
        name: incident_alarms::NAME,
        plane: runtime::Plane::Work,
        signal: &[incident_alarms::PAGE, incident_alarms::NOTE, incident_alarms::FLOOR_ASK, incident_alarms::RETRY],
        boot: IncidentAlarms::boot,
        cadence_ms: cadence_ms::<IncidentAlarms>,
        effects: incident_alarms::EFFECTS,
        runners: &[
            (incident_alarms::OBSERVE, |e, _, s| incident_alarms::observe_with(&lc(e), &e.world.run_dir, &s.input, now_ms())),
            (incident_alarms::PAGE, |e, _, s| incident_alarms::send(&incident_alarms::Delivery::configured(&lc(e), &e.world.run_dir)?, &s.input)),
            (incident_alarms::NOTE, |e, _, s| incident_alarms::note(&incident_alarms::Delivery::configured(&lc(e), &e.world.run_dir)?, &s.input)),
            (incident_alarms::FLOOR_ASK, |e, _, s| incident_alarms::send(&incident_alarms::Delivery::configured(&lc(e), &e.world.run_dir)?, &s.input)),
            (incident_alarms::RETRY, |e, _, s| incident_alarms::retry(&incident_alarms::Delivery::configured(&lc(e), &e.world.run_dir)?, &s.input)),
            (incident_alarms::ATTEMPT, |e, _, s| incident_alarms::attempt(&e.world.run_dir, &s.input)),
            (incident_alarms::KNOB_INIT, |e, _, _| incident_alarms::knob_init(&lc(e))),
            (incident_alarms::PUBLISH, |e, _, s| incident_alarms::publish(&lc(e), &s.input)),
            (incident_alarms::HEARTBEAT, |_, _, s| incident_alarms::heartbeat(&s.input)),
        ],
        keys: &[incident_alarms::KEYS],
        build: build::<IncidentAlarms>,
    },
];

/// Wall time in ms, for a runner whose reading is an age (the effect itself runs on the host).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The store client every family B runner reaches lc-serve through: the world's socket, with the
/// socket client's 5 s deadline.
pub fn lc(e: &crate::Effects) -> spira_config::lc_call::LcClient {
    spira_config::lc_call::LcClient::socket(e.world.lc_socket.clone())
}

/// The family's rows, complete but not yet in `TASKS` (see the module doc).
pub static PENDING: &[Task] = &[
];

/// The shape every family B and C stub shares. A task supplies its [`kit::Spec`], its types
/// and its `decide`; the tick, the observe probe and the carried state are driven here.
pub mod kit {
    use std::collections::BTreeMap;

    use runtime::calendar::Calendar;
    use runtime::event::*;
    use runtime::units::{UnitRan, UNIT_RAN};
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Serialize};

    use crate::watchers::{answered_effect, carried};
    use crate::{Effects, Settings};

    /// A task's configured values, by key: every key of its row, read once at build.
    pub type Cfg = BTreeMap<String, String>;

    /// The body of a boot tick. A calendar task's boot tick arms its first elapse and runs
    /// nothing; every other tick (body [`FIRE`]) runs the pass.
    pub const BOOT: &str = "boot";
    pub const FIRE: &str = "";

    /// The interests every stub registers: its tick, the outcomes of what it asked for, and
    /// the completions of the units it started.
    pub const fn interests(tick: EventKindTag) -> [EventKindTag; 5] {
        [tick, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT, UNIT_RAN]
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Cadence {
        /// A monotonic interval: the next tick is a period after this one.
        Every(u64),
        /// An `OnCalendar` spec (S8): the next tick is the next elapse.
        Calendar(Calendar),
    }

    impl Cadence {
        pub fn next(&self, now_ms: u64) -> u64 {
            match self {
                Cadence::Every(ms) => now_ms + ms,
                Cadence::Calendar(c) => c.next_after(now_ms),
            }
        }

        /// What the boot tick at virtual 0 arms: the period, or for a calendar the first
        /// elapse after the epoch. The row's `cadence_ms` is this.
        pub fn first_ms(&self) -> u64 {
            self.next(0)
        }

        pub fn is_calendar(&self) -> bool {
            matches!(self, Cadence::Calendar(_))
        }
    }

    /// The fixed facts of a task, named once in its module.
    pub struct Spec {
        pub name: &'static str,
        pub stream: &'static str,
        pub tick: EventKindTag,
        /// The carried-state event; emitted only when the carry changes.
        pub state: EventKindTag,
        /// What a tick asks first: the observe probe, or for a task with no observe the one
        /// read it makes.
        pub probe: &'static str,
        pub probe_deadline_ms: u64,
        /// The key holding the interval in seconds, or the `OnCalendar` spec.
        pub cadence_key: &'static str,
        pub calendar: bool,
        /// Every effect and probe name the task may ask for.
        pub effects: &'static [&'static str],
        /// Every config key building the task reads.
        pub keys: &'static [&'static str],
        /// The key whose value is the observe probe's input, for a task with one; most send none.
        pub probe_input_key: Option<&'static str>,
    }

    /// What `decide` is handed.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Input<S> {
        /// A tick, for the single-flight and catch-up rules.
        Fire,
        /// The observe probe's answer.
        Observed(S),
        /// The outcome of one of the task's own effects.
        Outcome { effect: String, ok: bool, body: String },
        /// A unit the task started has ended (P2 long legs, S6).
        Ran(UnitRan),
    }

    /// One effect or probe `decide` asks for.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Act {
        pub name: String,
        pub input: String,
        pub deadline_ms: u64,
        /// A memory-only read rather than a journaled effect.
        pub probe: bool,
    }

    pub type Decision<C> = Result<(C, Vec<Act>), String>;

    pub trait Stub: Handler + Sized + 'static {
        type Snapshot: DeserializeOwned;
        type Carry: Serialize + DeserializeOwned + Default + PartialEq + Clone;
        const SPEC: Spec;

        fn from_cfg(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Self, String>;
        fn cadence(&self) -> &Cadence;
        fn cfg(&self) -> &Cfg;
        fn decide(cfg: &Cfg, carry: &Self::Carry, input: &Input<Self::Snapshot>, now_ms: u64) -> Decision<Self::Carry>;

        fn registration(self) -> Registration {
            Registration::new(Self::SPEC.name, self)
        }
    }

    /// Every key in `keys`, each read once through `get`, or the first that does not resolve.
    pub fn configure(keys: &[&str], get: &dyn Fn(&str) -> Result<String, String>) -> Result<Cfg, String> {
        keys.iter().map(|k| get(k).map(|v| (k.to_string(), v))).collect()
    }

    /// The cadence from `cfg[key]`: positive seconds, or an `OnCalendar` spec.
    pub fn cadence(cfg: &Cfg, key: &str, calendar: bool) -> Result<Cadence, String> {
        let raw = cfg.get(key).ok_or_else(|| format!("{key} is read but not declared"))?;
        if calendar {
            return Calendar::parse(raw).map(Cadence::Calendar).map_err(|e| format!("{key}: {e}"));
        }
        raw.trim().parse::<u64>().ok().filter(|s| *s > 0).map(|s| Cadence::Every(s * 1000)).ok_or_else(|| format!("{key} must be a positive integer, got {raw:?}"))
    }

    /// `T` from the `Settings` its row is built with: each key as `Settings::from_lookup` read
    /// it into `task_config`, never the process's own config door. A key the settings lack (no
    /// row of `tasks::all()` declares it) is a broken table, and refuses the build naming it.
    pub fn built<T: Stub>(settings: &Settings) -> T {
        let get = |k: &str| settings.task_config.get(k).cloned().ok_or_else(|| format!("{k} is not in the settings (no row of tasks::all() declares it)"));
        T::from_cfg(&get).unwrap_or_else(|e| panic!("{}: {e}", T::SPEC.name))
    }

    pub fn build<T: Stub>(settings: &Settings) -> Registration {
        built::<T>(settings).registration()
    }

    pub fn cadence_ms<T: Stub>(settings: &Settings) -> u64 {
        built::<T>(settings).cadence().first_ms()
    }

    pub fn boot<T: Stub>() -> (&'static str, &'static str, EventKind) {
        (T::SPEC.name, T::SPEC.stream, EventKind::new(T::SPEC.tick, if T::SPEC.calendar { BOOT } else { FIRE }))
    }

    /// The runner of an effect whose task is not ported yet.
    pub fn not_ported<T: Stub>(_: &Effects, _: &str, _: &EffectSpec) -> Result<String, String> {
        unimplemented!("{}: not ported yet", T::SPEC.name)
    }

    /// The handler body every stub shares.
    pub fn handle<T: Stub>(t: &T, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let spec = &T::SPEC;
        let stream = StreamKey(spec.stream.into());
        if ev.kind.tag == UNIT_RAN {
            return match UnitRan::from_event_kind(&ev.kind).filter(|r| r.key_for(spec.name).is_some()) {
                Some(ran) => run(t, ctx, view, &stream, Input::Ran(ran), ev.at),
                None => vec![],
            };
        }
        if ev.stream != stream {
            return vec![];
        }
        if ev.kind.tag == spec.tick {
            let mut cmds = vec![];
            if !(t.cadence().is_calendar() && ev.kind.body == BOOT) {
                let input = spec.probe_input_key.and_then(|k| t.cfg().get(k)).cloned().unwrap_or_default();
                cmds.push(Command::Probe(EffectSpec { name: spec.probe.into(), input, deadline_ms: spec.probe_deadline_ms }));
            }
            cmds.push(Command::Timer { at: t.cadence().next(ev.at), kind: EventKind::new(spec.tick, FIRE) });
            return cmds;
        }
        let Some(asked) = answered_effect(view, ev) else { return vec![] };
        if asked == spec.probe {
            if ev.kind.tag != EFFECT_SUCCEEDED {
                return vec![];
            }
            let Ok(snap) = serde_json::from_str::<T::Snapshot>(&ev.kind.body) else { return vec![] };
            return run(t, ctx, view, &stream, Input::Observed(snap), ev.at);
        }
        if !spec.effects.contains(&asked.as_str()) {
            return vec![];
        }
        let ok = ev.kind.tag == EFFECT_SUCCEEDED;
        run(t, ctx, view, &stream, Input::Outcome { effect: asked, ok, body: ev.kind.body.clone() }, ev.at)
    }

    fn run<T: Stub>(t: &T, ctx: &Ctx, view: &Projection, stream: &StreamKey, input: Input<T::Snapshot>, now: u64) -> Vec<Command> {
        let carry = carried::<T::Carry>(view, stream, T::SPEC.state);
        match T::decide(t.cfg(), &carry, &input, now) {
            Ok((next, acts)) => {
                let mut cmds = vec![];
                if next != carry {
                    cmds.push(Command::Emit(EventKind::new(T::SPEC.state, &serde_json::to_string(&next).unwrap_or_default())));
                }
                for a in acts {
                    let spec = EffectSpec { name: a.name, input: a.input, deadline_ms: a.deadline_ms };
                    cmds.push(if a.probe { Command::Probe(spec) } else { Command::Effect(spec) });
                }
                cmds
            }
            Err(why) => {
                ctx.event().set(&format!("{}.refused", T::SPEC.name), why.as_str());
                vec![]
            }
        }
    }

    /// The names of `acts`, in order, for parity assertions.
    pub fn names(acts: &[Act]) -> Vec<&str> {
        acts.iter().map(|a| a.name.as_str()).collect()
    }

    /// The acceptance helpers T1–T6 (design §6) for a stub, used by `tests/task_{b,c}_*.rs`.
    pub mod testing {
        use super::*;
        use runtime::journal::Journal;
        use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, PLANTED_UNKNOWN};

        /// The launchers `tests/it/no_spawn.rs` scans for, each split so this file does not name one.
        pub const LAUNCHERS: &[&str] = &[concat!("Command", "::new"), concat!("std", "::process"), concat!("process", "::Command"), concat!("libc", "::fork"), concat!("libc", "::exec"), concat!("posix", "_spawn"), concat!("nix", "::unistd")];

        /// A config door that answers exactly `T`'s declared keys, `set` first and `"fixture"`
        /// otherwise, and refuses any other key: building through it proves every key read is
        /// declared on the row.
        pub fn lookup<T: Stub>(set: &[(&str, &str)]) -> impl Fn(&str) -> Result<String, String> {
            let set: BTreeMap<String, String> = set.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            move |k: &str| {
                if !T::SPEC.keys.contains(&k) {
                    return Err(format!("{k} is read but {} does not declare it", T::SPEC.name));
                }
                Ok(set.get(k).cloned().unwrap_or_else(|| "fixture".into()))
            }
        }

        pub fn handler<T: Stub>(set: &[(&str, &str)]) -> T {
            T::from_cfg(&lookup::<T>(set)).unwrap_or_else(|e| panic!("{}: {e}", T::SPEC.name))
        }

        /// A rig on `T` whose observe probe answers `snapshot`, booted.
        pub fn rig<T: Stub>(seed: u64, set: &[(&str, &str)], snapshot: &str) -> TaskRig {
            rig_cycle::<T>(seed, set, vec![snapshot.to_string()])
        }

        /// A rig on `T` whose observe probe answers `answers` in turn, round and round, booted.
        pub fn rig_cycle<T: Stub>(seed: u64, set: &[(&str, &str)], answers: Vec<String>) -> TaskRig {
            let asked = std::sync::atomic::AtomicUsize::new(0);
            let answer = move |_: &str| Ok(answers[asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % answers.len()].clone());
            let mut r = TaskRig::new(seed, handler::<T>(set).registration()).unwrap().double(T::SPEC.probe, answer);
            let (_, stream, kind) = boot::<T>();
            r.tick(stream, kind).unwrap();
            r
        }

        /// Panics with the handler's own panic when the stream was quarantined, so a stub's
        /// `unimplemented!` is the failure a test reports rather than a silent poisoned stream.
        pub fn assert_ported(rig: &TaskRig, stream: &str) {
            if let Some(p) = rig.journal.poisoned(&StreamKey(stream.into())).expect("read poison") {
                panic!("{}", p.message);
            }
        }

        /// The last carried state `rig` journaled on `T`'s stream.
        pub fn recorded<T: Stub>(rig: &TaskRig) -> Option<T::Carry> {
            rig.journaled().into_iter().rev().find(|e| e.stream.0 == T::SPEC.stream && e.kind.tag == T::SPEC.state).and_then(|e| serde_json::from_str(&e.kind.body).ok())
        }

        /// T1: `T` builds from its declared keys alone, its boot tick arms the first tick at
        /// `first_ms`, and it is a row of `tasks::all()` with its declared keys and effects.
        pub fn t1_registered_and_boots<T: Stub>(set: &[(&str, &str)], first_ms: u64) {
            let h = handler::<T>(set);
            assert_eq!(h.cadence().first_ms(), first_ms, "{}: cadence", T::SPEC.name);
            let mut rig = TaskRig::new(1, h.registration()).unwrap();
            let (_, stream, kind) = boot::<T>();
            assert_eq!(stream, T::SPEC.stream);
            rig.tick(stream, kind).unwrap();
            assert_eq!(rig.engine().next_timer_at(), Some(first_ms), "{}: the boot tick must arm the first tick", T::SPEC.name);
            for q in rig.requested() {
                assert!(T::SPEC.effects.contains(&q.name.as_str()), "{} asked for undeclared {}", T::SPEC.name, q.name);
            }
            let row = crate::tasks::all().find(|t| t.name == T::SPEC.name).unwrap_or_else(|| panic!("{} is not a row of tasks::all(): it waits in PENDING until its keys and mode are registered", T::SPEC.name));
            assert_eq!(row.effects, T::SPEC.effects);
            assert_eq!(row.keys.iter().flat_map(|k| k.iter().copied()).collect::<Vec<_>>(), T::SPEC.keys);
            assert_eq!(row.stream(), T::SPEC.stream);
        }

        /// T2: on `idle`, a tick journals nothing and asks for nothing beyond its probe.
        pub fn t2_silent<T: Stub>(set: &[(&str, &str)], idle: &str) {
            let mut r = rig::<T>(1, set, idle);
            r.tick(T::SPEC.stream, EventKind::new(T::SPEC.tick, FIRE)).unwrap();
            assert_ported(&r, T::SPEC.stream);
            assert_silent_tick(&mut r, T::SPEC.stream);
            assert_ported(&r, T::SPEC.stream);
        }

        /// T4: every declared effect has a real runner (`tasks::routes()`) and a double in
        /// `sim`, the simulator's names (the matcher refuses a planted name first), and `src`,
        /// the module's text, launches no process.
        pub fn t4_runnable<T: Stub>(src: &str, sim: &[&str]) {
            assert!(launcher(concat!("fn f() { Command", "::new(\"x\"); }")).is_some(), "the spawn scan must see a planted launcher");
            assert_eq!(launcher(src), None, "{} launches a process", T::SPEC.name);
            let real: Vec<&str> = crate::tasks::routes().keys().copied().collect();
            assert!(!real.contains(&PLANTED_UNKNOWN) && !sim.contains(&PLANTED_UNKNOWN));
            assert_eq!(effects_runnable(T::SPEC.effects, &real, sim), Ok(()), "{}", T::SPEC.name);
        }

        /// The first launcher `text` names before its `#[cfg(test)]` tail.
        pub fn launcher(text: &str) -> Option<&'static str> {
            let code = text.split("#[cfg(test)]").next().unwrap_or_default();
            LAUNCHERS.iter().copied().find(|l| code.contains(l))
        }

        /// T6: `days` virtual days from a rig made by `make` (seeded), twice, give one trace;
        /// the stream is never quarantined; the journal stays within `per_hour`. Returns the
        /// first run's rig.
        pub fn t6_deterministic<T: Stub>(make: impl Fn(u64) -> TaskRig, days: u64, per_hour: u64) -> TaskRig {
            let rig = assert_deterministic(make, 7, days);
            assert_ported(&rig, T::SPEC.stream);
            assert_journal_budget(&rig, per_hour);
            rig
        }

        /// The times `T`'s probe was asked, in order.
        pub fn fired_at<T: Stub>(rig: &TaskRig) -> Vec<u64> {
            rig.requested().iter().filter(|q| q.name == T::SPEC.probe).map(|q| q.at).collect()
        }

        /// Every elapse of `spec` in `(from, to]`.
        pub fn elapses(spec: &str, from: u64, to: u64) -> Vec<u64> {
            let c = Calendar::parse(spec).unwrap();
            let mut out = vec![];
            let mut at = c.next_after(from);
            while at <= to {
                out.push(at);
                at = c.next_after(at);
            }
            out
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every family-B and family-C row (both build through this kit) builds from the
        /// `Settings` it is handed, with no config door resolved: a planted value there is the
        /// value pinned, and a key the settings lack refuses the build.
        #[test]
        fn built_reads_the_settings_it_is_given_not_the_process_config() {
            use crate::tasks::b::reconcile_section::{ReconcileSection, CADENCE_KEY};
            let mut s = crate::settings::fixture();
            s.task_config.insert(CADENCE_KEY.into(), "4242".into());
            let t = built::<ReconcileSection>(&s);
            assert_eq!(t.cfg()[CADENCE_KEY], "4242", "the row pins what the settings hold");
            assert_eq!(cadence_ms::<ReconcileSection>(&s), 4_242_000);
            for row in crate::tasks::b::TASKS.iter().chain(crate::tasks::c::TASKS) {
                let _ = (row.build)(&s);
                let _ = (row.cadence_ms)(&s);
            }
            s.task_config.remove(CADENCE_KEY);
            assert!(std::panic::catch_unwind(|| built::<ReconcileSection>(&s)).is_err(), "a key the settings lack refuses");
        }
    }
}
