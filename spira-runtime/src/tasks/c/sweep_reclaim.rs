//! # sweep.reclaim — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-reclaim.md`;
//! its Acceptance section is the test file `tests/task_c_sweep_reclaim.rs`.
//!
//! ## Tick
//!
//! - **Cadence:** 1 h (`OnUnitActiveSec=1h`, `AccuracySec=5min`). The first tick comes 15 min after
//!   boot (`OnBootSec=15min`). The unit sets `Persistent=true`, which systemd ignores for a
//!   monotonic-only timer, so there is no catch-up.
//! - **Wake:** disk pressure is a candidate wake later (a `room.below_floor` event). The
//!   `sweep.target` task (`target-reap`) already covers the floor every 2 min. Not proposed here.
//! - **Single-flight:** one pass at a time.
//! - **No world guard:** unlike the hel units, this unit has no `ExecStartPre=world guard`. Its
//!   removals are lossless, so it runs on a halted world too, and the contract keeps that.
//!
//! ## Observe
//!
//! `sweep.reclaim.observe` (class `host`, deadline 120 s; walking `target/` trees is the slow part).
//! Snapshot:
//! `{require_ok:[(pattern, matched)], free_mib, candidates:[{path, bytes, age, verdict}], podman_bytes, cache:[{mtime, bytes, path}]}`.
//! - `require_ok` is the positive control. Each `--require` pattern must match something, or the
//!   pass removes nothing (`reclaim/src/lib.rs:200-204`). Today that is `--require
//!   /home/ryan/spira/run/worktree` (unit ExecStart).
//! - `free_mib` is `spira_config::room::statvfs_mib` on the first existing ancestor of the first
//!   glob (`reclaim/src/main.rs:76-79`).
//! - `candidates` is `plan(patterns, now, idle_secs, held, lease_dir)` (`lib.rs:125-129`), oldest
//!   first. `held` is every live `/proc/*/{cwd,fd/*}` and each `CARGO_TARGET_DIR=` in
//!   `environ` (`lib.rs:70-91`). Each verdict comes from `judge` (`lib.rs:106-122`): a symlink, a
//!   held path, a leased path (`<lease-dir>/<name>.lease`) or a path touched within the idle
//!   window is skipped.
//! - `podman_bytes` is the block usage of `--podman-dir` (`lib.rs:46-60`, `lib.rs:234`).
//! - `cache` is the file list under `--cache-dir`, leaving out `.tmp` files younger than 1 h
//!   (`lib.rs:131-148`).
//!
//! This reads the host, not a store. That is correct here, because the host's own filesystem is
//! the component that decides free space and liveness. No Spira store owns these facts.
//!
//! ## Decide
//!
//! The pure core is `run`'s branching (`lib.rs:199-246`), split from its IO, and `trim_cache`'s
//! selection (`lib.rs:149-161`).
//! 1. If any `require` matched nothing, emit nothing. The task records `sweep.reclaim.refused`
//!    once per distinct cause.
//! 2. If `free < min_free` (70 GiB, `main.rs:17`), emit `podman_prune` and then one
//!    `remove_target` per `Reclaim` candidate, oldest first, until the projected free space
//!    reaches `target_free` (100 GiB, `main.rs:18`). Today free space is re-read before each
//!    removal (`lib.rs:220`). The task projects it from candidate bytes, and each effect re-reads
//!    it before acting (see Act).
//! 3. Else, if `podman_bytes > podman_cap` (40 GiB, unit `--podman-cap-gib 40`), emit
//!    `podman_prune` (`lib.rs:232-239`).
//! 4. If the cache total is over `cache_cap` (default 40 GiB, `main.rs:20`), emit `trim_cache` for
//!    the oldest files until the total is under the cap.
//!
//! - **Carry:** `{refused_cause: Option<String>}`, so a persistent control failure is said once.
//! - **Fixtures:** `reclaim/src/tests.rs`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.reclaim.remove_target` | host | 300 s | `path:newest_mtime` | It refuses when the path is held, leased or a symlink at act time (it re-judges, `lib.rs:106`). It also refuses when free space has already reached the target (`lib.rs:220-223`). |
//! | `sweep.reclaim.podman_prune` | container | 600 s | `prune:<tick>` | It refuses any flag beyond `system prune -f`: no `-a`, no `--volumes` (`lib.rs:180-196`). The `podman` fork at `lib.rs:187` remains the allowed child. |
//! | `sweep.reclaim.trim_cache` | host | 120 s | `cache_dir:<tick>` | It refuses a `.tmp` file younger than 1 h (`lib.rs:142`), and stops at the cap. |
//!
//! The class cap for `container` is ≤2 (S3). `podman_prune` shares that cap with gate and cert
//! runs, and must not starve them. That is a deferral, not a drop.
//!
//! ## Silence
//!
//! An idle tick does three things: it issues the observe probe; it finds free space at or above
//! 70 GiB, podman under its cap and the cache under its cap; and it emits nothing. Today the unit
//! always prints `freed 0 MiB` and the cache line (`main.rs:82`, `lib.rs:243`). These become
//! wide-event fields.
//!
//! ## Replaces
//!
//! - `spira-reclaim-prod.{service,timer}`, `CPUQuota=50%` and `TimeoutStartSec=infinity`. An effect
//!   deadline replaces the infinite timeout.
//! - Children: `podman system prune -f` stays as an edge child (class D). The catalogue's "—" is
//!   **wrong**: the code forks podman (`lib.rs:187`).
//! - Pinned keys (S10). Today these are argv, not conf.d, and they become `Settings` keys:
//!   - `sweep.reclaim.globs` — the four from the unit
//!   - `require`, `lease_dir`
//!   - `idle_days` (2, `main.rs:16`)
//!   - `min_free_gib` (70), `target_free_gib` (100)
//!   - `cache_dir` (`run/sccache-dav`), `cache_cap_gib` (40)
//!   - `podman`, `podman_dir` (`%h/.local/share/containers/storage`), `podman_cap_gib` (40)
//!   - Also `SPIRA_METRICS_ENDPOINT` (`main.rs:56`).
//!
//! ## Reverse
//!
//! Roll back the release, which restores the unit and timer, or set `Observe` mode. Every removal
//! is lossless (rebuildable), so an act needs no recovery path.
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.reclaim";
pub const STREAM: &str = "task.sweep.reclaim";
pub const TICK: EventKindTag = "sweep.reclaim.tick";
pub const STATE: EventKindTag = "sweep.reclaim.state";
/// Recorded once per distinct control failure.
pub const REFUSED: EventKindTag = "sweep.reclaim.refused";

pub const OBSERVE: &str = "sweep.reclaim.observe";
pub const REMOVE_TARGET: &str = "sweep.reclaim.remove_target";
pub const PODMAN_PRUNE: &str = "sweep.reclaim.podman_prune";
pub const TRIM_CACHE: &str = "sweep.reclaim.trim_cache";
pub const EFFECTS: &[&str] = &[OBSERVE, REMOVE_TARGET, PODMAN_PRUNE, TRIM_CACHE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_RECLAIM_SECS";
pub const MIN_FREE_KEY: &str = "SPIRA_RECLAIM_MIN_FREE_GIB";
pub const TARGET_FREE_KEY: &str = "SPIRA_RECLAIM_TARGET_FREE_GIB";
pub const PODMAN_CAP_KEY: &str = "SPIRA_RECLAIM_PODMAN_CAP_GIB";
pub const CACHE_CAP_KEY: &str = "SPIRA_RECLAIM_CACHE_CAP_GIB";
/// Today argv; each becomes a key.
pub const KEYS: &[&str] = &[
    CADENCE_KEY,
    "SPIRA_RECLAIM_GLOBS",
    "SPIRA_RECLAIM_REQUIRE",
    "SPIRA_RECLAIM_LEASE_DIR",
    "SPIRA_RECLAIM_IDLE_DAYS",
    MIN_FREE_KEY,
    TARGET_FREE_KEY,
    "SPIRA_RECLAIM_CACHE_DIR",
    CACHE_CAP_KEY,
    "SPIRA_RECLAIM_PODMAN",
    "SPIRA_RECLAIM_PODMAN_DIR",
    PODMAN_CAP_KEY,
];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 120_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// `Reclaim`, or why a candidate is skipped (`judge`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Reclaim,
    Symlink,
    Held,
    Leased,
    #[default]
    Fresh,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Candidate {
    pub path: String,
    pub bytes: u64,
    pub age: u64,
    pub verdict: Verdict,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheFile {
    pub mtime: u64,
    pub bytes: u64,
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Each `--require` pattern and whether it matched: the positive control.
    pub require_ok: Vec<(String, bool)>,
    pub free_mib: u64,
    /// Oldest first.
    pub candidates: Vec<Candidate>,
    pub podman_bytes: u64,
    pub cache: Vec<CacheFile>,
}

/// The pass machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Observing,
    Reclaiming,
    Refused,
}

/// `state × event → state`. A tick while `reclaiming` is refused, naming `reclaiming`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "tick", Phase::Observing), (Phase::Observing, "observed:low", Phase::Reclaiming), (Phase::Observing, "observed:idle", Phase::Idle), (Phase::Observing, "require:unmatched", Phase::Refused), (Phase::Refused, "tick", Phase::Observing), (Phase::Reclaiming, "outcome", Phase::Reclaiming), (Phase::Reclaiming, "done", Phase::Idle)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub refused_cause: Option<String>,
}

pub struct SweepReclaim {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `run`'s branching split from its IO, and `trim_cache`'s selection.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("sweep.reclaim: not ported yet")
}

crate::tasks::b::hosted!(SweepReclaim);
