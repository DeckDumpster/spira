//! # sweep.nix_gc — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-nix-gc.md`;
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

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};
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

/// `Collect(gc_bin)`, or a refusal worth one inbox line.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("sweep.nix_gc: not ported yet")
}

crate::tasks::b::hosted!(SweepNixGc);
