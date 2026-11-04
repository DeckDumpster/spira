//! # sweep.tsd — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-tsd.md`;
//! its Acceptance section is the test file `tests/task_c_sweep_tsd.rs`.
//!
//! ## Tick
//!
//! - **Calendar:** `OnCalendar=*-*-* 04:20:00`, `AccuracySec=5min`, `Persistent=true`, no
//!   `RandomizedDelaySec` (unit file). Under S8, `next_after(now)` on the `runtime::calendar`
//!   parser; the sim parser already reads this spec (`sim/src/units.rs:77-109`).
//! - **Catch-up:** an overdue tick fires **once** (systemd `Persistent=true` semantics today).
//!   `last_fired` is carried in an emitted `sweep.tsd.state` event, since `Command::Timer` is
//!   memory-only (`runtime/src/event.rs:290-300`); at boot, `next_after(last_fired) <= now` fires
//!   one tick immediately.
//! - **Wakes:** none. **Single-flight:** a tick with a prune in flight re-arms and does nothing.
//! - **Guard:** none. The unit has no `ExecStartPre=world guard`; retention runs while the world
//!   is halted. Kept.
//!
//! ## Observe
//!
//! `sweep.tsd.observe` (probe, deadline 10 s) returns:
//!
//! ```text
//! Snapshot { families: Vec<(family, bytes)>, default_days: u32, overrides: BTreeMap<String,u32> }
//! ```
//!
//! - `families`: every `<family>.jsonl` under `$SPIRA_RUN/tsd/` that passes `tsd::valid_family`
//!   (`spira-config/src/tsd_prune_main.rs:53-65`, `tsd/src/prune.rs:146-150`). The tsd directory
//!   **is** the store for this data; the reading component is `tsd::prune`, so this is not a
//!   re-derivation.
//! - `default_days` / `overrides`: pinned config, not read per tick (S10).
//! - No lifecycle-store reads. The unit forks nothing today.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> Vec<(family, keep_days)>`: for each family,
//! `tsd::prune::keep_days(family, default, overrides)` (`tsd/src/prune.rs:57-59`), which floors the
//! bound at `READER_WINDOW_DAYS` (`tsd/src/prune.rs:20-29`; unknown family floors at 1 day,
//! `:37-39`). Pure already. Carry: `{ last_fired, in_flight }`.
//! **Fixtures:** the existing `keep_days`/`parse_overrides` tests in `tsd/src/prune.rs`, plus a
//! snapshot with a family below its floor and a malformed override (refused at config load,
//! `tsd/src/prune.rs:43-54`).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.tsd.prune` (one per family) | host | 300 s per family (unit total today 1800 s) | `sweep.tsd/<family>/<cutoff-epoch>` | a family file replaced while pruning (`tsd/src/prune.rs:118-120`, "left for the next pass"); an empty `SPIRA_RUN` (`tsd_prune_main.rs:27-29`) |
//!
//! The runner is `tsd::prune::prune_file(path, cutoff)` (`tsd/src/prune.rs:76-135`): stream into
//! `<file>.prune.tmp`, take the appenders' `flock`, copy the tail, rename. Rewrites nothing when no
//! row is old enough (`:109-111`). A row whose `ts` is unreadable is kept (`:100`).
//!
//! **Resource shaping.** The unit runs at `CPUQuota=20%`, `Nice=10`, `IOSchedulingClass=idle`.
//! In-process, the effector thread sets its own nice and `ioprio_set(IOPRIO_CLASS_IDLE)` per TID
//! before the loop; `CPUQuota` cannot be kept per thread (unverified that the host pool permits
//! per-thread priority).
//!
//! ## Silence
//!
//! An idle tick (no family with rows past its cutoff) emits the probe and N `prune` effects whose
//! outcome is `kept k, dropped 0`. To stay silent, the per-family prune is **not** journaled as an
//! effect when `dropped == 0`: the observe returns each family's oldest `ts`, and `decide` emits a
//! `prune` only for a family whose oldest row is older than its cutoff. Idle = nothing journaled.
//! This needs `Snapshot.families` to carry `oldest_ts` (read the first line; files are append-ordered
//! — unverified that every family is strictly time-ordered).
//!
//! ## Replaces
//!
//! - Unit `spira-tsd-prune-prod.service` / `.timer`; output `run/tsd-prune.log`
//!   (`StandardOutput=append:`).
//! - Children: none.
//! - Config keys pinned: `SPIRA_RUN`, `SPIRA_TSD_KEEP_DAYS` (default 30,
//!   `spira/conf.d.defaults.generated.sh:1170`), `SPIRA_TSD_KEEP_DAYS_BY_FAMILY` (default empty,
//!   `:1358`); read at `tsd_prune_main.rs:26-32`, listed in `spira-config/src/resolve.rs:489-490`.
//! - `tsd-prune --dry-run` stays as an operator CLI over the same library.
//!
//! ## Reverse
//!
//! Release rollback restores the unit and timer. Per task: `Observe` mode (S4) journals each
//! `sweep.tsd.prune` as `effect.shadowed` and deletes nothing; a missed day costs one day of extra
//! retention.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.tsd";
pub const STREAM: &str = "task.sweep.tsd";
pub const TICK: EventKindTag = "sweep.tsd.tick";
pub const STATE: EventKindTag = "sweep.tsd.state";

pub const OBSERVE: &str = "sweep.tsd.observe";
pub const PRUNE: &str = "sweep.tsd.prune";
pub const EFFECTS: &[&str] = &[OBSERVE, PRUNE];

/// `OnCalendar=*-*-* 04:20:00`, no jitter.
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_TSD_CALENDAR";
pub const KEEP_DAYS_KEY: &str = "SPIRA_TSD_KEEP_DAYS";
pub const BY_FAMILY_KEY: &str = "SPIRA_TSD_KEEP_DAYS_BY_FAMILY";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_RUN", KEEP_DAYS_KEY, BY_FAMILY_KEY];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Family {
    pub family: String,
    pub bytes: u64,
    /// The first row's `ts` (epoch ms): files are append-ordered.
    pub oldest_ts: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub families: Vec<Family>,
    pub default_days: u32,
    pub overrides: BTreeMap<String, u32>,
}

/// T3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Pruning,
}

/// `state × event → state`. A `prune` outcome while `idle` is refused naming `idle`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "observed:old", Phase::Pruning), (Phase::Idle, "observed:idle", Phase::Idle), (Phase::Pruning, "prune:outcome", Phase::Pruning), (Phase::Pruning, "done", Phase::Idle)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub last_fired: Option<u64>,
    /// Families with a prune in flight.
    pub in_flight: Vec<String>,
}

pub struct SweepTsd {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `tsd::prune::keep_days` per family; a prune only for a family whose oldest row is past it.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("sweep.tsd: not ported yet")
}

crate::tasks::b::hosted!(SweepTsd);
