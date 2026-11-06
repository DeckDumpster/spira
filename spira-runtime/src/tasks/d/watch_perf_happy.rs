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
use runtime::units::{Exit, RanOutput, UnitRan, UnitSpec, UNIT_START};
use perf_watch::happy::{self, Outcome, Step};
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
/// The checkout's source is the home repo from the pinned repo map (`World::home_path`), not
/// `SPIRA_REPO`: that is a per-copy fact the config door never resolves, so declaring it here
/// would refuse the service's settings whole.
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_RELEASE"];
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
    /// The checkout's answer: where the sim runs and which sim binary runs.
    CheckedOut(Checkout),
    /// The checkout failed, with why.
    CheckoutFailed(String),
    Ran(UnitRan, RanOutput),
}

/// What `watch.perf_happy.checkout` answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkout {
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub sim: String,
}

/// Every legal move, as data: `idle → running(attempt n) → idle`.
pub static TRANSITIONS: pattern::Table<Phase> = &[
    (Phase::Idle, "tick round=open", Phase::Idle),
    (Phase::Idle, "tick round=unknown", Phase::Idle),
    (Phase::Idle, "tick round=none", Phase::Running),
    (Phase::Running, "tick", Phase::Running),
    (Phase::Running, "checkout", Phase::Running),
    (Phase::Running, "checkout failed", Phase::Idle),
    (Phase::Running, "unit.ran fixture-retry", Phase::Running),
    (Phase::Running, "unit.ran fixture-down", Phase::Idle),
    (Phase::Running, "unit.ran failed", Phase::Idle),
    (Phase::Running, "unit.ran measured", Phase::Idle),
];

const WRITE_DEADLINE_MS: u64 = 5_000;
const CHECKOUT_DEADLINE_MS: u64 = 120_000;
const TEARDOWN_DEADLINE_MS: u64 = 60_000;
/// The production locators a sim world must not see (`perf-watch/src/main.rs:164`).
const PRODUCTION_LOCATORS: [&str; 5] = ["SPIRA_LC_PASSWORD_FILE", "SPIRA_LC_SOCKET", "SPIRA_RUN", "SPIRA_DB", "SPIRA_TOML"];
/// Where the run prints its world's `exec.log`, after the sim's own output.
const EXEC_LOG_MARK: &str = "=== perf-happy exec.log";

fn spec(name: &str, input: String, deadline_ms: u64) -> EffectSpec {
    EffectSpec { name: name.into(), input, deadline_ms }
}

/// The scratch directory the runs keep their worlds in.
pub fn scratch(cfg: &Config) -> String {
    format!("{}/perf-happy-path", cfg.run)
}

/// The unit for run `attempt`: `sim run` bounded by the run limit, after the back-off when it is
/// a retry, then the kept world's `exec.log` on stdout for the slowest phase.
fn run_unit(cfg: &Config, at: &Checkout, key: &str, attempt: u32) -> UnitSpec {
    let scratch = scratch(cfg);
    let wait = if attempt > 1 { format!("sleep {}; ", RETRY_WAIT_MS / 1000) } else { String::new() };
    let script = format!(
        "{wait}unset {locators}; mkdir -p \"$TMPDIR\"; timeout {RUN_LIMIT_S} \"$0\" run {SCENARIO} --seed {SEED} --keep; rc=$?; echo '{EXEC_LOG_MARK}'; cat \"$TMPDIR\"/sim-run-*-{SEED}/exec.log 2>/dev/null; exit $rc",
        locators = PRODUCTION_LOCATORS.join(" ")
    );
    let env = vec![("TMPDIR".to_string(), scratch)];
    UnitSpec { task: NAME.into(), key: key.into(), argv: vec!["sh".into(), "-c".into(), script, at.sim.clone()], env, cwd: (!at.cwd.is_empty()).then(|| at.cwd.clone().into()) }
}

/// The run's result as `happy::judge` reads it: the exec.log on success, else why it failed.
fn result_of(ran: &UnitRan, out: &RanOutput) -> Result<String, String> {
    let stdout = out.stdout_tail.clone().unwrap_or_default();
    match ran.exit {
        Exit::Code(0) => Ok(stdout.split_once(EXEC_LOG_MARK).map_or(String::new(), |(_, log)| log.to_string())),
        Exit::Code(124) => Err(format!("sim run did not finish in {RUN_LIMIT_S}s")),
        e => {
            let code = match e {
                Exit::Code(c) => format!("exit status: {c}"),
                Exit::Signal(s) => format!("signal: {s}"),
                Exit::Unknown => "an unknown status".into(),
            };
            Err(format!("sim run exited {code}: {}", happy::stderr_tail(out.stderr_tail.as_deref().unwrap_or_default())))
        }
    }
}

/// The bare alarm line for `outcome`, to `perf-watch.log`, as the unit's stdout carried it.
fn alarm(outcome: &Outcome, history: &[(u64, u64)]) -> Vec<EffectSpec> {
    let recent: Vec<u64> = history.iter().map(|(_, ms)| *ms).collect();
    happy::alarm(outcome, GOAL_MS, &recent).map(|l| spec(LOG, l, WRITE_DEADLINE_MS)).into_iter().collect()
}

/// One move of the machine: the next carry and its effects, or a refusal naming the state.
pub fn decide(carry: &Carry, input: &Input, now_ms: u64, cfg: &Config) -> Result<(Carry, Vec<EffectSpec>), String> {
    let mv = |on: &str| pattern::transition(TRANSITIONS, NAME, carry.phase, on);
    match input {
        Input::Observed(_) if carry.phase == Phase::Running => mv("tick").map(|_| (carry.clone(), vec![])),
        Input::Observed(snap) => {
            let round = match &snap.round {
                Round::Open => Ok(true),
                Round::None => Ok(false),
                Round::Unknown(e) => Err(e.clone()),
            };
            match happy::gate(round) {
                None => {
                    let phase = mv("tick round=none")?;
                    let next = Carry { phase, attempt: 1, run_unit: None, ..carry.clone() };
                    Ok((next, vec![spec(CHECKOUT, scratch(cfg), CHECKOUT_DEADLINE_MS)]))
                }
                Some(Outcome::Skipped) => mv("tick round=open").map(|_| (carry.clone(), vec![])),
                Some(o) => mv("tick round=unknown").map(|_| (carry.clone(), alarm(&o, &carry.history))),
            }
        }
        Input::CheckedOut(at) => {
            let phase = mv("checkout")?;
            let key = format!("run-{now_ms}-{}", carry.attempt);
            let unit = run_unit(cfg, at, &key, carry.attempt);
            let handle = runtime::units::unit_name(NAME, &key).map_err(|e| e.0)?;
            let next = Carry { phase, run_unit: Some(handle), ..carry.clone() };
            // The checkout's answer is kept for a retry, which starts the same tree again.
            Ok((next, vec![spec(UNIT_START, serde_json::to_string(&unit).unwrap_or_default(), RUN_LIMIT_S * 1000)]))
        }
        Input::CheckoutFailed(why) => {
            let phase = mv("checkout failed")?;
            Ok((Carry { phase, attempt: 0, run_unit: None, ..carry.clone() }, alarm(&Outcome::Failed(why.clone()), &carry.history)))
        }
        Input::Ran(ran, out) => {
            let attempt = carry.attempt.max(1);
            let step = happy::judge(result_of(ran, out), ran.duration_ms, attempt as usize);
            let teardown = spec(TEARDOWN, scratch(cfg), TEARDOWN_DEADLINE_MS);
            match step {
                Step::Retry => {
                    let phase = mv("unit.ran fixture-retry")?;
                    let next = Carry { phase, attempt: attempt + 1, run_unit: None, ..carry.clone() };
                    Ok((next, vec![teardown, spec(CHECKOUT, scratch(cfg), CHECKOUT_DEADLINE_MS)]))
                }
                Step::Done(o @ Outcome::FixtureDown(_)) => {
                    let phase = mv("unit.ran fixture-down")?;
                    Ok((Carry { phase, attempt: 0, run_unit: None, ..carry.clone() }, [vec![teardown], alarm(&o, &carry.history)].concat()))
                }
                Step::Done(o @ Outcome::Measured { millis, .. }) => {
                    let phase = mv("unit.ran measured")?;
                    let epoch = now_ms / 1000;
                    let mut history = carry.history.clone();
                    history.push((epoch, millis));
                    history = history.split_off(history.len().saturating_sub(5));
                    let line = happy::history_line(epoch, millis).trim_end().to_string();
                    let acts = [vec![teardown, spec(HISTORY, line, WRITE_DEADLINE_MS)], alarm(&o, &history)].concat();
                    Ok((Carry { phase, attempt: 0, history, run_unit: None }, acts))
                }
                Step::Done(o) => {
                    let phase = mv("unit.ran failed")?;
                    Ok((Carry { phase, attempt: 0, run_unit: None, ..carry.clone() }, [vec![teardown], alarm(&o, &carry.history)].concat()))
                }
            }
        }
    }
}

/// `watch.perf_happy.observe`: whether the home repo has a round open, from the store's round
/// view in-process (`queue round status`'s answer, from the component that decides it).
pub fn observe(e: &crate::Effects, _: &str, _: &EffectSpec) -> Result<String, String> {
    let open = spira_config::lc_call::LcClient::configured().and_then(store_view::Projection::configured).and_then(|views| {
        views.load().map_err(|e| format!("the store views: {e}"))?;
        happy::round_open_in(&views, &e.world.home_repo)
    });
    let round = match open {
        Ok(true) => Round::Open,
        Ok(false) => Round::None,
        Err(e) => Round::Unknown(e),
    };
    serde_json::to_string(&Snapshot { round }).map_err(|e| e.to_string())
}

/// `watch.perf_happy.checkout`: the release tree staged as a git checkout under the scratch
/// directory (`happy::checkout`), and the release's sim binary.
pub fn checkout(e: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    let (repo, release) = (e.world.home_path()?, cfg("SPIRA_RELEASE")?);
    std::fs::create_dir_all(&spec.input).map_err(|e| format!("{}: {e}", spec.input))?;
    let cwd = happy::checkout(&repo, std::path::Path::new(&spec.input))?;
    serde_json::to_string(&Checkout { cwd: cwd.display().to_string(), sim: format!("{release}/bin/sim") }).map_err(|e| e.to_string())
}

/// `watch.perf_happy.teardown`: each kept `sim-run-*-<seed>` world under the scratch directory is
/// brought down by `sim world down`, run as a unit of its own.
pub fn teardown(e: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    let units = e.units.as_deref().ok_or("no unit manager configured")?;
    let sim = format!("{}/bin/sim", cfg("SPIRA_RELEASE")?);
    let suffix = format!("-{SEED}");
    let mut downed = vec![];
    for d in std::fs::read_dir(&spec.input).map_err(|e| format!("{}: {e}", spec.input))?.flatten() {
        let n = d.file_name().to_string_lossy().into_owned();
        if n.starts_with("sim-run-") && n.ends_with(&suffix) {
            let unit = UnitSpec { task: NAME.into(), key: format!("{DOWN_PREFIX}{n}"), argv: vec![sim.clone(), "world".into(), "down".into(), d.path().display().to_string()], env: vec![], cwd: None };
            units.start(&unit).map_err(|e| e.0)?;
            downed.push(n);
        }
    }
    Ok(downed.join(" "))
}

/// The key prefix of a teardown unit, whose end the machine does not wait on.
pub const DOWN_PREFIX: &str = "down-";

fn append(name: &str, line: &str) -> Result<String, String> {
    let path = std::path::Path::new(&Config::pinned()?.run).join(name);
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

/// `watch.perf_happy.log`: the bare alarm to `run/perf-watch.log`.
pub fn log(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    append("perf-watch.log", &spec.input)
}

/// `watch.perf_happy.history`: `<epoch>\t<ms>` to `run/perf-happy-path.history`.
pub fn history(_: &crate::Effects, _: &str, spec: &EffectSpec) -> Result<String, String> {
    append("perf-happy-path.history", &spec.input)
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
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, runtime::units::UNIT_RAN]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return pattern::tick(ev, OBSERVE, String::new(), OBSERVE_DEADLINE_MS, ev.at + CADENCE_MS);
        }
        let input = if let Some(snap) = pattern::answer::<Snapshot>(view, ev, OBSERVE) {
            Input::Observed(snap)
        } else if let Some(at) = pattern::answer::<Checkout>(view, ev, CHECKOUT) {
            Input::CheckedOut(at)
        } else if ev.kind.tag == EFFECT_FAILED && crate::watchers::answers_observe(view, ev, CHECKOUT) {
            Input::CheckoutFailed(ev.kind.body.clone())
        } else if let Some((key, ran)) = pattern::unit_ran(ev, NAME) {
            if key.starts_with(DOWN_PREFIX) {
                return vec![];
            }
            Input::Ran(ran, RanOutput::from_event_kind(&ev.kind).unwrap_or_default())
        } else {
            return vec![];
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
