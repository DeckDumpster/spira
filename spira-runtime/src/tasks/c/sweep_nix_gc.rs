//! # sweep.nix_gc — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-nix-gc.md`;
//! its Acceptance section is the test file `tests/task_c_sweep_nix_gc.rs`.
//!
//! ## Tick
//!
//! - **Calendar:** `OnCalendar=weekly` (= `Mon *-*-* 00:00:00`), `RandomizedDelaySec=1h`,
//!   `AccuracySec=1h`, `Persistent=true`. Jitter from `Ctx::rand_u64`, deterministic by seed.
//! - **S8 gap:** the sim parser rejects the single-token `weekly` (`sim/src/units.rs:77-96`
//!   accepts only `[DOW] *-*-* HH:MM[:SS]`). Either the contract states the expanded form
//!   `Mon *-*-* 00:00:00` or S8 adds the shorthand. Proposed: expand in the task's registration.
//! - **Catch-up:** an overdue tick fires **once**; `last_fired` carried in `sweep.nix_gc.state`.
//! - **Single-flight:** yes. No `world guard` today; kept.
//!
//! ## Observe
//!
//! `sweep.nix_gc.observe` (probe, host, deadline 5 s) returns:
//!
//! ```text
//! Snapshot { env_ok: bool, prev: Absent|Ok|Dangling, gc_bin: Option<path> }
//! ```
//!
//! - `env_ok`: `$SPIRA_NIX_ENV_LINK/bin` is a directory (`nix-gc.sh:9-13`).
//! - `prev`: `$SPIRA_NIX_ENV_LINK.prev` is a symlink whose `bin` exists (`:14-17`).
//! - `gc_bin`: first executable `nix-collect-garbage` in `$SPIRA_NIX_PROFILE_BIN` then
//!   `$env_link/bin` (`:18-25`).
//! - These are the install's GC roots (`install` writes them, per the header `:2-5`); the
//!   filesystem is the owning component. No store verbs.
//!
//! ## Decide
//!
//! `decide(snap) -> Collect(gc_bin) | Refuse(reason)`: refuse when `!env_ok` (a collect would drop
//! the live env), when `prev == Dangling`, or when no binary (`:10-25`). Pure.
//! **Fixtures:** the three refusal cases and the happy path; `install/src/bin/install.rs` is named
//! in the script's `covers:` line (`:5`) — its existing tests are the parity source (unverified
//! that they exercise the refusals).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.nix_gc.collect` | unit | 1800 s (unit `TimeoutStartSec`) | `nix-gc/<ISO week>` | anything `decide` refused (re-checked in the runner: the link may move between probe and act during an install) |
//!
//! Runs `nix-collect-garbage` (an allow-listed child, W1). Class **unit**, not host: the job is
//! shaped `CPUQuota=20%`, `Nice=10`, `IOSchedulingClass=idle` today, and the only way to keep
//! `CPUQuota` is a transient unit (S6 `unit.start` with those properties), finished by `unit.ran`.
//! A host-class child keeps nice/ioprio but loses the quota. Garbage collection is naturally
//! idempotent; a second run in the same week collects nothing.
//!
//! A refusal is a fault worth one inbox line (`inbox.append`, as the hosted handlers do), since
//! today it exits 1 into `run/nix-gc.log` where nobody looks.
//!
//! ## Silence
//!
//! The tick is weekly; the effect is always taken when nothing refuses, so "idle" here means
//! between ticks: nothing on the 167 hours with no tick. On a refusal, at most one inbox line per week.
//!
//! ## Replaces
//!
//! - Unit `spira-nix-gc-prod.service` / `.timer`; output `run/nix-gc.log`.
//! - Children: `nix-collect-garbage` (stays).
//! - Config keys: `SPIRA_NIX_ENV_LINK` (default `$HOME/.local/lib/spira-env`),
//!   `SPIRA_NIX_PROFILE_BIN` (default `/nix/var/nix/profiles/default/bin`) — env-only today
//!   (`nix-gc.sh:9,18`), not in `Settings::KEYS`; S10 adds them.
//!
//! ## Reverse
//!
//! Release rollback restores unit and timer. `Observe` shadows `collect`; the store grows by one
//! week's garbage.
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};
pub use crate::watchers::INBOX_APPEND;

pub const NAME: &str = "sweep.nix_gc";
pub const STREAM: &str = "task.sweep.nix_gc";
pub const TICK: EventKindTag = "sweep.nix_gc.tick";
pub const STATE: EventKindTag = "sweep.nix_gc.state";

pub const OBSERVE: &str = "sweep.nix_gc.observe";
/// Starts `nix-collect-garbage` as a transient unit; finished by its `unit.ran`.
pub const COLLECT: &str = "sweep.nix_gc.collect";
pub const EFFECTS: &[&str] = &[OBSERVE, COLLECT, INBOX_APPEND];

/// `OnCalendar=weekly`.
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_NIX_GC_CALENDAR";
/// `RandomizedDelaySec=1h`.
pub const JITTER_KEY: &str = "SPIRA_RUNTIME_SWEEP_NIX_GC_JITTER_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, JITTER_KEY, "SPIRA_NIX_ENV_LINK", "SPIRA_NIX_PROFILE_BIN"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 5_000, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Prev {
    #[default]
    Absent,
    Ok,
    Dangling,
}

/// The install's GC roots; the filesystem owns them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub env_ok: bool,
    pub prev: Prev,
    pub gc_bin: Option<String>,
}

/// T3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Collecting,
    Refused,
}

/// `state × event → state`. `unit.ran` while `idle` is refused naming `idle`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "observed:ok", Phase::Collecting), (Phase::Idle, "observed:refuse", Phase::Refused), (Phase::Collecting, "unit.ran", Phase::Idle), (Phase::Refused, "tick", Phase::Idle)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub last_fired: Option<u64>,
    pub refused: Option<String>,
}

pub struct SweepNixGc {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// What [`COLLECT`] carries: the binary the probe found and the ISO week that keys the unit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collect {
    pub gc_bin: String,
    pub week: String,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Collecting => "collecting",
        Phase::Refused => "refused",
    }
}

/// The ISO-8601 week (`YYYY-Www`) holding `ms` since the epoch.
pub fn iso_week(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let weekday = (days + 3).rem_euclid(7); // Monday is 0; 1970-01-01 was a Thursday
    let thursday = days - weekday + 3;
    let year_of = |d: i64| crate::tasks::c::sweep_archive::archive::fmt_ts(d * 86_400)[..4].parse::<i64>().unwrap_or(1970);
    let year = year_of(thursday);
    let jan1 = crate::tasks::c::sweep_archive::archive::parse_ts(&format!("{year:04}-01-01")).unwrap_or(0) / 86_400;
    format!("{year:04}-W{:02}", (thursday - jan1) / 7 + 1)
}

/// How long a collect may stay in flight before a tick treats its completion as lost.
pub const STALE_MS: u64 = 2 * 3_600_000;

/// Why `snap` must not be collected (nix-gc.sh's three refusals), or `None`.
pub fn refusal(snap: &Snapshot) -> Option<&'static str> {
    if !snap.env_ok {
        Some("the spira-env link does not resolve to a spira-env (env) — refusing to collect")
    } else if snap.prev == Prev::Dangling {
        Some("the previous spira-env link is dangling (prev) — refusing to collect")
    } else if snap.gc_bin.is_none() {
        Some("nix-collect-garbage was not found (binary) in the profile or the spira-env")
    } else {
        None
    }
}

/// `Collect(gc_bin)`, or a refusal worth one inbox line.
pub fn decide(_cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        // Single-flight: a tick while the last collect's unit runs does nothing. A unit whose
        // completion never arrived (its `TimeoutStartSec` is 1800 s) does not wedge the task: a
        // tick past `STALE_MS` runs the pass again.
        Input::Observed(_) if carry.phase == Phase::Collecting && carry.last_fired.is_some_and(|t| now_ms.saturating_sub(t) < STALE_MS) => Ok((carry.clone(), vec![])),
        Input::Observed(snap) => match (refusal(snap), &snap.gc_bin) {
            (None, Some(bin)) => {
                let body = Collect { gc_bin: bin.clone(), week: iso_week(now_ms) };
                let act = Act { name: COLLECT.into(), input: serde_json::to_string(&body).map_err(|e| e.to_string())?, deadline_ms: 10_000, probe: false };
                Ok((Carry { phase: Phase::Collecting, last_fired: Some(now_ms), refused: None }, vec![act]))
            }
            (why, _) => {
                let why = why.unwrap_or("nix-collect-garbage was not found (binary)");
                let line = format!("{} [task:{NAME}] {why}", crate::watchers::iso8601(now_ms / 1000));
                Ok((Carry { phase: Phase::Refused, last_fired: Some(now_ms), refused: Some(why.into()) }, vec![Act { name: INBOX_APPEND.into(), input: line, deadline_ms: 10_000, probe: false }]))
            }
        },
        Input::Ran(_) if carry.phase == Phase::Collecting => Ok((Carry { phase: Phase::Idle, ..carry.clone() }, vec![])),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {:?}", phase_name(carry.phase), r.handle)),
        // The unit did not start: nothing is collecting, so the next tick tries again.
        Input::Outcome { effect, ok: false, body } if effect == COLLECT && carry.phase == Phase::Collecting => {
            let line = format!("{} [task:{NAME}] nix-collect-garbage did not start: {body}", crate::watchers::iso8601(now_ms / 1000));
            Ok((Carry { phase: Phase::Idle, ..carry.clone() }, vec![Act { name: INBOX_APPEND.into(), input: line, deadline_ms: 10_000, probe: false }]))
        }
        Input::Outcome { effect, .. } if effect == COLLECT || effect == INBOX_APPEND => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} no outcome of {effect} is expected", phase_name(carry.phase))),
    }
}

/// The runner of [`OBSERVE`]: nix-gc.sh's checks on the install's GC roots.
pub fn observe(env_link: &str, profile_bin: &str) -> Snapshot {
    use std::os::unix::fs::PermissionsExt;
    let env = std::path::Path::new(env_link);
    let prev = std::path::PathBuf::from(format!("{env_link}.prev"));
    let prev = match std::fs::symlink_metadata(&prev) {
        Ok(m) if m.file_type().is_symlink() => {
            if prev.join("bin").is_dir() {
                Prev::Ok
            } else {
                Prev::Dangling
            }
        }
        _ => Prev::Absent,
    };
    let executable = |p: &std::path::Path| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    let gc_bin = [std::path::PathBuf::from(profile_bin), env.join("bin")].into_iter().map(|d| d.join("nix-collect-garbage")).find(|p| executable(p)).map(|p| p.display().to_string());
    Snapshot { env_ok: env.join("bin").is_dir(), prev, gc_bin }
}

/// The runner of [`COLLECT`]: the refusals re-checked (an install may have moved the link since
/// the probe), then `nix-collect-garbage` started as this task's unit, keyed by the ISO week.
pub fn collect(units: Option<&dyn runtime::units::Units>, env_link: &str, profile_bin: &str, input: &str) -> Result<String, String> {
    let job: Collect = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let now = observe(env_link, profile_bin);
    if let Some(why) = refusal(&now) {
        return Err(why.into());
    }
    if now.gc_bin.as_deref() != Some(job.gc_bin.as_str()) {
        return Err(format!("nix-collect-garbage moved since the probe ({} is now {:?}) — refusing", job.gc_bin, now.gc_bin));
    }
    let units = units.ok_or("no unit manager configured")?;
    let spec = runtime::units::UnitSpec { task: NAME.into(), key: job.week, argv: vec![job.gc_bin], env: vec![], cwd: None };
    runtime::units::effect(units, runtime::units::UNIT_START, &serde_json::to_string(&spec).map_err(|e| e.to_string())?)
}

crate::tasks::b::hosted!(SweepNixGc);
