//! # sentinel-call — contract
//!
//! Pattern P2 of [[scheduler-consolidation-2026-11-03]] (§1 row A `call`). Today: `sentinel --call-only`
//! (`sentinel/src/pass.rs:56`, `call.rs:61`). Paths below are relative to `spira-releases/current/`.
//!
//! **Code vs catalogue.** There is no `sentinel-call` or `dwarf-launch` binary; the call is
//! `systemd-run … dwarf --home <home> <rune>` (`sentinel/src/call.rs:679-702`, `SPIRA_CALL`
//! default `systemd-run`, `cfg.rs:444`), and every dwarf unit carries
//! `ExecStopPost=… sentinel --call-only` (`call.rs:506-515`) — the "sentinel-call" child is that re-entry.
//!
//! ## Tick
//!
//! - Calendar `*-*-* *:*:00` (timer `spira-call-prod.timer`), single-flight. Today single-flight is
//!   the OS flock on `$SPIRA_RUN/call.lock`, waited up to `SPIRA_CALL_LOCK_WAIT` (`call.rs:858-895`,
//!   `cfg.rs:243`); in-process it is the task's own `running` state.
//! - Also woken by: `unit.ran` for any `spira-dwarf-*` unit (replaces the ExecStopPost re-entry,
//!   `call.rs:513`); `sentinel-pass` reaching CHECK 7 (`pass.rs:520`) emits `call.wanted` instead
//!   of running the call body itself.
//!
//! ## Observe — `sentinel-call.observe` (probe, 5 s)
//!
//! Snapshot `{ world, skew, capacity, live_by_rune, ready_by_rune, urgent, express_ready, throttle }`:
//!
//! | Field | Today | Contract source |
//! |---|---|---|
//! | `world` halted/draining | files `$SPIRA_RUN/world.halted`, `world.draining` (`call.rs:188-195`) | `world.state` projection (none exists yet — gap, unverified) |
//! | `skew` | `spira_config::release_skew::skew` (`call.rs:467-485`) | library call, same fn |
//! | `capacity` | `dwarf::capacity::pause_state($SPIRA_RUN/capacity-pause)` (`call.rs:72`, `cfg.rs:466`) | library call into `dwarf::capacity` |
//! | `live_by_rune` | `systemctl --user list-units spira-dwarf-<rune>-*` (`call.rs:211-238`, `pass.rs:624-655`) or pidfiles | `unit.facts` (S6) over prefix `spira-dwarf-` |
//! | `ready_by_rune` | `spira-claim bulk-ready-by-rune` (`call.rs:44-57`), `rune-ready <f>` (`call.rs:317-325`) | spira-claim library: `bulk_ready_by_rune`, `rune_ready` over the S7b ready projection |
//! | `urgent` | `spira-claim urgent <task runes>` (`call.rs:382-402`) | spira-claim library `urgent` |
//! | `express_ready` | `spira-claim rune-exclude` + `ready-count … --express` (`call.rs:349-376`) | spira-claim library `ready_count` |
//! | `throttle` | `$SPIRA_RUN/queue-throttled` stamp (`cfg.rs:471-475`, `call.rs:793`) | batcher's throttle projection (unverified that one exists) |
//! | youngest reactive dwarf | pidfile mtimes in `$SPIRA_RUN` (`call.rs:405-428`) | `unit.facts` start times |
//!
//! A failed ready answer is `None`, never 0 (`call.rs:314-325`, sp-hh599).
//!
//! ## Decide
//!
//! Pure `decide(carry, snap) -> (carry, Vec<Call>, Option<Preempt>)`, ported from the existing
//! pure helpers: `check7_pool_decision`, `ck7_pool`, `ck7_throttled`, `ck7_fill_cap`
//! (`call.rs:127-173`) plus the gates of `call_rune` (`call.rs:577-675`: world, skew, capacity,
//! fleet ceiling `SPIRA_MAX_LIVE_DWARVES`, elastic hold-back) and the loop of `ck7_call_body` (`call.rs:728-850`). The per-pass time budget
//! (`SPIRA_SENTINEL_PASS_BUDGET_SECS`, `call.rs:773`) becomes moot: decide is O(runes), no IO.
//! Carry: `in_flight: {unit → rune}` (units started, no `unit.ran` yet).
//! Fixtures: the existing `call::tests` table tests (`call.rs:951-1000`) and `tests.rs:593-670`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sentinel-call.unit_start` (`unit.start` of `spira-dwarf-<rune>-<now>` running `dwarf --home <home> <rune>`, props from `call_argv` `call.rs:519-533`) | unit | 10 s (unverified) | unit name | world halted/draining, skew, capacity paused/unknown, rune not on roster, `dwarf` not resolvable (`call.rs:680`) |
//! | `sentinel-call.preempt` (stop youngest reactive dwarf; today `slay.sh --bead` `call.rs:451-462`) | unit (`unit.stop`) | 30 s (unverified) | `(bead, express)` | no reactive dwarf; no task rune can take the slot (`call.rs:439-445`) |
//! | `sentinel-call.preempt_log` (append `lane-preemptions`, `call.rs:459-462`) | host | 1 s | `(bead, express)` | — |
//! | `sentinel-call.metric` (otel `call` event, `call.rs:704`) | host | 1 s | unit name | — |
//!
//! The dwarf session is the long leg: `unit.start` returns at unit creation; `unit.ran` closes it.
//!
//! ## States
//!
//! `idle → observing → deciding → starting → idle`; per started unit `started → running → ran`.
//!
//! | State × event | → |
//! |---|---|
//! | idle × tick / `call.wanted` / `unit.ran(spira-dwarf-*)` | observing |
//! | observing × tick | observing (coalesced; single-flight) |
//! | observing × probe ok | deciding |
//! | observing × probe failed/timed out | idle (no call; logged once) |
//! | deciding × decide → no calls | idle (silent) |
//! | deciding × decide → calls | starting |
//! | starting × all `unit_start` outcomes | idle |
//! | unit started × `unit.start` ok | running (in `in_flight`) |
//! | unit started × `unit.start` failed | ran(failed) — slot not counted |
//! | running × `unit.ran` | ran → removed from `in_flight`, emits `call.wanted` |
//!
//! ## Silence
//!
//! Nothing ready, or every gate closed: the probe only. The gate-refusal log lines of today
//! (`call.rs:189,76,603,651`) become a `Note` only when the refusal reason **changes**.
//!
//! ## Replaces
//!
//! `spira-call-prod.service`/`.timer` and `world guard` ExecStartPre; every dwarf unit's
//! `ExecStopPost=sentinel --call-only`; `call.lock`. Children retired: `spira-claim` ×4 verbs,
//! `systemctl list-units`, `systemd-run`, `slay.sh`. Keys pinned (S10): `SPIRA_RUN`, `SPIRA_MAX_DWARVES`,
//! `SPIRA_MAX_LIVE_DWARVES`, `SPIRA_QUEUE_THROTTLE_OVERRIDE`,
//! `SPIRA_CALL_LOCK_WAIT` (retired), `SPIRA_CALL`, `SPIRA_SYSTEMCTL`, `SPIRA_THROTTLE_STAMP`,
//! `SPIRA_CAPACITY_PAUSE`, `SPIRA_SENTINEL_PASS_BUDGET_SECS` (`cfg.rs:239-247,442-476`), per-rune
//! `RUNE_*` (`call.rs:256,268-272,299-305,520`). The CLI modes `--call`, `--call-argv`, `--world-gate`,
//! `--call-pass` (`pass.rs:84-95`) stay until czar-pass and `dwarf --escape` call the library.
//!
//! ## The port
//!
//! `decide` is `sentinel::plan::plan` (the sentinel library's pure CHECK 7) over the snapshot.
//! Each call is a `unit.start` through the Units port under the naming rule
//! `spira-run-sentinel_2dcall-<rune>_2d<at>_2d<n>` (`runtime::units::unit_name`), carried in
//! `in_flight` until its `unit.ran` (matched by `UnitRan::key_for`) or until the observe no
//! longer lists it live. The dwarf starts under the properties `call_argv` gave it
//! (`units::dwarf_props`: `TimeoutStartSec` from `RUNE_TIMEOUT_SECONDS`, no IO class or weight) as `UnitProps` on the start; its `ExecStopPost` refill (the `--call-only` re-exec) is
//! retired: the refill is the dwarf's own `unit.ran` on this stream, and the minute calendar. `SPIRA_CALL`, `SPIRA_SYSTEMCTL`
//! and `SPIRA_SENTINEL_PASS_BUDGET_SECS` are retired (the port replaces the first two, decide
//! is O(runes)); the throttle stamp and the capacity pause are the paths under `SPIRA_RUN`
//! the binary defaulted them to.
//!
//! ## Reverse
//!
//! Release rollback restores the timer and ExecStopPost; or set the task to `Observe` — every
//! `unit_start` is journaled `effect.shadowed` and the old timer may be re-enabled by hand.
//!

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runtime::calendar::Calendar;
use runtime::event::*;
use runtime::units::{self, UnitRan, UnitSpec, UNIT_RAN};
use sentinel::plan;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};
use crate::Effects;

pub const NAME: &str = "sentinel-call";
pub const STREAM: &str = "task.sentinel-call";
pub const TICK: EventKindTag = "sentinel-call.tick";
pub const STATE: EventKindTag = "sentinel-call.state";
/// Emitted by [`super::sentinel_pass`] at CHECK 7 and when a dwarf unit ends; wakes the call.
pub const CALL_WANTED: EventKindTag = "call.wanted";
/// A gate's refusal, noted (memory only) when the reason changes.
pub const REFUSED: EventKindTag = "sentinel-call.refused";
pub const OBSERVE: &str = "sentinel-call.observe";
pub const UNIT_START: &str = "sentinel-call.unit_start";
pub const PREEMPT: &str = "sentinel-call.preempt";
pub const PREEMPT_LOG: &str = "sentinel-call.preempt_log";
pub const METRIC: &str = "sentinel-call.metric";
/// The prefix every dwarf unit the binary started carries; still counted while any is live.
pub const DWARF_PREFIX: &str = units::DWARF_PREFIX;
pub const CALENDAR: &str = "*-*-* *:*:00";
pub const OBSERVE_DEADLINE_MS: u64 = 5_000;

/// The Act table: each effect and its deadline.
pub const ACTS: &[(&str, u64)] = &[(UNIT_START, 10_000), (PREEMPT, 30_000), (PREEMPT_LOG, 1_000), (METRIC, 1_000)];
/// Hand-offs decide returns as acts that are emitted, not run.
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, UNIT_START, PREEMPT, PREEMPT_LOG, METRIC];
/// The pinned keys registered in `spira/conf.d`.
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_RUNES", "SPIRA_MAX_DWARVES", "SPIRA_MAX_LIVE_DWARVES", "SPIRA_LIMIT_CPU", "SPIRA_LIMIT_MEMORY", "SPIRA_LIMIT_NETWORK", "SPIRA_QUEUE_THROTTLE_OVERRIDE"];
/// Keys the contract pins that `spira/conf.d` does not register: none. The five the contract
/// listed are retired or are paths under `SPIRA_RUN` (the module doc, "The port").
pub const UNREGISTERED: &[&str] = &[];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum World {
    #[default]
    Running,
    Halted,
    Draining,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capacity {
    #[default]
    Clear,
    Paused,
    Unknown,
}

/// One live dwarf unit, for the youngest-reactive-dwarf preemption.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveUnit {
    pub unit: String,
    pub rune: String,
    pub bead: String,
    pub started_ms: u64,
}

/// A rune's own call settings (`RUNE_SINGLE_FLIGHT`, `RUNE_ELASTIC`, `RUNE_USES`, `RUNE_REACTIVE`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuneCfg {
    pub single_flight: bool,
    pub elastic: bool,
    pub uses: Vec<String>,
    pub reactive: bool,
}

impl Default for RuneCfg {
    /// `rune_get`'s own defaults: a task rune, not single-flight, not elastic.
    fn default() -> Self {
        RuneCfg { single_flight: false, elastic: false, uses: vec![], reactive: false }
    }
}

/// One row of `spira-claim urgent`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrgentBead {
    pub id: String,
    pub p0: bool,
    pub express: bool,
}

/// The observe snapshot. A ready count that could not be read is `None`, never 0 (sp-hh599).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub world: World,
    /// The release skew refusal, when there is one.
    pub skew: Option<String>,
    pub capacity: Capacity,
    pub live_by_rune: BTreeMap<String, u32>,
    pub ready_by_rune: BTreeMap<String, Option<u32>>,
    /// `None` when the machine could not answer: the reactive runes yield.
    pub urgent: Option<Vec<UrgentBead>>,
    pub express_ready: Option<u32>,
    pub throttle: bool,
    pub reactive_dwarves: Vec<LiveUnit>,
    /// Every roster rune's live dwarf that a pidfile names, newest first.
    #[serde(default)]
    pub live_dwarves: Vec<LiveUnit>,
    /// The fleet ceiling `SPIRA_MAX_LIVE_DWARVES`, as pinned.
    pub max_live: Option<u32>,
    /// The `build-disk` limit `SPIRA_MAX_DWARVES`, as pinned.
    pub max_dwarves: Option<u32>,
    /// The limits of the other bound classes (`cpu`, `memory`, `network`); an unset one is absent.
    pub class_limits: BTreeMap<String, u32>,
    /// The callable runes in roster order; empty reads the order of `ready_by_rune`.
    pub roster: Vec<String>,
    /// Each rune's call settings; a rune missing here has `RuneCfg::default()`.
    pub runes: BTreeMap<String, RuneCfg>,
    /// Live dwarves of no rune on the roster: they count toward the fleet ceiling only.
    pub other_live: u32,
    /// The units this task started that are still live, by name.
    pub live_units: Vec<String>,
    /// When the probe read the world; it names the units a call starts.
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Idle,
    Observing,
    Deciding,
    Starting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Tick,
    CallWanted,
    /// `unit.ran` for a `spira-dwarf-*` unit this task started.
    UnitRan,
    /// `unit.ran` for a unit this task does not know.
    UnitRanForeign,
    ProbeOk,
    ProbeFailed,
    NoCalls,
    Calls,
    AllStarted,
}

/// `state × event → state`, from the contract's States table.
pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Tick, State::Observing),
    (State::Idle, Trigger::CallWanted, State::Observing),
    (State::Idle, Trigger::UnitRan, State::Observing),
    (State::Idle, Trigger::UnitRanForeign, State::Idle),
    (State::Observing, Trigger::Tick, State::Observing),
    (State::Observing, Trigger::ProbeOk, State::Deciding),
    (State::Observing, Trigger::ProbeFailed, State::Idle),
    (State::Deciding, Trigger::NoCalls, State::Idle),
    (State::Deciding, Trigger::Calls, State::Starting),
    (State::Starting, Trigger::AllStarted, State::Idle),
];

/// One started dwarf unit's own leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Leg {
    Started,
    Running,
    Ran,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegTrigger {
    StartOk,
    StartFailed,
    UnitRan,
}

pub static LEG_TABLE: &[(Leg, LegTrigger, Leg)] = &[(Leg::Started, LegTrigger::StartOk, Leg::Running), (Leg::Started, LegTrigger::StartFailed, Leg::Ran), (Leg::Running, LegTrigger::UnitRan, Leg::Ran)];

/// Carried between ticks: the machine and every unit started with no
/// `unit.ran` yet (unit name → rune); `refusal` is the last gate reason noted, so a refusal is
/// noted only when it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub state: State,
    pub in_flight: BTreeMap<String, String>,
    pub refusal: Option<String>,
    /// Since when a task slot has been open on every pass; `None` while none is.
    pub room_since_ms: Option<u64>,
    /// When the scheduler last judged the room.
    pub room_at_ms: Option<u64>,
    /// Each bound class as the last pass left it, for `scheduler bound`; `None` before the first pass.
    pub bound: Option<Vec<plan::Bound>>,
    /// The limit that bound task work on the last pass.
    pub task_limit: Option<plan::TaskLimit>,
    /// Each rune that ended the last pass with ready work and no call, with why; `None` before the first pass.
    pub uncalled: Option<BTreeMap<String, String>>,
}

fn moved<S: Copy + PartialEq + std::fmt::Debug, E: Copy + PartialEq + std::fmt::Debug>(table: &[(S, E, S)], state: S, event: E) -> Result<S, String> {
    table.iter().find(|(s, e, _)| *s == state && *e == event).map(|(_, _, to)| *to).ok_or_else(|| format!("{NAME}: {event:?} is not a move from state {state:?}"))
}

/// The next state, or a refusal naming the state the event is illegal in.
pub fn step(state: State, event: Trigger) -> Result<State, String> {
    moved(TABLE, state, event)
}

pub fn step_leg(leg: Leg, event: LegTrigger) -> Result<Leg, String> {
    moved(LEG_TABLE, leg, event)
}

/// The world `sentinel::plan` reads, from the snapshot.
pub fn call_world(snap: &Snapshot) -> plan::CallWorld {
    let roster: Vec<String> = if snap.roster.is_empty() { snap.ready_by_rune.keys().cloned().collect() } else { snap.roster.clone() };
    let (mut task, mut reactive) = (vec![], vec![]);
    for name in roster {
        let cfg = snap.runes.get(&name).cloned().unwrap_or_default();
        let rune = plan::Rune {
            live: i64::from(snap.live_by_rune.get(&name).copied().unwrap_or(0)),
            ready: snap.ready_by_rune.get(&name).copied().flatten().map(i64::from),
            single_flight: cfg.single_flight,
            elastic: cfg.elastic,
            uses: cfg.uses,
            reactive: cfg.reactive,
            name,
        };
        if rune.reactive {
            reactive.push(rune);
        } else {
            task.push(rune);
        }
    }
    plan::CallWorld {
        world: match snap.world {
            World::Running => plan::World::Running,
            World::Halted => plan::World::Halted,
            World::Draining => plan::World::Draining,
        },
        skew: snap.skew.clone(),
        capacity: match snap.capacity {
            Capacity::Clear => plan::Capacity::Open,
            Capacity::Paused => plan::Capacity::Paused,
            Capacity::Unknown => plan::Capacity::Unknown,
        },
        task,
        reactive,
        other_live: i64::from(snap.other_live),
        limits: snap.max_dwarves.map(|n| (plan::BUILD_DISK.to_string(), n)).into_iter().chain(snap.class_limits.iter().map(|(c, n)| (c.clone(), *n))).map(|(c, n)| (c, i64::from(n))).collect(),
        max_live_dwarves: snap.max_live.map(i64::from),
        throttled: snap.throttle,
        express_ready: snap.express_ready.is_some_and(|n| n > 0),
        urgent: snap.urgent.as_ref().map(|u| u.iter().map(|b| plan::Urgent { id: b.id.clone(), p0: b.p0, express: b.express }).collect()),
        reactive_dwarves: snap.reactive_dwarves.iter().map(|d| plan::ReactiveDwarf { rune: d.rune.clone(), bead: d.bead.clone(), started_ms: d.started_ms }).collect(),
        live_dwarves: snap.live_dwarves.iter().map(|d| plan::LiveDwarf { rune: d.rune.clone(), bead: d.bead.clone() }).collect(),
    }
}

/// What a `unit_start` act asks the runner to start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    pub rune: String,
    pub key: String,
    pub require_express: bool,
}

/// What a `preempt` act asks the runner to stop, and its log line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preempt {
    pub rune: String,
    pub bead: String,
    pub express: String,
    pub at_ms: u64,
}

/// The calls (`unit_start` per rune) and at most one preemption, from the snapshot alone.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let p = plan::plan(&call_world(snap));
    let mut next = carry.clone();
    next.state = State::Idle;
    next.in_flight.retain(|unit, _| snap.live_units.contains(unit));
    next.refusal = p.refusal.clone();
    next.room_since_ms = if p.task_room == Some(0) { None } else { carry.room_since_ms.or(Some(snap.now_ms)) };
    next.room_at_ms = Some(snap.now_ms);
    next.bound = Some(p.bound.clone());
    next.task_limit = p.task_limit.clone();
    next.uncalled = Some(p.uncalled.clone());
    let mut acts = vec![];
    if let Some(x) = &p.preempt {
        let key = format!("{}:{}", x.bead, x.express);
        let input = serde_json::to_string(&Preempt { rune: x.rune.clone(), bead: x.bead.clone(), express: x.express.clone(), at_ms: snap.now_ms }).unwrap_or_default();
        acts.push(Act::new(PREEMPT, &key).with_input(&input));
        acts.push(Act::new(PREEMPT_LOG, &key).with_input(input));
    }
    for (n, c) in p.calls.iter().enumerate() {
        let key = format!("{}-{}-{n}", c.rune, snap.now_ms);
        let Ok(unit) = units::unit_name(NAME, &key) else { continue };
        next.in_flight.insert(unit.clone(), c.rune.clone());
        let start = Start { rune: c.rune.clone(), key, require_express: c.require_express };
        acts.push(Act::new(UNIT_START, unit).with_input(serde_json::to_string(&start).unwrap_or_default()));
    }
    (next, acts)
}

/// The observe probe's input: the carry (which T3 reads back) and the pinned config the probe
/// reads the world with.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObserveIn {
    #[serde(flatten)]
    pub carry: Carry,
    #[serde(default)]
    pub pinned: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentinelCall {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl SentinelCall {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        SentinelCall { cadence: Cadence::Calendar(Calendar::parse(CALENDAR).expect("the minute calendar parses")), pinned }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sentinel-call/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn observe(&self, carry: &Carry, next_at: u64) -> Vec<Command> {
        let input = ObserveIn { carry: carry.clone(), pinned: self.pinned.clone() };
        kit::on_tick(&SHAPE, serde_json::to_string(&input).unwrap_or_default(), next_at)
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("state"), runtime::rows::Part::Field("refusal"), runtime::rows::Part::Field("room_since_ms"), runtime::rows::Part::Field("room_at_ms"), runtime::rows::Part::Field("bound"), runtime::rows::Part::Field("task_limit"), runtime::rows::Part::Map { name: "in_flight", drop: &[] }, runtime::rows::Part::Map { name: "uncalled", drop: &[] }];

impl Handler for SentinelCall {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, CALL_WANTED, super::czar::CALL_REQUESTED, UNIT_RAN, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        // Commands land on the event's own stream, so only this task's stream is answered: a
        // `call.wanted` or `unit.ran` routed here wakes the call; elsewhere the minute does.
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK || ev.kind.tag == CALL_WANTED || ev.kind.tag == super::czar::CALL_REQUESTED {
            return self.observe(&carry, self.cadence.next_after(ev.at));
        }
        if let Some(ran) = UnitRan::from_event_kind(&ev.kind) {
            if ran.key_for(NAME).is_none() {
                return vec![];
            }
            let mut next = carry.clone();
            next.in_flight.remove(&ran.handle.name);
            let mut out = kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, vec![]);
            out.extend(self.observe(&next, self.cadence.next_after(ev.at)));
            return out;
        }
        // A start lost at a restart may have started: the next observe says what runs.
        if let Some(spec) = crate::watchers::answered_spec(view, ev).filter(|s| s.name == UNIT_START && !crate::watchers::lost_at_restart(ev)) {
            let Ok(start) = serde_json::from_str::<Start>(&spec.input) else { return vec![] };
            let ok = ev.kind.tag == EFFECT_SUCCEEDED;
            let mut next = carry.clone();
            if !ok {
                if let Ok(unit) = units::unit_name(NAME, &start.key) {
                    next.in_flight.remove(&unit);
                }
            }
            let metric = Act::new(METRIC, &start.key).with_input(serde_json::json!({"rune": start.rune, "result": if ok { "ok" } else { "failed" }}).to_string());
            return kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, vec![metric])
                .into_iter()
                .map(|c| match c {
                    Command::Effect(s) if s.name == METRIC => Command::Probe(EffectSpec::probe(s.name, s.input, s.deadline_ms)),
                    c => c,
                })
                .collect();
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        let mut out = kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts);
        if next.refusal != carry.refusal {
            if let Some(why) = &next.refusal {
                out.push(Command::Note(EventKind::new(REFUSED, why)));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// The runners: the world read in-process, and the calls made through the Units port.

fn pinned_num(pinned: &BTreeMap<String, String>, key: &str) -> Option<u32> {
    pinned.get(key).and_then(|v| v.trim().parse().ok())
}

fn run_dir(e: &Effects, pinned: &BTreeMap<String, String>) -> PathBuf {
    pinned.get("SPIRA_RUN").filter(|r| !r.is_empty()).map(PathBuf::from).unwrap_or_else(|| e.world.run_dir.clone())
}

/// What a spira-claim verb reads from its environment: the pinned keys, then this process's
/// config, then its environment.
fn claim(pinned: &BTreeMap<String, String>, args: &[String]) -> sentinel::claim::Outcome {
    let lookup = |k: &str| pinned.get(k).cloned().or_else(|| spira_config::process::cfg(k).ok()).or_else(|| std::env::var(k).ok());
    sentinel::claim::dispatch_in(args, &mut std::io::empty(), &lookup)
}

/// Whether an express bead carries `labels` and none of the `rune-exclude` answer's labels.
fn any_express(labels: String, exclude: sentinel::claim::Outcome, count: impl FnOnce(&[String]) -> sentinel::claim::Outcome) -> bool {
    // A refusal prints nothing, and counting with no exclusions would count everything.
    if exclude.code != 0 {
        return false;
    }
    let n = count(&["ready-count".into(), labels, exclude.out.trim().to_string(), "--express".into()]);
    n.out.trim().parse::<i64>().unwrap_or(0) > 0
}

fn world_of(run: &Path) -> World {
    if run.join("world.halted").is_file() {
        World::Halted
    } else if run.join("world.draining").is_file() {
        World::Draining
    } else {
        World::Running
    }
}

fn skew() -> Option<String> {
    let own = std::env::var(spira_config::RELEASE_ENV).ok().filter(|r| !r.is_empty())?;
    spira_config::release_skew::skew(Path::new(&own)).map(|s| s.describe())
}

fn capacity_of(run: &Path, now_s: i64) -> Capacity {
    match sentinel::capacity::pause_state(&run.join("capacity-pause"), now_s) {
        sentinel::capacity::PauseState::Open => Capacity::Clear,
        sentinel::capacity::PauseState::Paused { .. } => Capacity::Paused,
        sentinel::capacity::PauseState::Unknown => Capacity::Unknown,
    }
}

/// The rune a unit this task started was called for: its key is `<rune>-<at>-<n>`.
fn rune_of_key(key: &str) -> Option<String> {
    key.rsplitn(3, '-').nth(2).map(str::to_string)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The live dwarf fleet as the fleet ceiling counts it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fleet {
    pub live_by_rune: BTreeMap<String, u32>,
    /// Live dwarves of no rune on the roster.
    pub other_live: u32,
    /// The live units this task started, by name.
    pub live_units: Vec<String>,
}

impl Fleet {
    /// Every live dwarf, on the roster or not: what `SPIRA_MAX_LIVE_DWARVES` bounds.
    pub fn total(&self) -> u32 {
        self.live_by_rune.values().sum::<u32>() + self.other_live
    }
}

/// The live dwarves the unit manager lists, by rune: the units this task started (their key
/// names the rune) and the `spira-dwarf-*` units started by name (`rune_name` gives each rune's
/// `RUNE_NAME`, the name their units carry). The set and the liveness rule are
/// `runtime::units::{dwarf_globs, is_live_dwarf}`, the ones dwarves.sh counts by (sp-bpnmzg:
/// listing `spira-run-sentinel-call-*` unescaped matched none of this task's own units, so
/// the ceiling never bound on them).
pub fn fleet(unitp: &dyn units::Units, roster: &[String], rune_name: &dyn Fn(&str) -> String) -> Result<Fleet, String> {
    let mut fleet = Fleet::default();
    let mut seen = std::collections::BTreeSet::new();
    for glob in units::dwarf_globs() {
        for d in unitp.list(&glob).map_err(|e| e.to_string())? {
            if !units::is_live_dwarf(&d.name, d.state) || !seen.insert(d.name.clone()) {
                continue;
            }
            let ran = UnitRan { handle: units::UnitHandle::named(&d.name), exit: units::Exit::Unknown, duration_ms: 0 };
            let rune = match ran.key_for(NAME) {
                Some(key) => {
                    fleet.live_units.push(d.name.clone());
                    rune_of_key(&key).filter(|r| roster.contains(r))
                }
                None => {
                    let rest = d.name.trim_start_matches(DWARF_PREFIX);
                    roster.iter().find(|f| rest.starts_with(&format!("{}-", rune_name(f)))).cloned()
                }
            };
            match rune {
                Some(r) => *fleet.live_by_rune.entry(r).or_default() += 1,
                None => fleet.other_live += 1,
            }
        }
    }
    Ok(fleet)
}

/// `sentinel-call.observe`: the world, the fleet, the runes and the machine's ready counts.
pub fn observe(e: &Effects, input: &str) -> Result<String, String> {
    let req: ObserveIn = serde_json::from_str(input).unwrap_or_default();
    let pinned = &req.pinned;
    let run = run_dir(e, pinned);
    let home = spira_config::resolve::locate_home_for_process()?;
    let unitp = e.units.as_deref().ok_or("no unit manager configured")?;
    let roster_all = spira_config::runes::spira_runes(&home, pinned.get("SPIRA_RUNES").map(String::as_str).filter(|v| !v.is_empty()));
    let get = |f: &str, k: &str, d: &str| spira_config::runes::rune_get(&home, f, k, d);
    let mut uses: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let roster: Vec<String> = roster_all
        .split_whitespace()
        .filter(|f| get(f, "RUNE_CALL", "auto") == "auto" && get(f, "RUNE_ROLE", "task") != "party")
        .filter(|f| match spira_config::runes::rune_uses(&home, f) {
            Ok(u) => {
                uses.insert(f.to_string(), u);
                true
            }
            Err(why) => {
                eprintln!("CHECK7 {why} — not calling");
                false
            }
        })
        .map(str::to_string)
        .collect();
    let runes: BTreeMap<String, RuneCfg> = roster
        .iter()
        .map(|f| {
            let cfg = RuneCfg {
                single_flight: get(f, "RUNE_SINGLE_FLIGHT", "0") == "1",
                elastic: get(f, "RUNE_ELASTIC", "0") == "1",
                uses: uses.remove(f).unwrap_or_default(),
                reactive: get(f, "RUNE_REACTIVE", "0") == "1",
            };
            (f.clone(), cfg)
        })
        .collect();
    let snap_ms = now_ms();
    let mut snap = Snapshot { world: world_of(&run), skew: skew(), capacity: capacity_of(&run, (snap_ms / 1000) as i64), roster: roster.clone(), runes, now_ms: snap_ms, ..Snapshot::default() };
    snap.max_dwarves = pinned_num(pinned, "SPIRA_MAX_DWARVES");
    snap.max_live = pinned_num(pinned, "SPIRA_MAX_LIVE_DWARVES");
    for (class, key) in [("cpu", "SPIRA_LIMIT_CPU"), ("memory", "SPIRA_LIMIT_MEMORY"), ("network", "SPIRA_LIMIT_NETWORK")] {
        if let Some(n) = pinned_num(pinned, key) {
            snap.class_limits.insert(class.to_string(), n);
        }
    }
    let rune_name = |f: &str| get(f, "RUNE_NAME", f);
    let fleet = fleet(unitp, &roster, &rune_name)?;
    (snap.live_by_rune, snap.other_live, snap.live_units) = (fleet.live_by_rune, fleet.other_live, fleet.live_units);
    let _one_view = sentinel::claim::TickView::begin();
    let bulk = claim(pinned, &["bulk-ready-by-rune".into()]);
    let counts: BTreeMap<String, usize> = if bulk.code == 0 { plan::parse_cache(&bulk.out).into_iter().collect() } else { BTreeMap::new() };
    for f in &roster {
        let n = match counts.get(f) {
            Some(n) => Some(*n as u32),
            None => {
                let one = claim(pinned, &["rune-ready".into(), f.clone()]);
                (one.code == 0).then(|| one.out.trim().parse().unwrap_or(0))
            }
        };
        snap.ready_by_rune.insert(f.clone(), n);
    }
    let task: Vec<String> = roster.iter().filter(|f| !snap.runes[*f].reactive).cloned().collect();
    snap.urgent = if task.is_empty() {
        Some(vec![])
    } else {
        let o = claim(pinned, &std::iter::once("urgent".to_string()).chain(task.iter().cloned()).collect::<Vec<_>>());
        (o.code == 0).then(|| {
            o.out
                .lines()
                .filter_map(|l| {
                    let f: Vec<&str> = l.split('\t').collect();
                    let [id, prio, _, express] = f[..] else { return None };
                    Some(UrgentBead { id: id.into(), p0: prio == "0", express: express == "1" })
                })
                .collect()
        })
    };
    let express = task.iter().any(|f| {
        let Ok(p) = spira_config::runes::rune_predicate(&home, f) else { return false };
        if p.labels.is_empty() {
            return false;
        }
        let ex = claim(pinned, &["rune-exclude".into(), f.clone(), p.exclude_labels]);
        any_express(p.labels, ex, |args| claim(pinned, args))
    });
    snap.express_ready = Some(u32::from(express));
    snap.throttle = plan::ck7_throttled(run.join("queue-throttled").is_file(), pinned.get("SPIRA_QUEUE_THROTTLE_OVERRIDE").map_or("", String::as_str));
    scan_pidfiles(&run, &mut snap, &rune_name);
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The live dwarves the pidfiles in `run` name, for each rune on `snap`'s roster.
fn scan_pidfiles(run: &Path, snap: &mut Snapshot, rune_name: &dyn Fn(&str) -> String) {
    let names: Vec<(String, String)> = snap.roster.iter().map(|f| (f.clone(), rune_name(f))).collect();
    for entry in std::fs::read_dir(run).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_prefix("dwarf-").and_then(|n| n.strip_suffix(".pid")) else { continue };
        let Some((rune, unit)) = names.iter().find(|(_, u)| stem.starts_with(&format!("{u}-"))) else { continue };
        let bead = stem[unit.len() + 1..].to_string();
        if bead.starts_with("sweep-") || !sentinel::dwarf_alive(&entry.path()) {
            continue;
        }
        let started_ms = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
        let dwarf = LiveUnit { unit: format!("dwarf-{stem}"), rune: rune.clone(), bead, started_ms };
        if snap.runes[rune].reactive {
            snap.reactive_dwarves.push(dwarf.clone());
        }
        snap.live_dwarves.push(dwarf);
    }
    snap.live_dwarves.sort_by(|a, b| b.started_ms.cmp(&a.started_ms).then_with(|| a.unit.cmp(&b.unit)));
}

/// `dwarf` on this process's PATH, strictly: a bare name is never handed to the unit manager.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// `sentinel-call.unit_start`: the dwarf for one rune, as a unit under the naming rule. Each
/// gate decide read is read again here, so a world that halted since the probe calls nobody.
pub fn unit_start(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let start: Start = serde_json::from_str(&spec.input).map_err(|e| format!("{UNIT_START}: {e}"))?;
    let run = e.world.run_dir.clone();
    match world_of(&run) {
        World::Running => {}
        w => return Err(format!("{UNIT_START} {}: the world is {w:?} — not calling", start.rune)),
    }
    if let Some(s) = skew() {
        return Err(format!("{UNIT_START} {}: RELEASE SKEW — {s}", start.rune));
    }
    if capacity_of(&run, (now_ms() / 1000) as i64) != Capacity::Clear {
        return Err(format!("{UNIT_START} {}: the account's capacity is paused or unreadable — not calling", start.rune));
    }
    let home = spira_config::resolve::locate_home_for_process()?;
    if !spira_config::runes::runes_dir(&home).join(format!("{}.rune", start.rune)).is_file() {
        return Err(format!("{UNIT_START} {}: no rune in the runes", start.rune));
    }
    let dwarf = on_path("dwarf").ok_or_else(|| format!("{UNIT_START} {}: dwarf not found on PATH — not calling", start.rune))?;
    let mut env = spira_config::release_env::dwarf_unit_env()?;
    if start.require_express {
        env.push(("SPIRA_REQUIRE_EXPRESS".into(), "1".into()));
    }
    let unit = UnitSpec { task: NAME.into(), key: start.key.clone(), argv: vec![dwarf.to_string_lossy().into_owned(), "--home".into(), home.to_string_lossy().into_owned(), start.rune.clone()], env, cwd: None };
    let timeout = spira_config::runes::rune_get(&home, &start.rune, "RUNE_TIMEOUT_SECONDS", "3600");
    let input = serde_json::to_string(&units::StartSpec { unit, props: units::dwarf_props(&timeout)? }).map_err(|e| e.to_string())?;
    e.unit(&EffectSpec::keyed(units::UNIT_START, spec.key.clone(), input, spec.deadline_ms))
}

/// `sentinel-call.preempt`: stop the unit the reactive dwarf on `bead` runs in, found by the pid
/// its pidfile names.
pub fn preempt(e: &Effects, input: &str) -> Result<String, String> {
    let p: Preempt = serde_json::from_str(input).map_err(|e| format!("{PREEMPT}: {e}"))?;
    let unitp = e.units.as_deref().ok_or("no unit manager configured")?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let unit_name = spira_config::runes::rune_get(&home, &p.rune, "RUNE_NAME", &p.rune);
    let pidfile = e.world.run_dir.join(format!("dwarf-{unit_name}-{}.pid", p.bead));
    let pid: u32 = std::fs::read_to_string(&pidfile).map_err(|err| format!("{PREEMPT}: {}: {err}", pidfile.display()))?.split_whitespace().next().and_then(|s| s.parse().ok()).ok_or_else(|| format!("{PREEMPT}: {} names no pid", pidfile.display()))?;
    let mut found = vec![];
    for glob in units::dwarf_globs() {
        found.extend(unitp.list(&glob).map_err(|e| e.to_string())?);
    }
    let unit = found.into_iter().find(|d| d.main_pid == Some(pid)).ok_or_else(|| format!("{PREEMPT}: no live unit runs pid {pid} for {}", p.bead))?;
    unitp.stop(&units::UnitHandle::named(&unit.name)).map_err(|e| e.to_string())?;
    Ok(format!("preempted a {} reactive dwarf on {} for P0 express {}", p.rune, p.bead, p.express))
}

/// `sentinel-call.preempt_log`: one line in `lane-preemptions`, as the binary wrote it.
pub fn preempt_log(e: &Effects, input: &str) -> Result<String, String> {
    use std::io::Write;
    let p: Preempt = serde_json::from_str(input).map_err(|e| format!("{PREEMPT_LOG}: {e}"))?;
    let path = e.world.run_dir.join("lane-preemptions");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    writeln!(f, "{}\t{}\t{}\t{}", p.at_ms / 1000, p.rune, p.bead, p.express).map_err(|err| err.to_string())?;
    Ok(path.display().to_string())
}

/// `sentinel-call.metric`: the `call` row of the metrics store, in-process.
pub fn metric(e: &Effects, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{METRIC}: {e}"))?;
    let fields: Vec<(String, serde_json::Value)> = ["rune", "result"].iter().map(|k| (k.to_string(), v[*k].clone())).collect();
    tsd::store::Pusher::new(e.world.metrics_endpoint.clone()).push("call", &tsd::now_iso(), &fields).map_err(|e| e.to_string())?;
    Ok("call".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tlxtmi1)]
    #[test]
    fn a_refused_rune_exclude_leaves_nothing_express_and_counts_nothing() {
        let said = |code, out: &str| sentinel::claim::Outcome { code, out: out.into(), err: String::new() };
        assert!(!any_express("plan".into(), said(2, ""), |_| panic!("counted with no exclusions")));
        let mut asked = Vec::new();
        assert!(any_express("plan".into(), said(0, "repo:spira\n"), |a| {
            asked = a.to_vec();
            said(0, "1")
        }));
        assert_eq!(asked, ["ready-count", "plan", "repo:spira", "--express"]);
    }

    #[test_id(twdkp6t)]
    #[test]
    fn the_pidfile_scan_names_every_rostered_dwarf_and_keeps_the_preemption_list_reactive() {
        let run = testkit::TempDir::new("sentinel-call-pidfiles");
        for stem in ["dwarf-builder-sp-a", "dwarf-skuld-sp-b", "dwarf-builder-sweep-7", "dwarf-skuld-sweep-8"] {
            std::fs::write(run.join(format!("{stem}.pid")), "1").unwrap();
            std::fs::write(run.join(format!("{stem}.lease")), i64::MAX.to_string()).unwrap();
        }
        std::fs::write(run.join("dwarf-builder-sp-ended.pid"), "1").unwrap();
        let runes = [("builder", false), ("skuld", true)];
        let mut snap = Snapshot { roster: runes.map(|(f, _)| f.to_string()).into(), runes: runes.map(|(f, reactive)| (f.to_string(), RuneCfg { reactive, ..RuneCfg::default() })).into(), ..Snapshot::default() };
        scan_pidfiles(&run, &mut snap, &|f| f.to_string());
        let pairs = |v: &[LiveUnit]| v.iter().map(|d| (d.rune.clone(), d.bead.clone())).collect::<std::collections::BTreeSet<_>>();
        let pair = |f: &str, b: &str| (f.to_string(), b.to_string());
        assert_eq!(pairs(&snap.live_dwarves), [pair("builder", "sp-a"), pair("skuld", "sp-b")].into());
        assert_eq!(pairs(&snap.reactive_dwarves), [pair("skuld", "sp-b")].into());
    }

    #[test_id(tyfqu8j)]
    #[test]
    fn the_pidfile_scan_lists_the_newest_dwarf_first() {
        let run = testkit::TempDir::new("sentinel-call-pidfile-order");
        for (bead, secs) in [("sp-b", 20), ("sp-a", 10), ("sp-c", 30)] {
            let pidfile = run.join(format!("dwarf-builder-{bead}.pid"));
            std::fs::write(&pidfile, "1").unwrap();
            std::fs::write(run.join(format!("dwarf-builder-{bead}.lease")), i64::MAX.to_string()).unwrap();
            std::fs::File::options().write(true).open(&pidfile).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
        }
        let mut snap = Snapshot { roster: vec!["builder".into()], runes: [("builder".to_string(), RuneCfg::default())].into(), ..Snapshot::default() };
        scan_pidfiles(&run, &mut snap, &|f| f.to_string());
        assert_eq!(snap.live_dwarves.iter().map(|d| d.bead.as_str()).collect::<Vec<_>>(), ["sp-c", "sp-b", "sp-a"]);
    }

    #[test_id(tfdx0qo)]
    #[test]
    fn the_carried_pool_limit_names_the_snapshots_build_disk_dwarves() {
        let dwarf = |rune: &str, bead: &str| LiveUnit { rune: rune.into(), bead: bead.into(), ..LiveUnit::default() };
        let snap = Snapshot {
            live_by_rune: [("builder".to_string(), 1), ("skuld".to_string(), 1)].into(),
            roster: vec!["builder".into(), "skuld".into()],
            runes: [("builder".to_string(), RuneCfg { uses: vec![plan::BUILD_DISK.into()], ..RuneCfg::default() })].into(),
            max_dwarves: Some(1),
            live_dwarves: vec![dwarf("skuld", "sp-s"), dwarf("builder", "sp-b")],
            ..Snapshot::default()
        };
        assert_eq!(decide(&Carry::default(), &snap).0.task_limit.unwrap().holders, ["sp-b"]);
    }
}
