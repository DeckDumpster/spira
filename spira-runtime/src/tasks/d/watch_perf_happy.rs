//! # watch.perf_happy — contract
//!
//! Replaces `spira-perf-happy-path-prod` (`perf-watch happy --repo harness --sim bin/sim --scratch run/perf-happy-path --history run/perf-happy-path.history`). The catalogue says P1. The code needs P2, because the sim run is a long leg.
//!
//! Catalogue differs: one pass is up to 3 × 280 s sim runs with a 20 s backoff (`perf-watch/src/happy.rs:49-71`, `main.rs:204,248`; unit `TimeoutStartSec=420`). That is a long leg, so it must be a `unit.start` started over D-Bus (P2), not an inline effect.
//!
//! ## Tick
//!
//! - Hourly (`OnUnitActiveSec=1h`, `OnBootSec=20min`); single-flight.
//! - A tick that finds a run still in flight does nothing.
//!
//! ## Observe
//!
//! `watch.perf_happy.observe` returns `{ round: open|none|unknown(err), history: [ms…last 5] }`.
//!
//! - Round state today is `timeout 5 queue round status` → `round=open|none` (`main.rs:190-201`). In the task it is read from the round/batch projection (S7b), the component that decides, not from the CLI.
//! - History is today's `--history` file, `<epoch>\t<ms>` lines (`happy.rs:73-80`). It becomes carried state.
//!
//! ## Decide
//!
//! The state machine is `idle → running(attempt n) → idle`.
//!
//! - On tick + `round=open`: stay idle, emit nothing (`happy.rs:52`).
//! - On tick + `round=unknown`: alarm `PERF UNRUNNABLE happy-path: round state unknown, run withheld: <e>` (`happy.rs:53,86`).
//! - On tick + `round=none`: `unit.start` the sim run.
//! - On `unit.ran` failed with `testdb up` in the stderr tail (`happy.rs:29-31`) and attempt < 3: wait 20 s, then start again (`happy.rs:62-65`, `main.rs:204`).
//! - On attempt 3: `PERF FIXTURE-DOWN` (`happy.rs:66,87`).
//! - On any other failure: `PERF UNRUNNABLE happy-path: <e>`.
//! - On success: measured ms plus `slowest_phase(exec.log)` (`happy.rs:36-47`). Append it to history, and alarm when ms > goal (60 000 default, `main.rs:248`) per `alarm` (`happy.rs:83-95`).
//! - Carry: `{ state, attempt, history[≤5], run_unit }`, emitted as `watch.perf_happy.state`.
//! - Pure parts reused: `measure`'s branching (split at the run), `alarm`, `trend`, `slowest_phase`, `is_fixture_failure`.
//! - Fixtures: `happy.rs:251-335` (slowest phase, over goal, at goal silent, open round withholds, unknown round alarms, failed run alarms, fixture retried then reported, fixture starts on retry, trend skips junk).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.perf_happy.observe` (probe) | store | 5 s | — | — |
//! | `watch.perf_happy.checkout` | git | 120 s | `watch.perf_happy:checkout:<tick>` | a repo path outside the registry |
//! | `unit.start` (shared) | unit | 280 s run limit (`main.rs:248`) | `watch.perf_happy:run:<tick>:<attempt>` | a second start while `running` |
//! | `watch.perf_happy.teardown` | container | 60 s | `watch.perf_happy:down:<run dir>` | a dir not matching `sim-run-*-<seed>` |
//! | `watch.perf_happy.log` | host | 5 s | `watch.perf_happy:<tick>` | a path outside `SPIRA_RUN` |
//! | `watch.perf_happy.history` | host | 5 s | `watch.perf_happy:hist:<epoch>` | — |
//!
//! - **`watch.perf_happy.checkout`** stages the release tree as a git repo in scratch: `git init/add -A -f/commit` plus `cp -a` (`happy.rs:133-190`).
//! - **`unit.start`** runs `sim run happy-path --seed 7 --keep` with production locators stripped (`main.rs:164,176-186,213`). It completes with `unit.ran {rc, exec.log tail, stderr tail}`.
//! - **`watch.perf_happy.teardown`** runs `sim world down <dir>` (`main.rs:233`).
//! - **`watch.perf_happy.log`** appends the bare alarm line to `run/perf-watch.log`, as the unit's stdout does today (`main.rs:273-275`, unit `StandardOutput=append:…/perf-watch.log`).
//! - **`watch.perf_happy.history`** appends `<epoch>\t<ms>` to the history file (`main.rs:267-271`), for parity readers. Otherwise carry holds it.
//!
//! Today this unit writes **no inbox line**. Its alarm reaches the Concierge only through the `perf-watch` log row (`spira/watchers:122`) and `watchd notify`. Adding an `inbox.append` would be a behaviour change and is left out.
//!
//! ## Silence
//!
//! An idle tick (round open, or a run already in flight) emits the probe only. A measured run under goal emits the history append and a state Emit, with no log line.
//!
//! ## Replaces
//!
//! - `spira-perf-happy-path-prod.service` and `spira-perf-happy-path-prod.timer` (`CPUQuota=50%`).
//! - Children:
//!   - `timeout 5 queue round status` (`main.rs:191`)
//!   - `git` ×4 and `cp -a` (`happy.rs:135,178`)
//!   - `sim run` and `sim world down` (`main.rs:213,233`)
//! - Keys pinned: `--goal-ms`, `--run-limit-s`, `--seed`, `--scenario` (`main.rs:248`), and `SPIRA_RELEASE` to `SPIRA_SIM_RELEASE` (`main.rs:182-184`).
//!
//! Catalogue differs: the children are `queue round status`, `sim`, `git`, `cp` and `timeout`. The catalogue's "sim, git, cp" omits `queue`.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Observe mode shadows `unit.start` and the appends.

use runtime::event::*;
use runtime::units::{UnitRan, UNIT_START};
use serde::{Deserialize, Serialize};
use spira_config::process::cfg;

use super::pattern;
use crate::watchers::carried;

pub const NAME: &str = "watch.perf_happy";
pub const STREAM: &str = "watch.perf_happy";
pub const TICK: EventKindTag = "watch.perf_happy.tick";
pub const STATE: EventKindTag = "watch.perf_happy.state";
/// A move the transition table refuses, its body naming the state it was refused in.
pub const REFUSED: EventKindTag = "watch.perf_happy.refused";
pub const OBSERVE: &str = "watch.perf_happy.observe";
pub const CHECKOUT: &str = "watch.perf_happy.checkout";
pub const TEARDOWN: &str = "watch.perf_happy.teardown";
pub const LOG: &str = "watch.perf_happy.log";
pub const HISTORY: &str = "watch.perf_happy.history";
pub const EFFECTS: &[&str] = &[OBSERVE, CHECKOUT, UNIT_START, TEARDOWN, LOG, HISTORY];
pub const KEYS: &[&str] = &["SPIRA_RUN"];
/// `OnUnitActiveSec=1h`.
pub const CADENCE_MS: u64 = 3_600_000;
const OBSERVE_DEADLINE_MS: u64 = 5_000;
/// The unit's argv (`perf-watch/src/main.rs:248`); no config key carries these today.
pub const GOAL_MS: u64 = 60_000;
pub const RUN_LIMIT_S: u64 = 280;
pub const SEED: u64 = 7;
pub const SCENARIO: &str = "happy-path";
/// Fixture-down retries and the wait between them (`happy.rs:62-66`, `main.rs:204`).
pub const ATTEMPTS: u32 = 3;
pub const RETRY_WAIT_MS: u64 = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub run: String,
}

impl Config {
    pub fn pinned() -> Result<Config, String> {
        Ok(Config { run: cfg("SPIRA_RUN")? })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Round {
    Open,
    #[default]
    None,
    Unknown(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub round: Round,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    Running,
}

/// `{ state, attempt, history[≤5], run_unit }`; history is `(epoch, ms)` per measured run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub phase: Phase,
    pub attempt: u32,
    pub history: Vec<(u64, u64)>,
    pub run_unit: Option<String>,
}

/// What moves the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Observed(Snapshot),
    Ran(UnitRan),
}

/// Every legal move, as data: `idle → running(attempt n) → idle`.
pub static TRANSITIONS: pattern::Table<Phase> = &[
    (Phase::Idle, "tick round=open", Phase::Idle),
    (Phase::Idle, "tick round=unknown", Phase::Idle),
    (Phase::Idle, "tick round=none", Phase::Running),
    (Phase::Running, "tick", Phase::Running),
    (Phase::Running, "unit.ran fixture-retry", Phase::Running),
    (Phase::Running, "unit.ran fixture-down", Phase::Idle),
    (Phase::Running, "unit.ran failed", Phase::Idle),
    (Phase::Running, "unit.ran measured", Phase::Idle),
];

/// One move of the machine: the next carry and its effects, or a refusal naming the state.
pub fn decide(_carry: &Carry, _input: &Input, _now_ms: u64, _cfg: &Config) -> Result<(Carry, Vec<EffectSpec>), String> {
    unimplemented!("watch.perf_happy: not ported yet")
}

pub struct WatchPerfHappy {
    pub config: Result<Config, String>,
}

impl WatchPerfHappy {
    pub fn new(config: Config) -> Self {
        WatchPerfHappy { config: Ok(config) }
    }

    pub fn pinned() -> Self {
        WatchPerfHappy { config: Config::pinned() }
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.perf_happy/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchPerfHappy {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, runtime::units::UNIT_RAN]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        let input = match (pattern::answer::<Snapshot>(view, ev, OBSERVE), pattern::unit_ran(ev, NAME)) {
            (Some(snap), _) => Input::Observed(snap),
            (None, Some((_, ran))) => Input::Ran(ran),
            _ => return vec![],
        };
        let config = match &self.config {
            Ok(c) => c,
            Err(e) => {
                ctx.event().set("config.refused", e.as_str());
                return vec![];
            }
        };
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        match decide(&carry, &input, ev.at, config) {
            Ok((next, acts)) => pattern::commands(STATE, &carry, &next, acts),
            Err(why) => vec![Command::Emit(EventKind::new(REFUSED, &why))],
        }
    }
}
