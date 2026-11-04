//! # sweep-target — contract
//!
//! Task id `sweep.target`, pattern P1, family A of [[scheduler-consolidation-2026-11-03]]. It runs
//! `target-reap --if-below-floor`. When free disk under the worktree root drops below the floor,
//! it reclaims the build output of idle worktrees, oldest first. Every tick it also removes
//! foreign-cache run directories that are stale and unreferenced.
//!
//! The binary is in the **testenv** crate: `testenv/Cargo.toml:28-29` names it
//! `src/target_reap_main.rs`. Paths are relative to `spira-releases/current`.
//!
//! **Code differs from the catalogue.** The catalogue lists the children as "— (df, rm)". The code
//! forks `df -Pk` (`testenv/src/reap.rs:272`), and also falls back to `podman unshare rm -rf` for
//! overlay dirs it cannot remove (`testenv/src/foreign.rs:246`). The foreign-cache sweep runs
//! **before** the floor check, on every tick (`target_reap_main.rs:77-84`).
//!
//! ## Tick
//!
//! - Cadence 2 min (`spira-target-reap-prod.timer`: OnBootSec 2 min, OnUnitActiveSec 2 min).
//!   Single-flight.
//! - There is **no `world guard`** ExecStartPre today. It runs while halted, and the task keeps
//!   that.
//! - It may also be woken by `round.finished` or `gate.verdict`, which are the moments output
//!   becomes idle (optional).
//! - Another caller exists: the `land` pass forks `target-reap` with no flags after landing
//!   (`landing-pass/src/pass.rs:228-233`). That full mode (landed, idle, tmp and gate-target
//!   reaping, `target_reap_main.rs:102-123`) is a separate destination (unverified which). This
//!   contract covers only `--if-below-floor`.
//!
//! ## Observe
//!
//! The snapshot is
//! `{ free_bytes, floor_bytes, candidates: [{ id, target, idle_secs, busy, bytes }], foreign_stale: [{ path, age_secs, busy, bytes }] }`.
//!
//! This is host state. No store verb applies, because the component that decides is the
//! filesystem itself. This path makes no `spira-lc` call: `landed_fn` is unused under
//! `--if-below-floor` (`target_reap_main.rs:86-100`).
//!
//! - **Free space.** `free_bytes` (`reap.rs:271-279`). It becomes `statvfs` in-process, with no
//!   `df`.
//! - **Candidates.** `candidates(worktrees)` (`reap.rs:42-54`) under `$SPIRA_RUN/worktree`
//!   (`target_reap_main.rs:34-35`), plus `idle_age_secs` (`reap.rs:126`) and `size_of`.
//! - **Busy.** `busy::worktree_busy` scans `/proc/*/cwd` and fds (`testenv/src/busy.rs:19-24`).
//!   This is a procfs read and stays in-process. It is heavy, so the probe runs on the `host`
//!   class.
//! - **Foreign stale runs.** Run dirs under `$SPIRA_RUN/foreign-cache/<repo>/runs` older than
//!   `STALE_RUN_AGE` = 3600 s (`foreign.rs:236,262-285`).
//! - **Config**, pinned once (S10):
//!   - `SPIRA_REAP_DISK_FLOOR_GIB`, default 50 (`target_reap_main.rs:61`,
//!     `spira/conf.d/SPIRA_REAP_DISK_FLOOR_GIB`)
//!   - `SPIRA_REAP_IDLE_SECS`, default 21,600 (`target_reap_main.rs:60`)
//!   - `SPIRA_RUN` (`target_reap_main.rs:34`)
//!   - Today the first two are read env-first, then from the config file
//!     (`target_reap_main.rs:53-59`). That bypasses `process::cfg`, which law-long-lived-processes
//!     forbids, so the task reads them only from `Settings`.
//!
//! ## Decide
//!
//! `decide(snap) -> Vec<Removal>`. It is pure and has no carried state.
//!
//! - **Foreign runs.** Every `foreign_stale` entry that is not busy and is at least 3600 s old
//!   (`foreign.rs:275`).
//! - **Above the floor:** nothing more (`target_reap_main.rs:87-88`).
//! - **Below the floor:** idle, non-busy candidates with `idle_secs >= SPIRA_REAP_IDLE_SECS`,
//!   sorted oldest first, then by id (`reap.rs:165-178`). It takes them until the summed `bytes`
//!   reach `floor_bytes - free_bytes`. *This differs from the code, which re-reads free space
//!   after each removal (`reap.rs:181-188`). The task re-observes on the next tick instead, so a
//!   tick may over-reclaim by one target.*
//! - **Unreadable free space:** reclaim nothing and report it (`target_reap_main.rs:89-92`). That
//!   is a refusal, not idle.
//! - **Fixtures.** The `reap.rs` and `foreign.rs` unit tests (`reap_idle` with fake age, busy and
//!   free closures).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.target.remove_target` (`remove_dir_all`, or `remove_file` for a symlink, `reap.rs:190-198`) | host | 120 s | `target path + mtime` | a target now busy, or one younger than the idle threshold (re-checked at run time) |
//! | `sweep.target.remove_foreign_run` (`remove_run_dir`, `foreign.rs:240-248`) | host; its `podman unshare` fallback is container | 300 s | `run path` | a busy run, or one younger than 3600 s |
//!
//! Each removal reports freed bytes as an effect outcome. The report line (`reap.rs:282`) becomes
//! `ctx.event()` fields.
//!
//! ## Silence
//!
//! An idle tick has free space at or above the floor and no stale foreign run. It emits the
//! observe probe and nothing else. Today the unit prints "0 stale foreign-cache run dir(s)" every
//! 2 min (`target_reap_main.rs:79-84`). That line goes away.
//!
//! ## Replaces
//!
//! - `spira-target-reap-prod.{service,timer}`.
//! - Children: `df` and `podman unshare rm`.
//! - Pinned keys: `SPIRA_REAP_DISK_FLOOR_GIB`, `SPIRA_REAP_IDLE_SECS`, `SPIRA_RUN`.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Per task, `Observe` mode shadows both removals. That is
//! harmless, because the disk only fills.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "sweep.target";
pub const STREAM: &str = "task.sweep.target";
pub const TICK: EventKindTag = "sweep.target.tick";
/// The task carries nothing; the tag exists so the shape is the family's.
pub const STATE: EventKindTag = "sweep.target.state";
pub const OBSERVE: &str = "sweep.target.observe";
pub const REMOVE_TARGET: &str = "sweep.target.remove_target";
pub const REMOVE_FOREIGN_RUN: &str = "sweep.target.remove_foreign_run";

pub const CADENCE_MS: u64 = 120_000;
/// The probe walks worktrees and `/proc` on the `host` class; the contract gives it no deadline.
pub const OBSERVE_DEADLINE_MS: u64 = 60_000;
/// `STALE_RUN_AGE` (`foreign.rs:236`).
pub const STALE_RUN_AGE_SECS: u64 = 3_600;

pub const ACTS: &[(&str, u64)] = &[(REMOVE_TARGET, 120_000), (REMOVE_FOREIGN_RUN, 300_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, REMOVE_TARGET, REMOVE_FOREIGN_RUN];
pub const KEYS: &[&str] = &["SPIRA_REAP_DISK_FLOOR_GIB", "SPIRA_REAP_IDLE_SECS", "SPIRA_RUN"];
pub const UNREGISTERED: &[&str] = &[];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// One worktree's build output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub target: String,
    pub idle_secs: u64,
    pub busy: bool,
    pub bytes: u64,
}

/// One foreign-cache run directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignRun {
    pub path: String,
    pub age_secs: u64,
    pub busy: bool,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// `statvfs` of the worktree root; `None` when unreadable, which reclaims nothing.
    pub free_bytes: Option<u64>,
    /// `SPIRA_REAP_DISK_FLOOR_GIB` in bytes, as pinned.
    pub floor_bytes: u64,
    /// `SPIRA_REAP_IDLE_SECS`, as pinned.
    pub idle_secs_min: u64,
    pub candidates: Vec<Candidate>,
    pub foreign_stale: Vec<ForeignRun>,
}

/// No carried state: every tick re-observes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {}

/// Stale foreign runs, then, below the floor only, idle targets oldest first until the deficit
/// is covered (one target of overshoot allowed).
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepTarget {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl SweepTarget {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        SweepTarget { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sweep.target/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for SweepTarget {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, String::new(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
