//! # sweep.reclaim — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-reclaim.md`;
//! its Acceptance section is the test file `tests/it/task_c_sweep_reclaim.rs`.
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
//!   $SPIRA_RUN/worktree` (unit ExecStart).
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

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

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
    /// Effects of the current pass still without an outcome.
    pub pending: u32,
}

pub struct SweepReclaim {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

const GIB: u64 = 1 << 30;

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Observing => "observing",
        Phase::Reclaiming => "reclaiming",
        Phase::Refused => "refused",
    }
}

fn gib(cfg: &Cfg, key: &str) -> Result<u64, String> {
    let raw = cfg.get(key).ok_or_else(|| format!("{key} is read but not declared"))?;
    raw.trim().parse::<u64>().map_err(|_| format!("{key} must be a whole number of GiB, got {raw:?}"))
}

fn act(name: &str, input: String, deadline_ms: u64) -> Act {
    Act { name: name.into(), input, deadline_ms, probe: false }
}

/// The acts a snapshot calls for: `reclaim::run`'s branching, split from its IO, and
/// `reclaim::trim_selection` for the cache.
pub fn plan(cfg: &Cfg, snap: &Snapshot) -> Result<Vec<Act>, String> {
    let (min_free, target_free) = (gib(cfg, MIN_FREE_KEY)? << 10, gib(cfg, TARGET_FREE_KEY)? << 10);
    let (podman_cap, cache_cap) = (gib(cfg, PODMAN_CAP_KEY)? * GIB, gib(cfg, CACHE_CAP_KEY)? * GIB);
    let mut acts = vec![];
    if snap.free_mib < min_free {
        acts.push(act(PODMAN_PRUNE, "root under the floor".into(), 600_000));
        let mut projected = snap.free_mib;
        for c in snap.candidates.iter().filter(|c| c.verdict == Verdict::Reclaim) {
            if projected >= target_free {
                break;
            }
            projected += c.bytes >> 20;
            acts.push(act(REMOVE_TARGET, c.path.clone(), 300_000));
        }
    } else if snap.podman_bytes > podman_cap {
        acts.push(act(PODMAN_PRUNE, format!("storage {} MiB over cap {} MiB", snap.podman_bytes >> 20, podman_cap >> 20), 600_000));
    }
    let files: Vec<(u64, u64, String)> = snap.cache.iter().map(|f| (f.mtime, f.bytes, f.path.clone())).collect();
    let trim = reclaim::trim_selection(&files, cache_cap);
    if !trim.is_empty() {
        acts.push(act(TRIM_CACHE, serde_json::to_string(&trim).map_err(|e| e.to_string())?, 120_000));
    }
    Ok(acts)
}

/// `run`'s branching split from its IO, and `trim_cache`'s selection.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire if carry.phase == Phase::Reclaiming => Err(format!("{NAME}: a tick in state reclaiming: the pass in flight has {} effect(s) without an outcome", carry.pending)),
        Input::Fire => Ok((Carry { phase: Phase::Observing, ..carry.clone() }, vec![])),
        // Single-flight: an observation while a pass still reclaims changes nothing.
        Input::Observed(_) if carry.phase == Phase::Reclaiming => Ok((carry.clone(), vec![])),
        Input::Observed(snap) => {
            if let Some((pattern, _)) = snap.require_ok.iter().find(|(_, ok)| !ok) {
                let cause = format!("positive control failed: {pattern} matches nothing, so an empty sweep proves nothing");
                return Ok((Carry { phase: Phase::Refused, refused_cause: Some(cause), pending: 0 }, vec![]));
            }
            let acts = plan(cfg, snap)?;
            if acts.is_empty() {
                return Ok((Carry::default(), vec![]));
            }
            Ok((Carry { phase: Phase::Reclaiming, refused_cause: None, pending: acts.len() as u32 }, acts))
        }
        Input::Outcome { effect, .. } if carry.phase == Phase::Reclaiming && [REMOVE_TARGET, PODMAN_PRUNE, TRIM_CACHE].contains(&effect.as_str()) => {
            let pending = carry.pending.saturating_sub(1);
            Ok((if pending == 0 { Carry::default() } else { Carry { pending, ..carry.clone() } }, vec![]))
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} an outcome of {effect} is not expected", phase_name(carry.phase))),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {:?}", phase_name(carry.phase), r.handle)),
    }
}

/// The pinned values the runners read, each a key of the row.
pub struct Pinned {
    pub globs: Vec<String>,
    pub require: Vec<String>,
    pub lease_dir: Option<std::path::PathBuf>,
    pub idle_secs: u64,
    pub target_free_mib: u64,
    pub cache_dir: Option<std::path::PathBuf>,
    pub podman: String,
    pub podman_dir: Option<std::path::PathBuf>,
}

impl Pinned {
    pub fn read(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Pinned, String> {
        let words = |k: &str| get(k).map(|v| v.split_whitespace().map(str::to_string).collect::<Vec<_>>());
        let dir = |k: &str| get(k).map(|v| Some(v.trim().to_string()).filter(|v| !v.is_empty()).map(std::path::PathBuf::from));
        let num = |k: &str| get(k).and_then(|v| v.trim().parse::<u64>().map_err(|_| format!("{k} must be a whole number, got {v:?}")));
        Ok(Pinned {
            globs: words("SPIRA_RECLAIM_GLOBS")?,
            require: words("SPIRA_RECLAIM_REQUIRE")?,
            lease_dir: dir("SPIRA_RECLAIM_LEASE_DIR")?,
            idle_secs: num("SPIRA_RECLAIM_IDLE_DAYS")? * 86_400,
            target_free_mib: num(TARGET_FREE_KEY)? << 10,
            cache_dir: dir("SPIRA_RECLAIM_CACHE_DIR")?,
            podman: get("SPIRA_RECLAIM_PODMAN")?.trim().to_string(),
            podman_dir: dir("SPIRA_RECLAIM_PODMAN_DIR")?,
        })
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The runner of [`OBSERVE`]: the host's facts, read in this process.
pub fn observe(p: &Pinned, proc_root: &std::path::Path) -> Result<String, String> {
    if p.globs.is_empty() {
        return Err("SPIRA_RECLAIM_GLOBS names no pattern — refusing".into());
    }
    let now = epoch_now();
    let require_ok = p.require.iter().map(|r| (r.clone(), !reclaim::glob(r).is_empty())).collect();
    let free_mib = spira_config::room::statvfs_mib(&reclaim::free_probe(&p.globs)).ok_or("cannot read free space")?;
    let held = reclaim::held_paths(proc_root);
    let candidates = reclaim::plan(&p.globs, now, p.idle_secs, &held, p.lease_dir.as_deref())
        .into_iter()
        .map(|c| Candidate {
            path: c.path.display().to_string(),
            bytes: c.bytes,
            age: c.age,
            verdict: match c.verdict {
                reclaim::Verdict::Reclaim => Verdict::Reclaim,
                reclaim::Verdict::Skip(why) if why == "symlink" => Verdict::Symlink,
                reclaim::Verdict::Skip(why) if why.starts_with("held") => Verdict::Held,
                reclaim::Verdict::Skip(why) if why == "leased" => Verdict::Leased,
                reclaim::Verdict::Skip(_) => Verdict::Fresh,
            },
        })
        .collect();
    let podman_bytes = p.podman_dir.as_deref().map_or(0, |d| reclaim::usage(d).bytes);
    let cache = p.cache_dir.as_deref().map(|d| reclaim::cache_files(d, now)).unwrap_or_default().into_iter().map(|(mtime, bytes, path)| CacheFile { mtime, bytes, path: path.display().to_string() }).collect();
    serde_json::to_string(&Snapshot { require_ok, free_mib, candidates, podman_bytes, cache }).map_err(|e| e.to_string())
}

/// The runner of [`REMOVE_TARGET`]: re-judges `path` at act time and removes it only when it
/// is still reclaimable, is one of the globs' matches, and free space is still short.
pub fn remove_target(p: &Pinned, proc_root: &std::path::Path, path: &str) -> Result<String, String> {
    let path = std::path::Path::new(path);
    if !p.globs.iter().flat_map(|g| reclaim::glob(g)).any(|m| m == path) {
        return Err(format!("{} is not a match of SPIRA_RECLAIM_GLOBS — refusing", path.display()));
    }
    if spira_config::room::statvfs_mib(&reclaim::free_probe(&p.globs)).is_some_and(|f| f >= p.target_free_mib) {
        return Ok(format!("kept {} (target free space reached)", path.display()));
    }
    let c = reclaim::judge(path, epoch_now(), p.idle_secs, &reclaim::held_paths(proc_root), p.lease_dir.as_deref());
    if let reclaim::Verdict::Skip(why) = c.verdict {
        return Err(format!("{} is not reclaimable at act time ({why}) — refusing", path.display()));
    }
    std::fs::remove_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("reclaimed {} {} MiB age {}s", path.display(), c.bytes >> 20, c.age))
}

/// The runner of [`PODMAN_PRUNE`]: `podman system prune -f`, the one allowed child, through
/// the process port; no other flag is ever passed.
pub fn podman_prune(p: &Pinned, proc: &dyn runtime::ports::Proc, why: &str) -> Result<String, String> {
    if p.podman.is_empty() {
        return Err("SPIRA_RECLAIM_PODMAN is empty — refusing".into());
    }
    // batch-job: a prune of tens of GB of images runs as long as its work, under the unit's CPUQuota
    let spec = runtime::ports::ProcSpec { prog: p.podman.clone(), args: reclaim::PODMAN_PRUNE_ARGS.iter().map(|a| a.to_string()).collect(), stdin: vec![], deadline: std::time::Duration::from_secs(600) };
    let out = proc.run(&spec)?;
    let text = String::from_utf8_lossy(&out.stdout);
    if out.code != Some(0) {
        return Err(format!("podman system prune -f failed ({:?}): {}", out.code, String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(format!("podman: {why}: system prune -f: {}", text.lines().rev().find(|l| l.contains("reclaimed")).unwrap_or("reclaimed size not reported").trim()))
}

/// The runner of [`TRIM_CACHE`]: removes the chosen files, each still under the cache dir and
/// not a `.tmp` younger than an hour.
pub fn trim_cache(p: &Pinned, input: &str) -> Result<String, String> {
    let dir = p.cache_dir.as_deref().ok_or("SPIRA_RECLAIM_CACHE_DIR is empty — refusing")?;
    let chosen: Vec<String> = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let live: std::collections::BTreeMap<String, u64> = reclaim::cache_files(dir, epoch_now()).into_iter().map(|(_, b, path)| (path.display().to_string(), b)).collect();
    let (mut n, mut freed) = (0u64, 0u64);
    for f in chosen.iter().filter_map(|f| live.get(f).map(|b| (f, *b))) {
        if std::fs::remove_file(f.0).is_ok() {
            n += 1;
            freed += f.1;
        }
    }
    Ok(format!("cache {}: trimmed {n} file(s), {} MiB", dir.display(), freed >> 20))
}

crate::tasks::b::hosted!(SweepReclaim);
