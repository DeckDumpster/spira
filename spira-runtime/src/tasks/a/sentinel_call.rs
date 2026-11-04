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
//! ## Reverse
//!
//! Release rollback restores the timer and ExecStopPost; or set the task to `Observe` — every
//! `unit_start` is journaled `effect.shadowed` and the old timer may be re-enabled by hand.
//!

use std::collections::BTreeMap;

use runtime::calendar::Calendar;
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "sentinel-call";
pub const STREAM: &str = "task.sentinel-call";
pub const TICK: EventKindTag = "sentinel-call.tick";
pub const STATE: EventKindTag = "sentinel-call.state";
/// Emitted by [`super::sentinel_pass`] at CHECK 7 and when a dwarf unit ends; wakes the call.
pub const CALL_WANTED: EventKindTag = "call.wanted";
pub const OBSERVE: &str = "sentinel-call.observe";
pub const UNIT_START: &str = "sentinel-call.unit_start";
pub const PREEMPT: &str = "sentinel-call.preempt";
pub const PREEMPT_LOG: &str = "sentinel-call.preempt_log";
pub const METRIC: &str = "sentinel-call.metric";
/// The prefix every dwarf unit this task starts carries.
pub const DWARF_PREFIX: &str = "spira-dwarf-";
pub const CALENDAR: &str = "*-*-* *:*:00";
pub const OBSERVE_DEADLINE_MS: u64 = 5_000;

/// The Act table: each effect and its deadline.
pub const ACTS: &[(&str, u64)] = &[(UNIT_START, 10_000), (PREEMPT, 30_000), (PREEMPT_LOG, 1_000), (METRIC, 1_000)];
/// Hand-offs decide returns as acts that are emitted, not run.
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, UNIT_START, PREEMPT, PREEMPT_LOG, METRIC];
/// The pinned keys registered in `spira/conf.d`.
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_MAX_DWARVES", "SPIRA_MAX_LIVE_DWARVES", "SPIRA_LANES_MAX_LIVE", "SPIRA_LANES", "SPIRA_QUEUE_THROTTLE_OVERRIDE"];
/// Keys the contract pins that `spira/conf.d` does not register yet: the port registers them
/// before they join `KEYS` (`SPIRA_CALL_LOCK_WAIT` is retired and not listed).
pub const UNREGISTERED: &[&str] = &["SPIRA_CALL", "SPIRA_SYSTEMCTL", "SPIRA_THROTTLE_STAMP", "SPIRA_CAPACITY_PAUSE", "SPIRA_SENTINEL_PASS_BUDGET_SECS"];

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

/// The observe snapshot. A ready count that could not be read is `None`, never 0 (sp-hh599).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub world: World,
    /// The release skew refusal, when there is one.
    pub skew: Option<String>,
    pub capacity: Capacity,
    pub live_by_rune: BTreeMap<String, u32>,
    pub ready_by_rune: BTreeMap<String, Option<u32>>,
    pub urgent: Vec<String>,
    pub express_ready: Option<u32>,
    pub throttle: bool,
    pub lane_dwarves: Vec<LiveUnit>,
    /// The fleet ceiling `SPIRA_MAX_LIVE_DWARVES` and lane spill `SPIRA_LANES_MAX_LIVE`, as pinned.
    pub max_live: Option<u32>,
    pub lanes_max_live: Option<u32>,
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
pub struct Carry {
    pub state: State,
    pub lane_last: Option<String>,
    pub in_flight: BTreeMap<String, String>,
    pub refusal: Option<String>,
}

/// The next state, or a refusal naming the state the event is illegal in.
pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

pub fn step_leg(_leg: Leg, _event: LegTrigger) -> Result<Leg, String> {
    crate::not_ported!(NAME)
}

/// The calls (`unit_start` per rune) and at most one preemption, from the snapshot alone.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
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

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sentinel-call/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for SentinelCall {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
