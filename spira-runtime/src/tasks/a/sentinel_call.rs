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
//! Snapshot `{ world, skew, capacity, live_by_rune, ready_by_rune, urgent, express_ready, throttle, lane_last }`:
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
//! | `lane_last` | `$SPIRA_RUN/lane-round-robin` (`call.rs:747`) | carried state (`sentinel-call.state`), not a file |
//! | youngest lane dwarf | pidfile mtimes in `$SPIRA_RUN` (`call.rs:405-428`) | `unit.facts` start times |
//!
//! A failed ready answer is `None`, never 0 (`call.rs:314-325`, sp-hh599).
//!
//! ## Decide
//!
//! Pure `decide(carry, snap) -> (carry, Vec<Call>, Option<Preempt>)`, ported from the existing
//! pure helpers: `check7_pool_decision`, `lane_rotate`, `ck7_pool`, `ck7_throttled`, `ck7_fill_cap`
//! (`call.rs:127-173`) plus the gates of `call_rune` (`call.rs:577-675`: world, skew, capacity,
//! fleet ceiling `SPIRA_MAX_LIVE_DWARVES`, lane spill `SPIRA_LANES_MAX_LIVE`, elastic hold-back,
//! only-slot hold) and the loop of `ck7_call_body` (`call.rs:728-850`). The per-pass time budget
//! (`SPIRA_SENTINEL_PASS_BUDGET_SECS`, `call.rs:773`) becomes moot: decide is O(runes), no IO.
//! Carry: `lane_last`, `in_flight: {unit → rune}` (units started, no `unit.ran` yet).
//! Fixtures: the existing `call::tests` table tests (`call.rs:951-1000`) and `tests.rs:593-670`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sentinel-call.unit_start` (`unit.start` of `spira-dwarf-<rune>-<now>` running `dwarf --home <home> <rune>`, props from `call_argv` `call.rs:519-533`) | unit | 10 s (unverified) | unit name | world halted/draining, skew, capacity paused/unknown, rune not on roster, `dwarf` not resolvable (`call.rs:680`) |
//! | `sentinel-call.preempt` (stop youngest lane dwarf; today `slay.sh --bead` `call.rs:451-462`) | unit (`unit.stop`) | 30 s (unverified) | `(bead, express)` | no lane dwarf; no task rune can take the slot (`call.rs:439-445`) |
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
//! `SPIRA_MAX_LIVE_DWARVES`, `SPIRA_LANES_MAX_LIVE`, `SPIRA_LANES`, `SPIRA_QUEUE_THROTTLE_OVERRIDE`,
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
//! (`dwarf_props`: `TimeoutStartSec` from `RUNE_TIMEOUT_SECONDS`, the idle IO class, IO weight
//! 10) as `UnitProps` on the start; its `ExecStopPost` refill (the `--call-only` re-exec) is
//! retired: the refill is the dwarf's own `unit.ran` on this stream, and the minute calendar. `SPIRA_CALL`, `SPIRA_SYSTEMCTL`
//! and `SPIRA_SENTINEL_PASS_BUDGET_SECS` are retired (the port replaces the first two, decide
//! is O(runes)); the throttle stamp and the capacity pause are the paths under `SPIRA_RUN`
//! the binary defaulted them to; `SPIRA_LANES` only named the lanes in a log line and is dropped.
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
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_RUNES", "SPIRA_MAX_DWARVES", "SPIRA_MAX_LIVE_DWARVES", "SPIRA_LANES_MAX_LIVE", "SPIRA_QUEUE_THROTTLE_OVERRIDE"];
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

/// One live dwarf unit, for the youngest-lane-dwarf preemption.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveUnit {
    pub unit: String,
    pub rune: String,
    pub bead: String,
    pub started_ms: u64,
}

/// A rune's own call settings (`RUNE_LANE`, `RUNE_MAX_CONCURRENT`, `RUNE_ELASTIC`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuneCfg {
    pub lane: bool,
    pub max_concurrent: u32,
    pub elastic: bool,
}

impl Default for RuneCfg {
    /// `rune_get`'s own defaults: a task rune, one at a time, not elastic.
    fn default() -> Self {
        RuneCfg { lane: false, max_concurrent: 1, elastic: false }
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
    /// `None` when the machine could not answer: the lanes yield.
    pub urgent: Option<Vec<UrgentBead>>,
    pub express_ready: Option<u32>,
    pub throttle: bool,
    pub lane_dwarves: Vec<LiveUnit>,
    /// The fleet ceiling `SPIRA_MAX_LIVE_DWARVES` and lane spill `SPIRA_LANES_MAX_LIVE`, as pinned.
    pub max_live: Option<u32>,
    pub lanes_max_live: Option<u32>,
    /// The task pool `SPIRA_MAX_DWARVES`, as pinned.
    pub max_dwarves: Option<u32>,
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

/// Carried between ticks: the machine, the lane rotation, and every unit started with no
/// `unit.ran` yet (unit name → rune); `refusal` is the last gate reason noted, so a refusal is
/// noted only when it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub state: State,
    pub lane_last: Option<String>,
    pub in_flight: BTreeMap<String, String>,
    pub refusal: Option<String>,
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

/// The world `sentinel::plan` reads, from the snapshot and the carried lane rotation.
pub fn call_world(carry: &Carry, snap: &Snapshot) -> plan::CallWorld {
    let roster: Vec<String> = if snap.roster.is_empty() { snap.ready_by_rune.keys().cloned().collect() } else { snap.roster.clone() };
    let (mut task, mut lanes) = (vec![], vec![]);
    for name in roster {
        let cfg = snap.runes.get(&name).cloned().unwrap_or_default();
        let rune = plan::Rune {
            live: i64::from(snap.live_by_rune.get(&name).copied().unwrap_or(0)),
            ready: snap.ready_by_rune.get(&name).copied().flatten().map(i64::from),
            lane: cfg.lane,
            max_concurrent: i64::from(cfg.max_concurrent),
            elastic: cfg.elastic,
            name,
        };
        if rune.lane {
            lanes.push(rune);
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
        lanes,
        other_live: i64::from(snap.other_live),
        max_dwarves: snap.max_dwarves.map(i64::from),
        max_live_dwarves: snap.max_live.map(i64::from),
        lanes_max_live: snap.lanes_max_live.map(i64::from),
        throttled: snap.throttle,
        express_ready: snap.express_ready.is_some_and(|n| n > 0),
        urgent: snap.urgent.as_ref().map(|u| u.iter().map(|b| plan::Urgent { id: b.id.clone(), p0: b.p0, express: b.express }).collect()),
        lane_last: carry.lane_last.clone().unwrap_or_default(),
        lane_dwarves: snap.lane_dwarves.iter().map(|d| plan::LaneDwarf { lane: d.rune.clone(), bead: d.bead.clone(), started_ms: d.started_ms }).collect(),
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
    pub lane: String,
    pub bead: String,
    pub express: String,
    pub at_ms: u64,
}

/// The calls (`unit_start` per rune) and at most one preemption, from the snapshot alone.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let p = plan::plan(&call_world(carry, snap));
    let mut next = carry.clone();
    next.state = State::Idle;
    next.in_flight.retain(|unit, _| snap.live_units.contains(unit));
    if let Some(l) = &p.lane_last {
        next.lane_last = Some(l.clone());
    }
    next.refusal = p.refusal.clone();
    let mut acts = vec![];
    if let Some(x) = &p.preempt {
        let key = format!("{}:{}", x.bead, x.express);
        let input = serde_json::to_string(&Preempt { lane: x.lane.clone(), bead: x.bead.clone(), express: x.express.clone(), at_ms: snap.now_ms }).unwrap_or_default();
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
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sentinel-call/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn observe(&self, carry: &Carry, next_at: u64) -> Vec<Command> {
        let input = ObserveIn { carry: carry.clone(), pinned: self.pinned.clone() };
        kit::on_tick(&SHAPE, serde_json::to_string(&input).unwrap_or_default(), next_at)
    }
}

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
        if let Some(spec) = crate::watchers::answered_spec(view, ev).filter(|s| s.name == UNIT_START) {
            let Ok(start) = serde_json::from_str::<Start>(&spec.input) else { return vec![] };
            let ok = ev.kind.tag == EFFECT_SUCCEEDED;
            let mut next = carry.clone();
            if !ok {
                if let Ok(unit) = units::unit_name(NAME, &start.key) {
                    next.in_flight.remove(&unit);
                }
            }
            let metric = Act::new(METRIC, &start.key).with_input(serde_json::json!({"rune": start.rune, "result": if ok { "ok" } else { "failed" }}).to_string());
            return kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, vec![metric]);
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

fn capacity_of(run: &Path) -> Capacity {
    match sentinel::capacity::pause_state(&run.join("capacity-pause")) {
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
    let roster: Vec<String> = roster_all.split_whitespace().filter(|f| get(f, "RUNE_CALL", "auto") == "auto" && get(f, "RUNE_ROLE", "task") != "party").map(str::to_string).collect();
    let runes: BTreeMap<String, RuneCfg> = roster
        .iter()
        .map(|f| (f.clone(), RuneCfg { lane: !get(f, "RUNE_LANE", "").is_empty(), max_concurrent: get(f, "RUNE_MAX_CONCURRENT", "1").trim().parse().unwrap_or(1), elastic: get(f, "RUNE_ELASTIC", "0") == "1" }))
        .collect();
    let mut snap = Snapshot { world: world_of(&run), skew: skew(), capacity: capacity_of(&run), roster: roster.clone(), runes, now_ms: now_ms(), ..Snapshot::default() };
    snap.max_dwarves = pinned_num(pinned, "SPIRA_MAX_DWARVES");
    snap.max_live = pinned_num(pinned, "SPIRA_MAX_LIVE_DWARVES");
    snap.lanes_max_live = pinned_num(pinned, "SPIRA_LANES_MAX_LIVE");
    let rune_name = |f: &str| get(f, "RUNE_NAME", f);
    let fleet = fleet(unitp, &roster, &rune_name)?;
    (snap.live_by_rune, snap.other_live, snap.live_units) = (fleet.live_by_rune, fleet.other_live, fleet.live_units);
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
    let task: Vec<String> = roster.iter().filter(|f| !snap.runes[*f].lane).cloned().collect();
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
        let n = claim(pinned, &["ready-count".into(), p.labels, ex.out.trim().to_string(), "--express".into()]);
        n.out.trim().parse::<i64>().unwrap_or(0) > 0
    });
    snap.express_ready = Some(u32::from(express));
    snap.throttle = plan::ck7_throttled(run.join("queue-throttled").is_file(), pinned.get("SPIRA_QUEUE_THROTTLE_OVERRIDE").map_or("", String::as_str));
    let lane_names: Vec<(String, String)> = roster.iter().filter(|f| snap.runes[*f].lane).map(|f| (f.clone(), get(f, "RUNE_NAME", f))).collect();
    for entry in std::fs::read_dir(&run).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_prefix("dwarf-").and_then(|n| n.strip_suffix(".pid")) else { continue };
        let Some((lane, unit)) = lane_names.iter().find(|(_, u)| stem.starts_with(&format!("{u}-"))) else { continue };
        let bead = stem[unit.len() + 1..].to_string();
        if bead.starts_with("sweep-") || !sentinel::dwarf_alive(&entry.path()) {
            continue;
        }
        let started_ms = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
        snap.lane_dwarves.push(LiveUnit { unit: format!("dwarf-{stem}"), rune: lane.clone(), bead, started_ms });
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
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
    if capacity_of(&run) != Capacity::Clear {
        return Err(format!("{UNIT_START} {}: the account's capacity is paused or unreadable — not calling", start.rune));
    }
    let home = spira_config::resolve::locate_home_for_process()?;
    if !spira_config::runes::runes_dir(&home).join(format!("{}.rune", start.rune)).is_file() {
        return Err(format!("{UNIT_START} {}: no rune in the runes", start.rune));
    }
    let dwarf = on_path("dwarf").ok_or_else(|| format!("{UNIT_START} {}: dwarf not found on PATH — not calling", start.rune))?;
    let release = std::env::var(spira_config::RELEASE_ENV).unwrap_or_default();
    let mut env = vec![
        ("SPIRA_RELEASE".to_string(), release.clone()),
        ("PATH".to_string(), format!("{release}/bin:{release}/spira:/usr/local/bin:/usr/bin:/bin")),
        ("HOME".to_string(), std::env::var("HOME").unwrap_or_default()),
        ("SPIRA_TOML".to_string(), spira_config::process::spec()?),
    ];
    if start.require_express {
        env.push(("SPIRA_REQUIRE_EXPRESS".into(), "1".into()));
    }
    let unit = UnitSpec { task: NAME.into(), key: start.key.clone(), argv: vec![dwarf.to_string_lossy().into_owned(), "--home".into(), home.to_string_lossy().into_owned(), start.rune.clone()], env, cwd: None };
    let timeout = spira_config::runes::rune_get(&home, &start.rune, "RUNE_TIMEOUT_SECONDS", "3600");
    let input = serde_json::to_string(&units::StartSpec { unit, props: dwarf_props(&timeout)? }).map_err(|e| e.to_string())?;
    e.unit(&EffectSpec { name: units::UNIT_START.into(), input, deadline_ms: spec.deadline_ms })
}

/// The properties `call_argv` gave every dwarf call (`sentinel/src/call.rs`): the rune's
/// `RUNE_TIMEOUT_SECONDS` as `TimeoutStartSec`, the idle IO class, IO weight 10. No
/// `ExecStopPost`: the `--call-only` re-exec is retired, and the dwarf's end is its `unit.ran`.
pub fn dwarf_props(timeout_secs: &str) -> Result<units::UnitProps, String> {
    let secs: u64 = timeout_secs.trim().parse().map_err(|_| format!("RUNE_TIMEOUT_SECONDS must be whole seconds, got {timeout_secs:?}"))?;
    Ok(units::UnitProps { start_timeout_ms: Some(secs * 1000), io_class: Some("idle".into()), io_weight: Some(10), ..units::UnitProps::default() })
}

/// `sentinel-call.preempt`: stop the unit the lane dwarf on `bead` runs in, found by the pid
/// its pidfile names.
pub fn preempt(e: &Effects, input: &str) -> Result<String, String> {
    let p: Preempt = serde_json::from_str(input).map_err(|e| format!("{PREEMPT}: {e}"))?;
    let unitp = e.units.as_deref().ok_or("no unit manager configured")?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let unit_name = spira_config::runes::rune_get(&home, &p.lane, "RUNE_NAME", &p.lane);
    let pidfile = e.world.run_dir.join(format!("dwarf-{unit_name}-{}.pid", p.bead));
    let pid: u32 = std::fs::read_to_string(&pidfile).map_err(|err| format!("{PREEMPT}: {}: {err}", pidfile.display()))?.split_whitespace().next().and_then(|s| s.parse().ok()).ok_or_else(|| format!("{PREEMPT}: {} names no pid", pidfile.display()))?;
    let mut found = vec![];
    for glob in units::dwarf_globs() {
        found.extend(unitp.list(&glob).map_err(|e| e.to_string())?);
    }
    let unit = found.into_iter().find(|d| d.main_pid == Some(pid)).ok_or_else(|| format!("{PREEMPT}: no live unit runs pid {pid} for {}", p.bead))?;
    unitp.stop(&units::UnitHandle::named(&unit.name)).map_err(|e| e.to_string())?;
    Ok(format!("preempted a {} lane dwarf on {} for P0 express {}", p.lane, p.bead, p.express))
}

/// `sentinel-call.preempt_log`: one line in `lane-preemptions`, as the binary wrote it.
pub fn preempt_log(e: &Effects, input: &str) -> Result<String, String> {
    use std::io::Write;
    let p: Preempt = serde_json::from_str(input).map_err(|e| format!("{PREEMPT_LOG}: {e}"))?;
    let path = e.world.run_dir.join("lane-preemptions");
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    writeln!(f, "{}\t{}\t{}\t{}", p.at_ms / 1000, p.lane, p.bead, p.express).map_err(|err| err.to_string())?;
    Ok(path.display().to_string())
}

/// `sentinel-call.metric`: the `call` row of the metrics store, in-process.
pub fn metric(e: &Effects, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{METRIC}: {e}"))?;
    let fields: Vec<(String, serde_json::Value)> = ["rune", "result"].iter().map(|k| (k.to_string(), v[*k].clone())).collect();
    tsd::store::Pusher::new(e.world.metrics_endpoint.clone()).push("call", &tsd::now_iso(), &fields).map_err(|e| e.to_string())?;
    Ok("call".into())
}
