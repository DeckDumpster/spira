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
//! **Hand-built targets.** Every tick it also removes a cargo target that hand-driven work left
//! under a root in `SPIRA_HAND_TARGET_ROOTS` (`testenv::hand`): written last at least
//! `SPIRA_HAND_TARGET_IDLE_SECS` ago, and held by no process. A hand build points
//! `CARGO_TARGET_DIR` at `<root>/<purpose>/target`; only that `target` is removed, never the logs
//! or checkout beside it. Watchtower watches each root's filesystem for free space.
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
pub const REMOVE_HAND_TARGET: &str = "sweep.target.remove_hand_target";

pub const CADENCE_MS: u64 = 120_000;
/// The probe walks worktrees and `/proc` on the `host` class; the contract gives it no deadline.
pub const OBSERVE_DEADLINE_MS: u64 = 60_000;
/// `STALE_RUN_AGE` (`foreign.rs:236`).
pub const STALE_RUN_AGE_SECS: u64 = 3_600;

pub const ACTS: &[(&str, u64)] = &[(REMOVE_TARGET, 120_000), (REMOVE_FOREIGN_RUN, 300_000), (REMOVE_HAND_TARGET, 300_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, REMOVE_TARGET, REMOVE_FOREIGN_RUN, REMOVE_HAND_TARGET];
pub const KEYS: &[&str] = &["SPIRA_REAP_DISK_FLOOR_GIB", "SPIRA_REAP_IDLE_SECS", "SPIRA_RUN", "SPIRA_HAND_TARGET_ROOTS", "SPIRA_HAND_TARGET_IDLE_SECS"];
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

/// A hand-built target nothing has written for the idle threshold. `written_at` is when it was
/// last written, in Unix seconds, so a rebuilt target at the same path is a new removal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandTarget {
    pub path: String,
    pub guard: String,
    pub written_at: u64,
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
    #[serde(default)]
    pub hand_stale: Vec<HandTarget>,
}

/// No carried state: every tick re-observes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {}

/// Stale foreign runs, then, below the floor only, idle targets oldest first until the deficit
/// is covered (one target of overshoot allowed). The order and the cover are testenv's own
/// `reap::pick_idle`; the staleness rule is `foreign::is_stale`.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let stale_age = std::time::Duration::from_secs(STALE_RUN_AGE_SECS);
    let mut acts: Vec<Act> = snap
        .foreign_stale
        .iter()
        .filter(|r| testenv::foreign::is_stale(Some(std::time::Duration::from_secs(r.age_secs)), stale_age, r.busy))
        .map(|r| Act::new(REMOVE_FOREIGN_RUN, r.path.clone()).with_input(serde_json::to_string(r).unwrap_or_default()))
        .collect();
    acts.extend(snap.hand_stale.iter().filter(|h| !h.busy).map(|h| Act::new(REMOVE_HAND_TARGET, format!("{}@{}", h.path, h.written_at)).with_input(serde_json::to_string(h).unwrap_or_default())));
    let Some(free) = snap.free_bytes else { return (carry.clone(), acts) };
    if free >= snap.floor_bytes {
        return (carry.clone(), acts);
    }
    let items: Vec<testenv::reap::IdleTarget> = snap.candidates.iter().map(|c| testenv::reap::IdleTarget { id: c.id.clone(), idle_secs: c.idle_secs, busy: c.busy, bytes: c.bytes }).collect();
    for i in testenv::reap::pick_idle(&items, snap.idle_secs_min, Some(snap.floor_bytes - free)) {
        let c = &snap.candidates[i];
        acts.push(Act::new(REMOVE_TARGET, c.target.clone()).with_input(serde_json::to_string(&Removal { candidate: c.clone(), idle_secs_min: snap.idle_secs_min }).unwrap_or_default()));
    }
    (carry.clone(), acts)
}

/// `remove_target`'s input: the candidate, and the idle threshold it is re-checked against.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal {
    pub candidate: Candidate,
    pub idle_secs_min: u64,
}

// ---------------------------------------------------------------------------------------------
// Runners. Every read and removal is testenv's library, in this process: no `df`, no child.

const GIB: u64 = 1 << 30;

fn pinned_u64(key: &str) -> Result<u64, String> {
    let v = spira_config::process::cfg(key)?;
    v.trim().parse().map_err(|_| format!("{key} must be a non-negative integer, got {v:?}"))
}

/// The snapshot: free space under `$SPIRA_RUN/worktree` by `statvfs`, and, only when it is
/// below the floor, the candidates with their idle age, busy-ness and size; the stale foreign
/// runs every tick. An idle tick above the floor walks no target at all.
pub fn observe(run: &std::path::Path) -> Result<String, String> {
    let worktrees = run.join("worktree");
    let floor_bytes = pinned_u64("SPIRA_REAP_DISK_FLOOR_GIB")? * GIB;
    let idle_secs_min = pinned_u64("SPIRA_REAP_IDLE_SECS")?;
    let free_bytes = testenv::reap::free_bytes(&worktrees);
    let mut candidates = vec![];
    if free_bytes.is_some_and(|f| f < floor_bytes) {
        for (id, dir, t) in testenv::reap::candidates(&worktrees) {
            let Some(idle_secs) = testenv::reap::idle_age_secs(&dir) else { continue };
            let busy = idle_secs >= idle_secs_min && testenv::busy::worktree_busy(&dir);
            candidates.push(Candidate { id, target: t.display().to_string(), idle_secs, busy, bytes: testenv::reap::size_of(&t) });
        }
    }
    let stale_age = std::time::Duration::from_secs(STALE_RUN_AGE_SECS);
    let foreign_stale = testenv::foreign::run_dirs(&run.join("foreign-cache"))
        .into_iter()
        .filter(|(_, age)| testenv::foreign::is_stale(*age, stale_age, false))
        .map(|(path, age)| ForeignRun { busy: testenv::busy::worktree_busy(&path), bytes: testenv::reap::size_of(&path), age_secs: age.map_or(0, |a| a.as_secs()), path: path.display().to_string() })
        .collect();
    let hand_idle = pinned_u64("SPIRA_HAND_TARGET_IDLE_SECS")?;
    let roots: Vec<_> = spira_config::process::cfg("SPIRA_HAND_TARGET_ROOTS")?.split_whitespace().map(std::path::PathBuf::from).collect();
    let now = std::time::SystemTime::now();
    let hand_stale = testenv::hand::candidates(&roots)
        .into_iter()
        .filter_map(|t| {
            let written_at = testenv::hand::stale_written_at(&t.path, now, hand_idle)?;
            Some(HandTarget { busy: testenv::hand::held(&t), bytes: testenv::reap::size_of(&t.path), path: t.path.display().to_string(), guard: t.guard.display().to_string(), written_at })
        })
        .collect();
    serde_json::to_string(&Snapshot { free_bytes, floor_bytes, idle_secs_min, candidates, foreign_stale, hand_stale }).map_err(|e| e.to_string())
}

/// Remove one idle target, re-checking at run time that it is neither busy nor younger than
/// the threshold. Answers the bytes freed.
pub fn remove_target(input: &str) -> Result<String, String> {
    let r: Removal = serde_json::from_str(input).map_err(|e| format!("{REMOVE_TARGET}: bad input: {e}"))?;
    let t = std::path::Path::new(&r.candidate.target);
    let dir = t.parent().ok_or_else(|| format!("{REMOVE_TARGET}: {} has no worktree", t.display()))?;
    match testenv::reap::idle_age_secs(dir) {
        Some(a) if a >= r.idle_secs_min => {}
        _ => return Err(format!("{REMOVE_TARGET}: {} is no longer idle — refused", dir.display())),
    }
    if testenv::busy::worktree_busy(dir) {
        return Err(format!("{REMOVE_TARGET}: {} is busy — refused", dir.display()));
    }
    testenv::reap::remove_target(t).map(|n| n.to_string())
}

/// Remove one stale foreign run, re-checking that it is still stale and unbusy. Answers the
/// bytes freed.
pub fn remove_foreign_run(input: &str) -> Result<String, String> {
    let r: ForeignRun = serde_json::from_str(input).map_err(|e| format!("{REMOVE_FOREIGN_RUN}: bad input: {e}"))?;
    let path = std::path::Path::new(&r.path);
    if !path.exists() {
        return Ok("0".into());
    }
    let age = std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    if !testenv::foreign::is_stale(age, std::time::Duration::from_secs(STALE_RUN_AGE_SECS), testenv::busy::worktree_busy(path)) {
        return Err(format!("{REMOVE_FOREIGN_RUN}: {} is busy or young — refused", path.display()));
    }
    let bytes = testenv::reap::size_of(path);
    if testenv::foreign::remove_run_dir(path) {
        Ok(bytes.to_string())
    } else {
        Err(format!("{REMOVE_FOREIGN_RUN}: {} could not be removed", path.display()))
    }
}

/// Remove one hand-built target, re-checking at run time that it is still unwritten past the
/// threshold, under a configured root, and held by no process. Answers the bytes freed.
pub fn remove_hand_target(input: &str) -> Result<String, String> {
    let h: HandTarget = serde_json::from_str(input).map_err(|e| format!("{REMOVE_HAND_TARGET}: bad input: {e}"))?;
    let roots: Vec<_> = spira_config::process::cfg("SPIRA_HAND_TARGET_ROOTS")?.split_whitespace().map(std::path::PathBuf::from).collect();
    let t = testenv::hand::candidates(&roots).into_iter().find(|t| t.path.as_os_str() == h.path.as_str()).ok_or_else(|| format!("{REMOVE_HAND_TARGET}: {} is under no configured root — refused", h.path))?;
    if testenv::hand::stale_written_at(&t.path, std::time::SystemTime::now(), pinned_u64("SPIRA_HAND_TARGET_IDLE_SECS")?).is_none() {
        return Err(format!("{REMOVE_HAND_TARGET}: {} was written within the threshold — refused", h.path));
    }
    if testenv::hand::held(&t) {
        return Err(format!("{REMOVE_HAND_TARGET}: {} is held by a process — refused", h.path));
    }
    testenv::reap::remove_target(&t.path).map(|n| n.to_string())
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

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
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
