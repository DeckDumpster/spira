//! # sweep.reclaim — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-reclaim.md`;
//! its Acceptance section is the test file `tests/it/task_c_sweep_reclaim.rs`.
//!
//! Three lossless clean-ups, nothing else: the sccache store is trimmed to its cap, idle top-level tmp
//! directories are reaped, and git maintenance runs on each mapped repo. Build-target directories and
//! free space belong to `sweep.target` and the reconciler's disk remedy; this task does not react to them.
//!
//! ## Tick
//!
//! - **Cadence:** 1 h. The first tick comes 15 min after boot.
//! - **Single-flight:** one pass at a time.
//! - **No world guard:** the removals are lossless, so the task runs on a halted world too.
//!
//! ## Observe
//!
//! `sweep.reclaim.observe` (class `host`, deadline 120 s). Snapshot:
//! `{repos:[{name, path, error}], cache:[{mtime, bytes, path}], tmp:[{path, bytes, age, verdict}]}`.
//! - `cache` is the file list under the cache dir, leaving out `.tmp` files younger than 1 h
//!   (`reclaim::cache_files`).
//! - `tmp` is `reclaim::tmp_plan(tmp_root, uid, now, tmp_idle, held)`: the top-level directories of
//!   `SPIRA_TMP_PROBE_PATH` owned by the sweep's uid, oldest first. `held` is every live
//!   `/proc/*/{cwd,fd/*}` and each `CARGO_TARGET_DIR=` in `environ`. A symlink, a held path or a path
//!   touched within the idle window is skipped, and a directory containing a socket or fifo is `special`,
//!   because `/proc/<pid>/fd` names a unix socket by inode and its holder would look absent. Plain files
//!   are not candidates.
//!
//! This reads the host, not a store: the host's own filesystem decides liveness.
//!
//! ## Decide
//!
//! 1. One `remove_tmp` per `Reclaim` tmp candidate, whatever the free space: a tmpfs costs RAM before it
//!    costs space.
//! 2. One `maintain` per row of the repo map.
//! 3. If the cache total is over the cap (`SPIRA_RECLAIM_CACHE_CAP_GIB`), one `trim_cache` for the oldest
//!    files until the total is under it (`reclaim::trim_selection`).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.reclaim.remove_tmp` | host | 300 s | `path:age` | A path that is not a top-level directory of the tmp root this uid owns, or that is held, a symlink, holds a socket or fifo, or was touched within the idle window at act time. |
//! | `sweep.reclaim.trim_cache` | host | 120 s | `cache` | A `.tmp` file younger than 1 h; stops at the cap. |
//! | `sweep.reclaim.maintain` | git | 600 s | `maintain:<name>` | A repo whose row could not be read, or whose `objects/maintenance.lock` is younger than 1200 s. |
//!
//! ## Git maintenance
//!
//! Each repo gets `git maintenance run --task=loose-objects --task=incremental-repack`, then `--task=loose-objects`
//! again, because that task packs on one run and deletes the packed copies on the next. Each run packs at most 50 000 loose
//! objects (git's default batch), so a large backlog drains over several passes. No `gc`, `prune` or `--aggressive`.
//! The outcome reports `count` from `git count-objects -v` before and after; a failure names the repo with git's stderr, and
//! never stops the other rows. A `objects/maintenance.lock` makes git exit 0 having done nothing: older than 1200 s it is
//! removed and said so, younger is an error.
//!
//! ## Silence
//!
//! An idle tick issues the observe probe and, apart from the maintenance leg, emits nothing.
//!
//! ## Pinned keys
//!
//! `SPIRA_RUNTIME_SWEEP_RECLAIM_SECS`, `SPIRA_RECLAIM_CACHE_DIR` (else `SPIRA_SCCACHE_DAV_ROOT`, else `$SPIRA_RUN/sccache-dav`),
//! `SPIRA_RECLAIM_CACHE_CAP_GIB`, `SPIRA_TMP_PROBE_PATH` (shared with watchtower) and `SPIRA_RECLAIM_TMP_IDLE_HOURS`.
//!
//! ## Reverse
//!
//! Roll back the release, or set `Observe` mode. Every removal is lossless (rebuildable), so an act needs no recovery path.
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.reclaim";
pub const STREAM: &str = "task.sweep.reclaim";
pub const TICK: EventKindTag = "sweep.reclaim.tick";
pub const STATE: EventKindTag = "sweep.reclaim.state";

pub const OBSERVE: &str = "sweep.reclaim.observe";
pub const TRIM_CACHE: &str = "sweep.reclaim.trim_cache";
pub const MAINTAIN: &str = "sweep.reclaim.maintain";
pub const REMOVE_TMP: &str = "sweep.reclaim.remove_tmp";
pub const EFFECTS: &[&str] = &[OBSERVE, TRIM_CACHE, MAINTAIN, REMOVE_TMP];
/// The only git maintenance tasks ever run: both lossless, neither prunes reachable history.
pub const MAINTAIN_ARGS: &[&str] = &["maintenance", "run", "--task=loose-objects", "--task=incremental-repack"];
const MAINTAIN_DEADLINE_MS: u64 = 600_000;

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_RECLAIM_SECS";
pub const CACHE_DIR_KEY: &str = "SPIRA_RECLAIM_CACHE_DIR";
pub const STORE_ROOT_KEY: &str = "SPIRA_SCCACHE_DAV_ROOT";
pub const CACHE_CAP_KEY: &str = "SPIRA_RECLAIM_CACHE_CAP_GIB";
pub const TMP_ROOT_KEY: &str = "SPIRA_TMP_PROBE_PATH";
pub const TMP_IDLE_KEY: &str = "SPIRA_RECLAIM_TMP_IDLE_HOURS";
pub const KEYS: &[&str] = &[CADENCE_KEY, CACHE_DIR_KEY, STORE_ROOT_KEY, "SPIRA_RUN", CACHE_CAP_KEY, TMP_ROOT_KEY, TMP_IDLE_KEY];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 120_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None, jitter_key: None };

/// `Reclaim`, or why a candidate is skipped (`judge`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Reclaim,
    Symlink,
    Held,
    Special,
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

/// One row of the repo map.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Repo {
    pub name: String,
    pub path: String,
    /// Set when the row could not be read; the effect reports it instead of maintaining.
    pub error: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Every registered checkout; each gets one [`MAINTAIN`] per pass.
    pub repos: Vec<Repo>,
    pub cache: Vec<CacheFile>,
    /// Top-level directories of the tmp root, oldest first.
    pub tmp: Vec<Candidate>,
}

/// The pass machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Observing,
    Reclaiming,
}

/// `state × event → state`. A tick while `reclaiming` is refused, naming `reclaiming`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "tick", Phase::Observing), (Phase::Observing, "observed:low", Phase::Reclaiming), (Phase::Observing, "observed:idle", Phase::Idle), (Phase::Reclaiming, "outcome", Phase::Reclaiming), (Phase::Reclaiming, "done", Phase::Idle)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
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
    }
}

fn gib(cfg: &Cfg, key: &str) -> Result<u64, String> {
    let raw = cfg.get(key).ok_or_else(|| format!("{key} is read but not declared"))?;
    raw.trim().parse::<u64>().map_err(|_| format!("{key} must be a whole number of GiB, got {raw:?}"))
}

fn act(name: &str, key: String, input: String, deadline_ms: u64) -> Act {
    Act { name: name.into(), key, input, deadline_ms, probe: false }
}

/// The acts a snapshot calls for: the tmp, maintain and cache legs, the cache chosen by
/// `reclaim::trim_selection`.
pub fn plan(cfg: &Cfg, snap: &Snapshot) -> Result<Vec<Act>, String> {
    let cache_cap = gib(cfg, CACHE_CAP_KEY)? * GIB;
    let mut acts = vec![];
    for c in snap.tmp.iter().filter(|c| c.verdict == Verdict::Reclaim) {
        acts.push(act(REMOVE_TMP, format!("{}:{}", c.path, c.age), c.path.clone(), 300_000));
    }
    for r in &snap.repos {
        acts.push(act(MAINTAIN, format!("maintain:{}", r.name), serde_json::to_string(r).map_err(|e| e.to_string())?, MAINTAIN_DEADLINE_MS));
    }
    let files: Vec<(u64, u64, String)> = snap.cache.iter().map(|f| (f.mtime, f.bytes, f.path.clone())).collect();
    let trim = reclaim::trim_selection(&files, cache_cap);
    if !trim.is_empty() {
        acts.push(act(TRIM_CACHE, "cache".into(), serde_json::to_string(&trim).map_err(|e| e.to_string())?, 120_000));
    }
    Ok(acts)
}

pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire if carry.phase == Phase::Reclaiming => Err(format!("{NAME}: a tick in state reclaiming: the pass in flight has {} effect(s) without an outcome", carry.pending)),
        Input::Fire => Ok((Carry { phase: Phase::Observing, ..carry.clone() }, vec![])),
        // Single-flight: an observation while a pass still reclaims changes nothing.
        Input::Observed(_) if carry.phase == Phase::Reclaiming => Ok((carry.clone(), vec![])),
        Input::Observed(snap) => {
            let acts = plan(cfg, snap)?;
            if acts.is_empty() {
                return Ok((Carry::default(), vec![]));
            }
            Ok((Carry { phase: Phase::Reclaiming, pending: acts.len() as u32 }, acts))
        }
        Input::Outcome { effect, .. } if carry.phase == Phase::Reclaiming && [TRIM_CACHE, MAINTAIN, REMOVE_TMP].contains(&effect.as_str()) => {
            let pending = carry.pending.saturating_sub(1);
            Ok((if pending == 0 { Carry::default() } else { Carry { pending, ..carry.clone() } }, vec![]))
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} an outcome of {effect} is not expected", phase_name(carry.phase))),
        Input::Lost(lost) => lost.as_failure(decide, cfg, carry, now_ms),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {:?}", phase_name(carry.phase), r.handle)),
    }
}

/// The pinned values the runners read, each a key of the row.
pub struct Pinned {
    pub cache_dir: Option<std::path::PathBuf>,
    pub tmp_root: Option<std::path::PathBuf>,
    pub tmp_idle_secs: u64,
}

fn cache_dir(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Option<std::path::PathBuf>, String> {
    for k in [CACHE_DIR_KEY, STORE_ROOT_KEY] {
        let v = get(k)?;
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        return if std::path::Path::new(v).is_absolute() { Ok(Some(v.into())) } else { Err(format!("{k} must be an absolute path, got {v:?}")) };
    }
    let run = get("SPIRA_RUN")?;
    Ok(Some(std::path::Path::new(run.trim()).join("sccache-dav")).filter(|d| d.is_absolute()))
}

impl Pinned {
    pub fn read(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Pinned, String> {
        let dir = |k: &str| get(k).map(|v| Some(v.trim().to_string()).filter(|v| !v.is_empty()).map(std::path::PathBuf::from));
        let num = |k: &str| get(k).and_then(|v| v.trim().parse::<u64>().map_err(|_| format!("{k} must be a whole number, got {v:?}")));
        Ok(Pinned { cache_dir: cache_dir(get)?, tmp_root: dir(TMP_ROOT_KEY)?, tmp_idle_secs: num(TMP_IDLE_KEY)? * 3_600 })
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn to_candidates(planned: Vec<reclaim::Candidate>) -> Vec<Candidate> {
    planned
        .into_iter()
        .map(|c| Candidate {
            path: c.path.display().to_string(),
            bytes: c.bytes,
            age: c.age,
            verdict: match c.verdict {
                reclaim::Verdict::Reclaim => Verdict::Reclaim,
                reclaim::Verdict::Skip(why) if why == "symlink" => Verdict::Symlink,
                reclaim::Verdict::Skip(why) if why.starts_with("held") => Verdict::Held,
                reclaim::Verdict::Skip(why) if why.starts_with("holds a socket") => Verdict::Special,
                reclaim::Verdict::Skip(_) => Verdict::Fresh,
            },
        })
        .collect()
}

/// The runner of [`OBSERVE`]: the host's facts, read in this process.
pub fn observe(p: &Pinned, proc_root: &std::path::Path, repos: Vec<Repo>) -> Result<String, String> {
    let now = epoch_now();
    let held = reclaim::held_paths(proc_root);
    let tmp = p.tmp_root.as_deref().map(|r| to_candidates(reclaim::tmp_plan(r, reclaim::own_uid(), now, p.tmp_idle_secs, &held))).unwrap_or_default();
    let cache = p.cache_dir.as_deref().map(|d| reclaim::cache_files(d, now)).unwrap_or_default().into_iter().map(|(mtime, bytes, path)| CacheFile { mtime, bytes, path: path.display().to_string() }).collect();
    serde_json::to_string(&Snapshot { repos, cache, tmp }).map_err(|e| e.to_string())
}

/// The runner of [`REMOVE_TMP`]: re-plans the tmp root at act time and removes `path` only when
/// it is still a top-level directory there and still reclaimable.
pub fn remove_tmp(p: &Pinned, proc_root: &std::path::Path, path: &str) -> Result<String, String> {
    let root = p.tmp_root.as_deref().ok_or("no tmp root is configured — refusing")?;
    let path = std::path::Path::new(path);
    let c = reclaim::tmp_plan(root, reclaim::own_uid(), epoch_now(), p.tmp_idle_secs, &reclaim::held_paths(proc_root)).into_iter().find(|c| c.path == path).ok_or_else(|| format!("{} is not a directory of {} this process owns — refusing", path.display(), root.display()))?;
    if let reclaim::Verdict::Skip(why) = c.verdict {
        return Err(format!("{} is not reclaimable at act time ({why}) — refusing", path.display()));
    }
    std::fs::remove_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("reclaimed {} {} MiB age {}s", path.display(), c.bytes >> 20, c.age))
}

/// The runner of [`TRIM_CACHE`]: removes the chosen files, each still under the cache dir and
/// not a `.tmp` younger than an hour.
pub fn trim_cache(p: &Pinned, input: &str) -> Result<String, String> {
    let dir = p.cache_dir.as_deref().ok_or("no cache store root resolves — refusing")?;
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

/// `count:` of `git count-objects -v`: the loose objects.
fn loose_count(proc: &dyn runtime::ports::Proc, path: &str) -> Result<u64, String> {
    // batch-job: counting loose objects walks the whole object store, cold
    let spec = runtime::ports::ProcSpec { prog: "git".into(), args: ["-C", path, "count-objects", "-v"].map(String::from).to_vec(), stdin: vec![], deadline: std::time::Duration::from_secs(120) };
    let out = proc.run(&spec)?;
    if out.code != Some(0) {
        return Err(format!("git count-objects -v exit {:?}: {}", out.code, String::from_utf8_lossy(&out.stderr).trim()));
    }
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix("count: ")?.trim().parse().ok()).ok_or_else(|| "git count-objects -v printed no count".into())
}

/// A held `objects/maintenance.lock` makes `git maintenance run` exit 0 having done nothing. One older
/// than twice the effect's deadline has no live holder and is removed; a younger one is an error.
fn clear_stale_lock(proc: &dyn runtime::ports::Proc, path: &str) -> Result<bool, String> {
    let spec = runtime::ports::ProcSpec { prog: "git".into(), args: ["-C", path, "rev-parse", "--git-path", "objects/maintenance.lock"].map(String::from).to_vec(), stdin: vec![], deadline: std::time::Duration::from_secs(5) };
    let out = proc.run(&spec)?;
    if out.code != Some(0) {
        return Err(format!("git rev-parse exit {:?}: {}", out.code, String::from_utf8_lossy(&out.stderr).trim()));
    }
    let lock = std::path::Path::new(path).join(String::from_utf8_lossy(&out.stdout).trim());
    let Ok(meta) = std::fs::metadata(&lock) else { return Ok(false) };
    let age = meta.modified().ok().and_then(|m| m.elapsed().ok()).map_or(0, |a| a.as_secs());
    if age <= 2 * MAINTAIN_DEADLINE_MS / 1000 {
        return Err(format!("{} is held (age {age}s), so maintenance would do nothing", lock.display()));
    }
    std::fs::remove_file(&lock).map_err(|e| format!("{}: {e}", lock.display()))?;
    Ok(true)
}

/// The runner of [`MAINTAIN`]: the two lossless maintenance tasks on one checkout, reporting its
/// loose objects before and after. A failure names the repo and carries git's own stderr.
pub fn maintain(proc: &dyn runtime::ports::Proc, input: &str) -> Result<String, String> {
    let repo: Repo = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let fail = |e: String| format!("{} ({}): {e}", repo.name, repo.path);
    if !repo.error.is_empty() {
        return Err(fail(repo.error.clone()));
    }
    let before = loose_count(proc, &repo.path).map_err(fail)?;
    let cleared = clear_stale_lock(proc, &repo.path).map_err(fail)?;
    // `loose-objects` packs on one run and deletes the packed copies on the next, so a pass runs it twice.
    for tasks in [MAINTAIN_ARGS, &MAINTAIN_ARGS[..3]] {
        let mut args = vec!["-C".to_string(), repo.path.clone()];
        args.extend(tasks.iter().map(|a| a.to_string()));
        let out = proc.run(&runtime::ports::ProcSpec { prog: "git".into(), args, stdin: vec![], deadline: std::time::Duration::from_millis(MAINTAIN_DEADLINE_MS) }).map_err(fail)?;
        if out.code != Some(0) {
            return Err(fail(format!("git maintenance exit {:?}: {}", out.code, String::from_utf8_lossy(&out.stderr).trim())));
        }
    }
    let after = loose_count(proc, &repo.path).map_err(fail)?;
    Ok(format!("{}: loose objects {before} before, {after} after{}", repo.name, if cleared { " (removed a stale maintenance.lock)" } else { "" }))
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("phase"), runtime::rows::Part::Field("pending")];

crate::tasks::b::hosted!(SweepReclaim);

#[cfg(test)]
mod tests {
    use super::*;

    fn read(pairs: &[(&str, String)]) -> Pinned {
        let get = move |k: &str| Ok(pairs.iter().find(|(n, _)| *n == k).map_or("", |p| p.1.as_str()).to_string());
        Pinned::read(&|k| if k.ends_with("_GIB") || k.ends_with("_DAYS") || k.ends_with("_HOURS") { Ok("1".into()) } else { get(k) }).unwrap()
    }

    #[test_id(tm1twfc)]
    #[test]
    fn the_cache_cap_follows_the_stores_configured_root() {
        let root = testkit::TempDir::new("sweep-reclaim-store-root");
        let store = format!("{}/mnt/other/store", root.display());
        let p = read(&[("SPIRA_SCCACHE_DAV_ROOT", store.clone()), ("SPIRA_RUN", format!("{}/r", root.display()))]);
        assert_eq!(p.cache_dir.as_deref(), Some(std::path::Path::new(&store)));
    }

    #[test_id(tbkd4iq)]
    #[test]
    fn an_empty_store_root_means_the_run_dirs_sccache_dav() {
        let root = testkit::TempDir::new("sweep-reclaim-run-dir");
        let p = read(&[("SPIRA_RUN", format!("{}/r", root.display()))]);
        assert_eq!(p.cache_dir.as_deref(), Some(std::path::Path::new(&format!("{}/r/sccache-dav", root.display()))));
    }

    #[test_id(ta9slcb)]
    #[test]
    fn an_explicit_reclaim_cache_dir_still_wins() {
        let root = testkit::TempDir::new("sweep-reclaim-explicit");
        let (x, y) = (format!("{}/x", root.display()), format!("{}/y", root.display()));
        let p = read(&[("SPIRA_RECLAIM_CACHE_DIR", x.clone()), ("SPIRA_SCCACHE_DAV_ROOT", y)]);
        assert_eq!(p.cache_dir.as_deref(), Some(std::path::Path::new(&x)));
    }

    #[test_id(tfm57q4)]
    #[test]
    fn explicit_dir_wins_over_root_and_run_when_all_three_are_set() {
        let root = testkit::TempDir::new("sweep-reclaim-all-three");
        let (x, y) = (format!("{}/x", root.display()), format!("{}/y", root.display()));
        let p = read(&[("SPIRA_RECLAIM_CACHE_DIR", x.clone()), ("SPIRA_SCCACHE_DAV_ROOT", y), ("SPIRA_RUN", format!("{}/r", root.display()))]);
        assert_eq!(p.cache_dir.as_deref(), Some(std::path::Path::new(&x)));
    }

    #[test_id(tx58dpb)]
    #[test]
    fn a_relative_store_root_is_refused_not_silently_untrimmed() {
        let err = Pinned::read(&|k| Ok(match k { "SPIRA_SCCACHE_DAV_ROOT" => "store".into(), k if k.ends_with("_GIB") || k.ends_with("_DAYS") || k.ends_with("_HOURS") => "1".into(), _ => String::new() })).err().unwrap();
        assert!(err.contains("SPIRA_SCCACHE_DAV_ROOT"), "{err}");
    }

    #[test_id(tgs0mbd)]
    #[test]
    fn no_root_and_no_run_dir_trims_nothing() {
        assert_eq!(read(&[]).cache_dir, None);
    }

    fn tmp_pinned(root: &std::path::Path) -> Pinned {
        Pinned { cache_dir: None, tmp_root: Some(root.join("tmp")), tmp_idle_secs: 3_600 }
    }

    fn aged_dir(root: &std::path::Path, name: &str, secs: u64) -> std::path::PathBuf {
        let d = root.join("tmp").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("f"), vec![1u8; 8192]).unwrap();
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        for p in [d.join("f"), d.clone()] {
            std::fs::File::open(p).unwrap().set_modified(t).unwrap();
        }
        d
    }

    #[test_id(thcxpb1)]
    #[test]
    fn observe_reports_tmp_dirs_with_a_socket_dir_as_special_and_remove_tmp_refuses_what_is_not_reclaimable() {
        let t = testkit::TempDir::new("sweep-reclaim-tmp");
        let (idle, sock) = (aged_dir(t.path(), "tmp.idle", 86_400), aged_dir(t.path(), "tmp-1000", 86_400));
        let _l = std::os::unix::net::UnixListener::bind(sock.join("default")).unwrap();
        let young = aged_dir(t.path(), "tmp.young", 5);
        let outside = t.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let p = tmp_pinned(t.path());
        let proc_root = t.path().join("proc");
        std::fs::create_dir(&proc_root).unwrap();
        let snap: Snapshot = serde_json::from_str(&observe(&p, &proc_root, vec![]).unwrap()).unwrap();
        let verdict = |d: &std::path::Path| snap.tmp.iter().find(|c| c.path == d.display().to_string()).map(|c| c.verdict);
        assert_eq!((verdict(&idle), verdict(&sock), verdict(&young)), (Some(Verdict::Reclaim), Some(Verdict::Special), Some(Verdict::Fresh)));
        for refused in [&sock, &young, &outside] {
            assert!(remove_tmp(&p, &proc_root, &refused.display().to_string()).is_err(), "{refused:?}");
            assert!(refused.exists());
        }
        assert!(remove_tmp(&p, &proc_root, &idle.display().to_string()).unwrap().starts_with("reclaimed"));
        assert!(!idle.exists());
    }
}
