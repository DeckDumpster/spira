//! Owned build directories (sp-ke0uk8): each dwarf session builds in ONE cargo target it owns
//! for its whole bead, as a single developer would — change, build, change, build.
//!
//! The directories live under the bases on the compile disks (`SPIRA_DWARF_BUILD_BASE`, a
//! whitespace-separated list of directories), as `dir.<n>`, each guarded by `dir.<n>.lock`. A new
//! slot is made on the base holding the fewest slots; an existing slot stays where it is. A lease is a non-blocking exclusive `flock` on that
//! lock file, held by the dwarf process from the session's start to its teardown; the kernel
//! drops it when the holder dies, so a dead holder's directory becomes idle — never deleted —
//! with nothing to reap. Rules:
//!
//! - **Live is untouchable.** Nothing writes to or deletes a directory whose lease is live.
//!   There is no cap on a live directory and no trimming while it builds.
//! - **Warm start.** A new lease takes the most recently idled directory that holds anything
//!   (the lock file's mtime is the idle time: a release stamps it), and the shared selective clean
//!   ([`CLEAN_SCRIPT`]) then cleans only the workspace crates whose bytes differ from the tree that
//!   directory was last made consistent with. A submitted builder's release reserves its slot
//!   (`dir.<n>.gate`, naming the bead) for that bead's gate: a reserved idle slot is taken only
//!   when no unreserved one is idle, and any take or eviction removes the reservation. A release judges every crate changed since that
//!   record by unit: a `.fingerprint` unit stamped after the newest mtime and ctime of the crate's
//!   changed files is kept, any other is removed, and a crate left with no kept unit (or with a
//!   deleted file) is cleaned, before the new record is written, so the record never names bytes
//!   the artifacts were not built from. An empty directory is a new one.
//! - **Idle-only eviction.** Over the disk budget (`SPIRA_DWARF_BUILD_BUDGET_GIB`), idle
//!   directories go least recently used first, until the total is under; live ones are counted
//!   but never removed.
//!
//! - **Trim on release.** [`trim`] drops what no later build can use: idle profiles, and
//!   incremental, `deps`, `.fingerprint` and `build` entries untouched in the lease and a day old.
//!
//! Dependencies still come from sccache, which is shared, content-addressed and append-only, so
//! no lease can make another session's dependencies cold.

use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Registry key: the directories holding the owned build directories, one per compile disk, separated by whitespace.
pub const BASE_ENV: &str = "SPIRA_DWARF_BUILD_BASE";
/// Registry key: GiB the owned build directories may hold together before idle ones are evicted.
pub const BUDGET_ENV: &str = "SPIRA_DWARF_BUILD_BUDGET_GIB";
/// The shared selective clean (`spira/target-clean.sh`): the one copy of the logic, called by
/// `spira/unit-tests.sh` and by the lease path.
pub const CLEAN_SCRIPT: &str = "target-clean.sh";
/// The selective clean's deadline, in seconds as `timeout` takes them.
pub const CLEAN_DEADLINE_SECS: &str = "900";

/// A live lease on one owned build directory. Dropping it releases the directory without
/// stamping it; [`Lease::release`] is the teardown path.
pub struct Lease {
    dir: PathBuf,
    lock: std::fs::File,
    /// True when the directory held a previous session's build (a warm start).
    pub reused: bool,
    started: SystemTime,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease").field("dir", &self.dir).field("reused", &self.reused).finish()
    }
}

impl Lease {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// When the lease was taken: the cut [`trim`] measures "touched during the lease" against.
    pub fn started(&self) -> SystemTime {
        self.started
    }

    /// Trim the directory ([`trim`]), then release it to idle, stamping now as its idle time so
    /// the next lease prefers it.
    pub fn release(self) -> Trimmed {
        self.release_stamped()
    }

    /// [`Lease::release`], leaving the slot reserved for `bead`'s gate: [`lease`] takes a
    /// reserved idle slot only when no unreserved one is idle.
    pub fn release_reserved(self, bead: &str) -> Trimmed {
        let _ = std::fs::write(gate_path(self.dir.parent().unwrap_or(Path::new("")), self.slot_n()), bead);
        self.release_stamped()
    }

    fn slot_n(&self) -> u64 {
        self.dir.file_name().and_then(|f| f.to_str()).and_then(|f| f.strip_prefix("dir.")).and_then(|n| n.parse().ok()).unwrap_or(0)
    }

    fn release_stamped(self) -> Trimmed {
        let trimmed = trim(&self.dir, self.started, SystemTime::now());
        let _ = self.lock.set_len(0);
        let _ = self.lock.set_modified(SystemTime::now());
        trimmed
    }
}

/// The bases a [`BASE_ENV`] value lists.
pub fn parse_bases(value: &str) -> Vec<PathBuf> {
    value.split_whitespace().map(PathBuf::from).collect()
}

/// The configured bases; empty when [`BASE_ENV`] is unset or blank.
pub fn configured_bases() -> Vec<PathBuf> {
    crate::process::cfg(BASE_ENV).map(|v| parse_bases(&v)).unwrap_or_default()
}

fn lock_path(base: &Path, n: u64) -> PathBuf {
    base.join(format!("dir.{n}.lock"))
}

fn gate_path(base: &Path, n: u64) -> PathBuf {
    base.join(format!("dir.{n}.gate"))
}

fn dir_path(base: &Path, n: u64) -> PathBuf {
    base.join(format!("dir.{n}"))
}

/// The `n` of every `dir.<n>.lock` under `base`, ascending.
fn slots(base: &Path) -> Vec<u64> {
    let mut v: Vec<u64> = std::fs::read_dir(base)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_prefix("dir.")).and_then(|n| n.strip_suffix(".lock")).and_then(|n| n.parse().ok()))
                .collect()
        })
        .unwrap_or_default();
    v.sort_unstable();
    v
}

/// Try the slot's lock without waiting: Some(file holding it) when it was idle.
fn try_lock(base: &Path, n: u64) -> Option<std::fs::File> {
    let f = std::fs::OpenOptions::new().write(true).open(lock_path(base, n)).ok()?;
    // SAFETY: flock on a descriptor we own; the lock lives as long as `f`.
    (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(f)
}

/// When the slot last went idle: its lock file's mtime.
fn idle_since(base: &Path, n: u64) -> SystemTime {
    std::fs::metadata(lock_path(base, n)).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

fn holds_anything(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut rd| rd.next().is_some())
}

/// One scanned slot: not reserved for a gate, holds a build, idle since, index of its base, its number.
type ScannedSlot = (bool, bool, SystemTime, usize, u64);

/// The order to try idle slots in: unreserved before reserved; within each, warm ones first, most
/// recently idled first, then empty ones.
fn lease_order(mut scanned: Vec<ScannedSlot>) -> Vec<ScannedSlot> {
    scanned.sort_by(|a, b| b.cmp(a));
    scanned
}

/// Lease a build directory under `bases` for `who`: the most recently idled one that holds a
/// build, else an empty idle one (a slot reserved for a gate after both), else a new one on the base holding the fewest slots (the
/// first listed on a tie). Never waits and never touches a live one.
pub fn lease(bases: &[PathBuf], who: &str) -> std::io::Result<Lease> {
    for b in bases {
        let _ = std::fs::create_dir_all(b);
    }
    // Taking each lock in `lease_order` means the first one won is the best idle directory, whatever races us.
    let scanned: Vec<ScannedSlot> = bases
        .iter()
        .enumerate()
        .flat_map(|(i, b)| slots(b).into_iter().map(move |n| (!gate_path(b, n).exists(), holds_anything(&dir_path(b, n)), idle_since(b, n), i, n)))
        .collect();
    let order = lease_order(scanned);
    let mut won = order.iter().find_map(|&(_, warm, _, i, n)| try_lock(&bases[i], n).map(|f| (i, n, f, warm)));
    if won.is_none() {
        let mut fewest: Vec<(usize, usize)> = bases.iter().enumerate().map(|(i, b)| (slots(b).len(), i)).collect();
        fewest.sort();
        let mut last_err = None;
        for (_, i) in fewest {
            match new_slot(&bases[i]) {
                Ok((n, f)) => {
                    won = Some((i, n, f, false));
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if won.is_none() {
            return Err(last_err.unwrap_or_else(|| std::io::Error::other("no build base is configured")));
        }
    }
    let (i, n, lock, reused) = won.expect("a lock was won above");
    let base = &bases[i];
    take(base, n, lock, reused, who)
}

/// Why [`lease_dir`] did not lease the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// `dir` is no slot of a configured base.
    NotASlot,
    /// The slot's directory or lock file is gone.
    Gone,
    /// Still held after the wait; `holder` is the lock file's trimmed content, as [`take`] wrote it.
    Held { holder: String },
}

/// Lease the one named slot `dir` (`<base>/dir.<n>`, strictly under a configured base): waits at
/// most `wait` for a live holder that is `owner`, naming it on stderr; any other holder is refused at once. The lease is a warm one.
pub fn lease_dir(bases: &[PathBuf], dir: &Path, who: &str, owner: &str, wait: std::time::Duration) -> std::io::Result<Result<Lease, Refused>> {
    let Some((base, n)) = dir.parent().zip(dir.file_name().and_then(|f| f.to_str()).and_then(|f| f.strip_prefix("dir.")).and_then(|n| n.parse::<u64>().ok())) else {
        return Ok(Err(Refused::NotASlot));
    };
    if !dir.is_absolute() || !bases.iter().any(|b| b == base) {
        return Ok(Err(Refused::NotASlot));
    }
    if !dir.is_dir() || !lock_path(base, n).is_file() {
        return Ok(Err(Refused::Gone));
    }
    let holder = || std::fs::read_to_string(lock_path(base, n)).unwrap_or_default().trim().to_string();
    let until = std::time::Instant::now() + wait;
    let mut named = false;
    let lock = loop {
        if let Some(f) = try_lock(base, n) {
            break f;
        }
        let now_holder = holder();
        if std::time::Instant::now() >= until || (!now_holder.is_empty() && now_holder != owner) {
            return Ok(Err(Refused::Held { holder: now_holder }));
        }
        if !named {
            named = true;
            eprintln!("waiting up to {}s for {} to release {}", wait.as_secs(), holder(), dir.display());
        }
        std::thread::sleep(std::time::Duration::from_millis(100).min(wait));
    };
    take(base, n, lock, true, who).map(Ok)
}

fn new_slot(base: &Path) -> std::io::Result<(u64, std::fs::File)> {
    let mut n = slots(base).last().copied().unwrap_or(0) + 1;
    loop {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(lock_path(base, n)) {
            Ok(_) => {
                if let Some(f) = try_lock(base, n) {
                    return Ok((n, f));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        n += 1;
    }
}

fn take(base: &Path, n: u64, mut lock: std::fs::File, reused: bool, who: &str) -> std::io::Result<Lease> {
    use std::io::Write;
    let dir = dir_path(base, n);
    std::fs::create_dir_all(&dir)?;
    // The holder's name, for whoever reads the base; writing it stamps the lock file, so a
    // holder that dies without releasing reads as idle since it leased.
    let _ = std::fs::remove_file(gate_path(base, n));
    let _ = lock.set_len(0);
    let _ = lock.write_all(who.as_bytes());
    Ok(Lease { dir, lock, reused, started: SystemTime::now() })
}

/// Empty `dir` in place: everything under it goes, the directory stays. Only for a directory
/// the caller holds the lease on.
pub fn empty(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let _ = if e.file_type().is_ok_and(|t| t.is_dir()) { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
}

/// A file or directory untouched for this long, and not touched in the lease, is dropped on release.
pub const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
/// A profile folder nothing wrote in this long is dropped on release.
pub const PROFILE_IDLE_AFTER: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// What a release's [`trim`] measured and removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Trimmed {
    pub before: u64,
    pub after: u64,
    pub idle_profiles: u64,
    pub incremental: u64,
    pub stale: u64,
}

impl std::fmt::Display for Trimmed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} bytes before, {} after; removed {} in idle profiles, {} of stale incremental state, {} of stale deps/fingerprint/build",
            self.before, self.after, self.idle_profiles, self.incremental, self.stale
        )
    }
}

/// The newest access (unless `writes_only`) or modification time of any file under `p`.
/// Directories' own times are ignored: listing one moves its atime, and this scan lists them.
fn newest_touch(p: &Path, writes_only: bool) -> SystemTime {
    let Ok(md) = std::fs::symlink_metadata(p) else { return SystemTime::UNIX_EPOCH };
    if md.is_dir() {
        return children(p).iter().map(|c| newest_touch(c, writes_only)).max().unwrap_or(SystemTime::UNIX_EPOCH);
    }
    let w = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if writes_only { w } else { w.max(md.accessed().unwrap_or(SystemTime::UNIX_EPOCH)) }
}

fn remove_any(p: &Path) {
    let _ = if std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()) { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
}

/// Remove each of `entries` that nothing touched since `cut`; returns the bytes removed. Reads
/// count only where the filesystem records them: under `noatime` a read-only file looks untouched.
fn remove_untouched(entries: Vec<PathBuf>, cut: SystemTime) -> u64 {
    let mut bytes = 0;
    for p in entries {
        if newest_touch(&p, false) < cut {
            bytes += crate::build::size_of(&p);
            remove_any(&p);
        }
    }
    bytes
}

fn children(p: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

fn dirs(p: &Path) -> Vec<PathBuf> {
    children(p).into_iter().filter(|c| std::fs::symlink_metadata(c).is_ok_and(|m| m.is_dir())).collect()
}

/// Trim a released build directory of what no later build can use: a profile folder nothing
/// wrote in [`PROFILE_IDLE_AFTER`]; then, per remaining profile, incremental session folders and
/// `deps`/`.fingerprint`/`build` entries that neither the lease (from `lease_start`) nor the last
/// [`STALE_AFTER`] read or wrote. `now` is injected.
pub fn trim(dir: &Path, lease_start: SystemTime, now: SystemTime) -> Trimmed {
    let mut t = Trimmed { before: crate::build::size_of(dir), ..Trimmed::default() };
    let stale_cut = lease_start.min(now - STALE_AFTER);
    let profiles = dirs(dir).into_iter().filter(|d| ["deps", ".fingerprint", "build", "incremental"].iter().any(|s| d.join(s).is_dir()));
    for profile in profiles {
        if newest_touch(&profile, true) < now - PROFILE_IDLE_AFTER {
            t.idle_profiles += crate::build::size_of(&profile);
            remove_any(&profile);
            continue;
        }
        let sessions = dirs(&profile.join("incremental")).iter().flat_map(|c| children(c)).collect();
        t.incremental += remove_untouched(sessions, stale_cut);
        for sub in ["deps", ".fingerprint", "build"] {
            t.stale += remove_untouched(children(&profile.join(sub)), stale_cut);
        }
    }
    t.after = crate::build::size_of(dir);
    t
}

/// Evict idle directories under `bases`, least recently used first, until everything under the
/// bases together holds at most `budget_bytes`. A live slot is measured but never touched; a
/// directory that is not a slot goes by the age of its newest write unless a live process stands in it.
/// Returns the directories removed, in the order they went. Bytes are each directory's own
/// blocks ([`crate::build::size_of`]); the filesystem's fullness is never read.
pub fn evict_idle(bases: &[PathBuf], budget_bytes: u64) -> Vec<PathBuf> {
    evict_idle_with(bases, budget_bytes, &crate::build::size_of)
}

/// [`evict_idle`] with the measurement injected: `size` answers the bytes one directory holds.
pub fn evict_idle_with(bases: &[PathBuf], budget_bytes: u64, size: &dyn Fn(&Path) -> u64) -> Vec<PathBuf> {
    evict_idle_in(bases, budget_bytes, size, Path::new(system_paths::PROC))
}

/// [`evict_idle_with`] with the process table's root injected.
pub fn evict_idle_in(bases: &[PathBuf], budget_bytes: u64, size: &dyn Fn(&Path) -> u64, proc_root: &Path) -> Vec<PathBuf> {
    let mut sized = Vec::new();
    for (i, b) in bases.iter().enumerate() {
        let slot_dirs: Vec<PathBuf> = slots(b).into_iter().map(|n| dir_path(b, n)).collect();
        for n in slots(b) {
            let path = dir_path(b, n);
            sized.push((Kind::Slot(i, n), size(&path), path));
        }
        for path in dirs(b).into_iter().filter(|p| !slot_dirs.contains(p)) {
            sized.push((Kind::Stray, size(&path), path));
        }
    }
    if sized.iter().map(|(_, bytes, _)| bytes).sum::<u64>() <= budget_bytes {
        return Vec::new();
    }
    let candidates = sized
        .into_iter()
        .filter(|(_, bytes, _)| *bytes > 0)
        .map(|(kind, bytes, path)| {
            let age = match kind {
                Kind::Slot(i, n) => idle_since(&bases[i], n),
                Kind::Stray => newest_touch(&path, true),
            };
            Candidate { path, kind, bytes, age }
        })
        .collect();
    evict_walk(candidates, budget_bytes, &mut |c| match c.kind {
        // Under the slot's own lock: a directory a session takes meanwhile is skipped, and one
        // evicted here is an empty directory the next lease takes last.
        Kind::Slot(i, n) => {
            let Some(_held) = try_lock(&bases[i], n) else { return false };
            empty(&c.path);
            let _ = std::fs::remove_file(gate_path(&bases[i], n));
            true
        }
        // Read at the removal, not before the age scan: a process may have started standing here since.
        Kind::Stray => {
            if stood_in(&c.path, &held_by_processes(proc_root)) {
                return false;
            }
            remove_any(&c.path);
            true
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Slot(usize, u64),
    Stray,
}

/// One directory under a base that eviction may remove; `age` is when it was last used.
#[derive(Debug, Clone)]
struct Candidate {
    path: PathBuf,
    kind: Kind,
    bytes: u64,
    age: SystemTime,
}

/// The candidates in the order eviction tries them: empty ones dropped, oldest first, ties by
/// path; none at all while the total is within budget.
fn eviction_order(candidates: &[Candidate], budget_bytes: u64) -> Vec<&Candidate> {
    if candidates.iter().map(|c| c.bytes).sum::<u64>() <= budget_bytes {
        return Vec::new();
    }
    let mut order: Vec<&Candidate> = candidates.iter().filter(|c| c.bytes > 0).collect();
    order.sort_by(|a, b| (a.age, &a.path).cmp(&(b.age, &b.path)));
    order
}

/// Walk [`eviction_order`] until the total is within budget. `remove` tries one removal and says
/// whether it happened; a refused candidate stays counted. Returns the paths removed, in order.
fn evict_walk(candidates: Vec<Candidate>, budget_bytes: u64, remove: &mut dyn FnMut(&Candidate) -> bool) -> Vec<PathBuf> {
    let mut total: u64 = candidates.iter().map(|c| c.bytes).sum();
    let mut gone = Vec::new();
    for c in eviction_order(&candidates, budget_bytes) {
        if total <= budget_bytes {
            break;
        }
        if remove(c) {
            total = total.saturating_sub(c.bytes);
            gone.push(c.path.clone());
        }
    }
    gone
}

/// Whether any of the paths a process holds lies inside `dir`.
fn stood_in(dir: &Path, held: &[PathBuf]) -> bool {
    held.iter().any(|h| h.starts_with(dir))
}

/// One gauge the pool reports; `dir` labels the per-directory series.
#[derive(Debug, PartialEq)]
pub struct Sample {
    pub name: &'static str,
    pub value: f64,
    pub dir: Option<String>,
}

fn sample(name: &'static str, value: u64, dir: Option<String>) -> Sample {
    Sample { name, value: value as f64, dir }
}

/// What a lease or a release of `dir` reports: its own bytes, the pool the budget counts (every
/// `dir.<n>`), the base total (everything under each base, including what no budget counts) and
/// the budget itself. `size` answers the bytes one path holds.
pub fn lease_samples(bases: &[PathBuf], dir: &Path, budget_bytes: u64, size: &dyn Fn(&Path) -> u64) -> Vec<Sample> {
    let pool: u64 = bases.iter().flat_map(|b| slots(b).into_iter().map(move |n| dir_path(b, n))).map(|d| size(&d)).sum();
    let base: u64 = bases.iter().map(|b| size(b)).sum();
    vec![
        sample("build_dir_bytes", size(dir), Some(dir.display().to_string())),
        sample("build_dir_pool_bytes", pool, None),
        sample("build_dir_base_bytes", base, None),
        sample("build_dir_budget_bytes", budget_bytes, None),
    ]
}

/// One event per directory [`evict_idle`] removed.
pub fn eviction_samples(gone: &[PathBuf]) -> Vec<Sample> {
    gone.iter().map(|d| sample("build_dir_evicted", 1, Some(d.display().to_string()))).collect()
}

/// Push `samples` as gauges; a failed push is counted by the sink, never returned.
pub fn push(sink: &spira_otel::metrics::MetricsSink, samples: &[Sample]) {
    let at = SystemTime::now();
    let series: Vec<spira_otel::metrics::Series> = samples
        .iter()
        .map(|s| spira_otel::metrics::Series { name: s.name.into(), value: s.value, labels: s.dir.iter().map(|d| ("dir".to_string(), d.clone())).collect(), at })
        .collect();
    if !series.is_empty() {
        let _ = sink.try_push_series(&series);
    }
}

/// Measure the pool on disk and push it: called after a lease and after a release.
pub fn meter(bases: &[PathBuf], dir: &Path, budget_bytes: u64) {
    let sink = crate::process::metrics_sink("build-dir");
    push(&sink, &lease_samples(bases, dir, budget_bytes, &crate::build::size_of));
}

/// Push one event per directory evicted.
pub fn meter_evictions(gone: &[PathBuf]) {
    push(&crate::process::metrics_sink("build-dir"), &eviction_samples(gone));
}

/// Where every live process stands: its cwd and the `CARGO_TARGET_DIR` it was handed.
fn held_by_processes(proc_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pid in children(proc_root).into_iter().filter(|p| p.file_name().is_some_and(|n| n.as_encoded_bytes().iter().all(u8::is_ascii_digit))) {
        out.extend(std::fs::read_link(pid.join("cwd")));
        for kv in std::fs::read(pid.join("environ")).unwrap_or_default().split(|b| *b == 0) {
            if let Some(v) = kv.strip_prefix(b"CARGO_TARGET_DIR=") {
                out.push(PathBuf::from(String::from_utf8_lossy(v).into_owned()));
            }
        }
    }
    out
}

/// The owned build directory a caller's `CARGO_TARGET_DIR` names, when it is one: an absolute
/// directory strictly under one of `bases`. Any other value is ignored, so an inherited override can
/// never redirect a build elsewhere.
pub fn owned(var: Option<std::ffi::OsString>, bases: &[PathBuf]) -> Option<PathBuf> {
    let p = PathBuf::from(var?);
    let ours = p.is_absolute() && p.is_dir() && bases.iter().any(|b| p != *b && p.starts_with(b));
    ours.then_some(p)
}

const BUILT_RECORD: &str = ".unit-tests-built-manifest";

/// Run the shared selective clean (`script`, [`CLEAN_SCRIPT`]) for the tree at `root` against
/// the build directory `dir`: only the workspace crates whose bytes differ from the tree the
/// directory was last made consistent with are cleaned, and `root`'s manifest is recorded.
/// `release` (a lease's end) also cleans every crate with a file modified since the record. `path` is the PATH cargo is
/// found on. Ok(the script's output) or Err(why).
pub fn clean(script: &Path, root: &Path, dir: &Path, path: &str, release: bool) -> Result<String, String> {
    let r = run_clean(script, root, dir, path, release);
    if release && r.is_err() {
        let _ = std::fs::remove_file(dir.join(BUILT_RECORD));
    }
    r
}

fn run_clean(script: &Path, root: &Path, dir: &Path, path: &str, release: bool) -> Result<String, String> {
    // A whole-workspace `cargo clean` of a large target is slow: bounded by a deadline of its own.
    let mut cmd = crate::bounded::bounded_for("bash", CLEAN_DEADLINE_SECS);
    cmd.arg(script);
    if release {
        cmd.arg("--release");
    }
    let out = cmd
        .arg(root)
        .current_dir(root)
        .env("CARGO_TARGET_DIR", dir)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", script.display()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!("{} exited {}: {}", script.display(), out.status, text.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(p: &Path, kib: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![1u8; kib * 1024]).unwrap();
    }

    fn touch(p: &Path, accessed_ago: u64, modified_ago: u64) {
        fill(p, 1);
        let ago = |s: u64| SystemTime::now() - std::time::Duration::from_secs(s);
        let times = std::fs::FileTimes::new().set_accessed(ago(accessed_ago)).set_modified(ago(modified_ago));
        std::fs::File::options().write(true).open(p).unwrap().set_times(times).unwrap();
    }

    const H: u64 = 3600;

    #[test_id(taihmvp)]
    #[test]
    fn trim_drops_incremental_session_folders_the_lease_did_not_touch_and_a_day_old() {
        let d = testkit::TempDir::new("bdir-trim-inc");
        let inc = d.path().join("debug/incremental/crate_a-1");
        touch(&inc.join("s-old/dep-graph.bin"), 30 * H, 30 * H);
        touch(&inc.join("s-fresh/dep-graph.bin"), 30 * H, 30 * H);
        touch(&inc.join("s-read/dep-graph.bin"), 30 * H, 30 * H);
        touch(&inc.join("s-recent/dep-graph.bin"), 2 * H, 2 * H);
        touch(&d.path().join("debug/deps/anchor"), 0, 0);
        let lease_start = SystemTime::now() - std::time::Duration::from_secs(5 * H);
        touch(&inc.join("s-fresh/query-cache.bin"), 30 * H, H);
        touch(&inc.join("s-read/query-cache.bin"), H, 30 * H);
        let t = trim(d.path(), lease_start, SystemTime::now());
        assert!(!inc.join("s-old").exists(), "untouched in the lease and over 24 hours old goes");
        assert!(inc.join("s-fresh").exists(), "written in the lease stays");
        assert!(inc.join("s-read").exists(), "read in the lease stays");
        assert!(inc.join("s-recent").exists(), "untouched but under 24 hours old stays");
        // clock-safe: trim ages files against the real now; fixture mtimes are set relative to it
        assert!(t.incremental > 0 && t.stale == 0 && t.idle_profiles == 0 && t.after < t.before, "{t:?}");
    }

    #[test_id(tmdikj5)]
    #[test]
    fn trim_drops_deps_fingerprint_and_build_entries_untouched_in_the_lease_and_a_day_old() {
        let d = testkit::TempDir::new("bdir-trim-deps");
        let p = d.path().join("debug");
        touch(&p.join("anchor"), 0, 0);
        for sub in ["deps/old.rlib", ".fingerprint/old-1/lib-old", "build/old-1/out"] {
            touch(&p.join(sub), 30 * H, 30 * H);
        }
        touch(&p.join("deps/used.rlib"), H, 30 * H);
        touch(&p.join("deps/written.rlib"), 30 * H, H);
        touch(&p.join("deps/newish.rlib"), 3 * H, 3 * H);
        let lease_start = SystemTime::now() - std::time::Duration::from_secs(5 * H);
        let t = trim(d.path(), lease_start, SystemTime::now());
        for gone in ["deps/old.rlib", ".fingerprint/old-1", "build/old-1"] {
            assert!(!p.join(gone).exists(), "{gone}");
        }
        for kept in ["deps/used.rlib", "deps/written.rlib", "deps/newish.rlib"] {
            assert!(p.join(kept).exists(), "{kept}");
        }
        // clock-safe: trim ages files against the real now; fixture mtimes are set relative to it
        assert!(t.stale > 0 && t.incremental == 0, "{t:?}");
    }

    #[test_id(ty9ixba)]
    #[test]
    fn trim_drops_a_profile_nothing_wrote_in_six_hours_and_keeps_a_fresh_one() {
        let d = testkit::TempDir::new("bdir-trim-profile");
        touch(&d.path().join("unittest/deps/x"), 8 * H, 8 * H);
        touch(&d.path().join("debug/deps/y"), 8 * H, 2 * H);
        touch(&d.path().join(".unit-tests-built-manifest"), 8 * H, 8 * H);
        touch(&d.path().join("scratch/note"), 8 * H, 8 * H);
        let t = trim(d.path(), SystemTime::now(), SystemTime::now());
        assert!(!d.path().join("unittest").exists(), "idle over 6 hours goes");
        assert!(d.path().join("debug/deps/y").exists(), "written within 6 hours stays");
        assert!(d.path().join(".unit-tests-built-manifest").exists(), "files beside the profiles are not profiles");
        assert!(d.path().join("scratch/note").exists(), "a directory that is no profile is not trimmed");
        // clock-safe: trim ages files against the real now; fixture mtimes are set relative to it
        assert!(t.idle_profiles > 0, "{t:?}");
    }

    #[test_id(t9k0vio)]
    #[test]
    fn trimmed_display_names_bytes_before_after_and_each_step() {
        let t = Trimmed { before: 900, after: 100, idle_profiles: 5, incremental: 7, stale: 11 };
        let line = t.to_string();
        for want in ["900 bytes before", "100 after", "5 in idle profiles", "7 of stale incremental", "11 of stale deps"] {
            assert!(line.contains(want), "{line}");
        }
        assert!(!line.contains('\n'));
    }

    #[test_id(twym2l8)]
    #[test]
    fn trim_drops_an_idle_profile_even_if_the_lease_only_read_it() {
        let d = testkit::TempDir::new("bdir-trim-read-profile");
        touch(&d.path().join("unittest/deps/x"), 0, 8 * H);
        trim(d.path(), SystemTime::now() - std::time::Duration::from_secs(H), SystemTime::now());
        assert!(!d.path().join("unittest").exists(), "the idle test is on writes: a read does not keep a profile");
    }

    #[test_id(t50qbqp)]
    #[test]
    fn trim_keeps_what_a_lease_older_than_a_day_touched() {
        let d = testkit::TempDir::new("bdir-trim-cut");
        let p = d.path().join("debug");
        touch(&p.join("anchor"), 0, 0);
        touch(&p.join("deps/in_lease.rlib"), 30 * H, 30 * H);
        touch(&p.join("deps/before_lease.rlib"), 60 * H, 60 * H);
        let lease_start = SystemTime::now() - std::time::Duration::from_secs(40 * H);
        trim(d.path(), lease_start, SystemTime::now());
        assert!(p.join("deps/in_lease.rlib").exists(), "touched after the lease began stays, though over 24 hours old");
        assert!(!p.join("deps/before_lease.rlib").exists());
    }

    #[test_id(t9qxmkm)]
    #[test]
    fn releasing_a_lease_trims_its_directory() {
        let d = testkit::TempDir::new("bdir-release-trim");
        let base = d.path().join("base");
        let l = lease(std::slice::from_ref(&base), "sp-a").unwrap();
        touch(&l.dir().join("debug/anchor"), 0, 0);
        touch(&l.dir().join("debug/deps/old.rlib"), 30 * H, 30 * H);
        let t = l.release();
        assert!(!base.join("dir.1/debug/deps/old.rlib").exists());
        assert!(t.stale > 0, "{t:?}");
    }

    #[test_id(twlh7hs)]
    #[test]
    fn a_second_lease_while_one_is_live_never_touches_the_live_directory() {
        let held = |p: &Path| u64::from(holds_anything(p)) * 100;
        let d = testkit::TempDir::new("bdir-live");
        let base = d.path().join("base");
        let a = lease(std::slice::from_ref(&base), "sp-a").expect("first lease");
        fill(&a.dir().join("debug/deps/liba.rlib"), 64);
        let b = lease(std::slice::from_ref(&base), "sp-b").expect("second lease");
        assert_ne!(a.dir(), b.dir(), "a live directory is never handed out twice");
        assert!(a.dir().join("debug/deps/liba.rlib").exists());
        let proc = empty_proc(d.path());
        assert!(evict_idle_in(std::slice::from_ref(&base), 0, &held, &proc).is_empty(), "nothing idle: a budget of zero removes nothing live");
        assert!(a.dir().join("debug/deps/liba.rlib").exists(), "the live directory is untouched under budget pressure");
    }

    #[test_id(t53fx6v)]
    #[test]
    fn lease_dir_takes_an_idle_named_slot_and_a_released_one_is_lockable_again() {
        let d = testkit::TempDir::new("bdir-lease-dir");
        let bases = vec![d.path().join("base")];
        let first = lease(&bases, "builder").unwrap();
        let dir = first.dir().to_path_buf();
        first.release();
        let got = lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::ZERO).unwrap().expect("idle slot is leased");
        assert_eq!(got.dir(), dir);
        assert!(got.reused);
        got.release();
        assert!(lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::ZERO).unwrap().is_ok());
    }

    #[test_id(t0ujgqv)]
    #[test]
    fn lease_dir_gives_up_on_a_held_slot_and_takes_it_once_its_holder_releases() {
        let d = testkit::TempDir::new("bdir-lease-dir-held");
        let bases = vec![d.path().join("base")];
        let held = lease(&bases, "builder").unwrap();
        let dir = held.dir().to_path_buf();
        assert!(lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::ZERO).unwrap().is_err());
        assert!(lease_dir(&bases, &dir, "gate", "builder", std::time::Duration::from_millis(150)).unwrap().is_err(), "held for the whole wait");
        let waiter = {
            let (b, d) = (bases.clone(), dir.clone());
            std::thread::spawn(move || lease_dir(&b, &d, "gate", "builder", std::time::Duration::from_secs(10)).unwrap().is_ok())
        };
        held.release();
        assert!(waiter.join().unwrap(), "the waiter took the slot once its holder released");
    }

    #[test_id(tplace1)]
    #[test]
    fn a_slot_reserved_for_a_gate_is_leased_after_an_older_warm_one_and_the_take_clears_it() {
        let d = testkit::TempDir::new("bdir-reserved");
        let bases = vec![d.path().join("base")];
        let (one, two) = (lease(&bases, "a").unwrap(), lease(&bases, "b").unwrap());
        let (d1, d2) = (one.dir().to_path_buf(), two.dir().to_path_buf());
        fill(&d1.join("x"), 8);
        fill(&d2.join("x"), 8);
        two.release();
        one.release_reserved("sp-a");
        let older = std::fs::OpenOptions::new().write(true).open(lock_path(&bases[0], 2)).unwrap();
        older.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1)).unwrap();
        let got = lease(&bases, "c").unwrap();
        assert_eq!(got.dir(), d2);
        assert!(gate_path(&bases[0], 1).exists());
        drop(got);
        let got = lease(&bases, "c").unwrap();
        assert_eq!(got.dir(), d2, "still idle and unreserved");
        let _held = got;
        let only = lease(&bases, "d").unwrap();
        assert_eq!(only.dir(), d1, "a reserved slot is taken when nothing else is idle");
        assert!(!gate_path(&bases[0], 1).exists());
    }

    #[test_id(tkjsqrr)]
    #[test]
    fn an_empty_unreserved_slot_is_leased_before_a_warm_reserved_one() {
        let d = testkit::TempDir::new("bdir-reserved-empty");
        let bases = vec![d.path().join("base")];
        let (one, two) = (lease(&bases, "a").unwrap(), lease(&bases, "b").unwrap());
        let (d1, d2) = (one.dir().to_path_buf(), two.dir().to_path_buf());
        fill(&d1.join("x"), 8);
        two.release();
        one.release_reserved("sp-a");
        assert_eq!(lease(&bases, "c").unwrap().dir(), d2);
    }

    #[test_id(tplace2)]
    #[test]
    fn lease_dir_refuses_a_slot_held_by_another_session_at_once() {
        let d = testkit::TempDir::new("bdir-not-owner");
        let bases = vec![d.path().join("base")];
        let held = lease(&bases, "sp-b").unwrap();
        let (b, dir) = (bases.clone(), held.dir().to_path_buf());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(lease_dir(&b, &dir, "gate", "sp-a", std::time::Duration::from_secs(3600)).unwrap().err()));
        // clock-safe: the cap only bounds a wait for the refusal, which arrives at once
        let refused = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("a slot another session holds is refused without waiting");
        assert_eq!(refused, Some(Refused::Held { holder: "sp-b".into() }));
    }

    #[test_id(tplace3)]
    #[test]
    fn evicting_a_reserved_idle_slot_removes_its_reservation() {
        let d = testkit::TempDir::new("bdir-evict-reserved");
        let base = d.path().join("base");
        let l = lease(std::slice::from_ref(&base), "sp-a").unwrap();
        fill(&l.dir().join("x"), 64);
        l.release_reserved("sp-a");
        assert!(gate_path(&base, 1).exists());
        evict_idle_in(std::slice::from_ref(&base), 0, &crate::build::size_of, &empty_proc(d.path()));
        assert!(!gate_path(&base, 1).exists());
    }

    #[test_id(t0dlx6z)]
    #[test]
    fn lease_dir_names_the_holder_of_a_held_slot_and_a_missing_slot_as_gone() {
        let d = testkit::TempDir::new("bdir-lease-dir-why");
        let bases = vec![d.path().join("base")];
        let held = lease(&bases, "sp-other").unwrap();
        let dir = held.dir().to_path_buf();
        let why = |p: &Path| lease_dir(&bases, p, "gate", "", std::time::Duration::ZERO).unwrap().err();
        assert_eq!(why(&dir), Some(Refused::Held { holder: "sp-other".into() }));
        assert_eq!(why(&bases[0].join("dir.99")), Some(Refused::Gone));
        assert_eq!(why(&d.path().join("elsewhere/dir.1")), Some(Refused::NotASlot));
    }

    #[test_id(tha2k55)]
    #[test]
    fn lease_dir_refuses_a_path_outside_the_bases_a_missing_slot_and_a_non_slot_name() {
        let d = testkit::TempDir::new("bdir-lease-dir-refused");
        let base = d.path().join("base");
        let bases = vec![base.clone()];
        let l = lease(&bases, "builder").unwrap();
        let dir = l.dir().to_path_buf();
        l.release();
        let none = |p: &Path| lease_dir(&bases, p, "gate", "", std::time::Duration::ZERO).unwrap().is_err();
        let outside = d.path().join("elsewhere/dir.1");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(d.path().join("elsewhere/dir.1.lock"), "").unwrap();
        assert!(none(&outside));
        assert!(none(&base.join("dir.99")), "no such slot");
        assert!(none(&base), "the base is no slot");
        assert!(none(&dir.join("debug")), "a path inside a slot is not a slot");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(none(&dir), "a slot whose directory is gone");
    }

    #[test_id(t3qpn9j)]
    #[test]
    fn only_an_absolute_directory_under_the_base_is_an_owned_build_directory() {
        let d = testkit::TempDir::new("bdir-owned");
        let base = d.path().join("base");
        std::fs::create_dir_all(base.join("dir.1")).unwrap();
        let some = |p: &Path| Some(p.as_os_str().to_os_string());
        assert_eq!(owned(some(&base.join("dir.1")), std::slice::from_ref(&base)), Some(base.join("dir.1")));
        assert_eq!(owned(some(&base), std::slice::from_ref(&base)), None, "the base itself is no directory of its own");
        assert_eq!(owned(some(&d.path().join("elsewhere")), std::slice::from_ref(&base)), None);
        assert_eq!(owned(some(&base.join("dir.1")), &[]), None, "no base configured: nothing is owned");
        assert_eq!(owned(Some("rel/dir.1".into()), &[PathBuf::from("rel")]), None);
    }

    #[test_id(tp83kj5)]
    #[test]
    fn four_new_slots_over_two_bases_are_two_on_each() {
        let d = testkit::TempDir::new("bdir-two-bases");
        let bases = [d.path().join("b"), d.path().join("c")];
        let held: Vec<Lease> = (0..4).map(|i| lease(&bases, &format!("sp-{i}")).unwrap()).collect();
        for b in &bases {
            assert_eq!(slots(b).len(), 2, "{}", b.display());
        }
        assert_eq!(held.len(), 4);
    }

    #[test_id(td88s3z)]
    #[test]
    fn a_new_slot_goes_to_the_base_with_fewer_slots_not_the_first_listed() {
        let d = testkit::TempDir::new("bdir-uneven");
        let bases = [d.path().join("b"), d.path().join("c")];
        let _crowded: Vec<Lease> = (0..3).map(|i| lease(&bases[..1], &format!("sp-{i}")).unwrap()).collect();
        assert_eq!(lease(&bases, "sp-new").unwrap().dir().parent(), Some(bases[1].as_path()));
    }

    #[test_id(tieniqv)]
    #[test]
    fn a_base_that_cannot_hold_a_slot_is_skipped_for_the_next() {
        let d = testkit::TempDir::new("bdir-unusable");
        let file = d.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let bases = [file.join("b"), d.path().join("c")];
        assert_eq!(lease(&bases, "sp-a").unwrap().dir().parent(), Some(bases[1].as_path()));
        assert!(lease(&bases[..1], "sp-b").is_err(), "positive control: with only the unusable base there is no slot");
    }

    fn value(s: &[Sample], name: &str, dir: Option<&str>) -> Option<f64> {
        s.iter().find(|x| x.name == name && x.dir.as_deref() == dir).map(|x| x.value)
    }

    #[test_id(tgzkyg9)]
    #[test]
    fn a_lease_reports_its_directory_the_pool_the_budget_counts_and_everything_under_the_base() {
        let d = testkit::TempDir::new("bdir-meter");
        let base = d.path().join("base");
        let leases: Vec<Lease> = (0..2).map(|i| lease(std::slice::from_ref(&base), &format!("sp-{i}")).unwrap()).collect();
        std::fs::create_dir_all(base.join("orphan")).unwrap();
        let (a, b) = (leases[0].dir().to_path_buf(), leases[1].dir().to_path_buf());
        let (orphan, whole) = (base.join("orphan"), base.clone());
        let size = move |p: &Path| match p {
            p if p == a => 100,
            p if p == b => 20,
            p if p == orphan => 3000,
            p if p == whole => 3120,
            _ => 0,
        };
        let s = lease_samples(std::slice::from_ref(&base), leases[0].dir(), 777, &size);
        let n = leases[0].dir().display().to_string();
        assert_eq!(value(&s, "build_dir_bytes", Some(&n)), Some(100.0));
        assert_eq!(value(&s, "build_dir_pool_bytes", None), Some(120.0), "the pool is the directories the budget counts");
        assert_eq!(value(&s, "build_dir_base_bytes", None), Some(3120.0), "the base total includes what the budget does not count");
        assert_eq!(value(&s, "build_dir_budget_bytes", None), Some(777.0));
    }

    #[test_id(tba5obp)]
    #[test]
    fn pushed_samples_reach_the_endpoint_with_their_names_and_the_dir_label() {
        let (addr, h) = spira_otel::metrics::run::fixture_sink();
        let sink = spira_otel::metrics::MetricsSink::new(Some(&addr), "build-dir", std::time::Duration::from_secs(2));
        push(&sink, &eviction_samples(&[PathBuf::from("x/dir.7")]));
        let body = h.join().unwrap();
        for want in ["build_dir_evicted", "dir", "x/dir.7", "build-dir"] {
            assert!(body.contains(want), "{want} missing from the push");
        }
    }

    fn empty_proc(d: &Path) -> PathBuf {
        let proc = d.join("empty-proc");
        std::fs::create_dir_all(&proc).unwrap();
        proc
    }

    fn proc_naming(d: &Path, pid: u32, target: &Path) -> PathBuf {
        let proc = d.join("proc");
        std::fs::create_dir_all(proc.join(pid.to_string())).unwrap();
        std::fs::write(proc.join(pid.to_string()).join("environ"), format!("A=b\0CARGO_TARGET_DIR={}\0", target.display())).unwrap();
        proc
    }

    #[test_id(t4fy3pn)]
    #[test]
    fn eviction_reclaims_a_non_slot_directory_by_age_and_spares_a_live_slot_and_a_named_one() {
        let d = testkit::TempDir::new("bdir-nonslot");
        let base = d.path().join("base");
        let live = lease(std::slice::from_ref(&base), "sp-live").unwrap();
        fill(&live.dir().join("debug/x"), 1);
        let stale = base.join("hand.1");
        let named = base.join("hand.2");
        for p in [&stale, &named] {
            fill(&p.join("debug/x"), 1);
            touch(&p.join("debug/x"), 4 * 86400, 4 * 86400);
        }
        let held = |p: &Path| if p.join("debug/x").exists() || holds_anything(p) { 100 } else { 0 };
        let proc = proc_naming(d.path(), 4242, &named.join("debug"));
        let gone = evict_idle_in(std::slice::from_ref(&base), 250, &held, &proc);
        assert_eq!(gone, vec![stale.clone()], "the unreferenced non-slot directory goes, counted against the budget");
        assert!(!stale.exists());
        assert!(named.join("debug/x").exists(), "a directory a live process's CARGO_TARGET_DIR names stays");
        assert!(live.dir().join("debug/x").exists(), "a live slot is left alone");
    }

    #[test_id(txstu49)]
    #[test]
    fn the_public_eviction_counts_a_non_slot_directory_against_the_budget() {
        let d = testkit::TempDir::new("bdir-nonslot-public");
        let base = d.path().join("base");
        let stray = base.join("hand.1");
        touch(&stray.join("x"), 3 * 86400, 3 * 86400);
        let held = |p: &Path| u64::from(holds_anything(p)) * 100;
        assert_eq!(evict_idle_in(std::slice::from_ref(&base), 0, &held, &empty_proc(d.path())), vec![stray]);
    }

    #[test_id(t9owcz6)]
    #[test]
    fn the_real_eviction_measures_bytes_on_disk_and_reclaims_a_stray_beside_a_live_slot() {
        let d = testkit::TempDir::new("bdir-nonslot-real");
        let base = d.path().join("base");
        let live = lease(std::slice::from_ref(&base), "sp-live").unwrap();
        fill(&live.dir().join("debug/x"), 64);
        let stray = base.join("hand.1");
        fill(&stray.join("debug/x"), 64);
        touch(&stray.join("debug/x"), 3 * 86400, 3 * 86400);
        let budget = crate::build::size_of(live.dir()) + 1024;
        assert_eq!(evict_idle_in(std::slice::from_ref(&base), budget, &crate::build::size_of, &empty_proc(d.path())), vec![stray.clone()]);
        assert!(!stray.exists());
        assert!(live.dir().join("debug/x").exists());
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    const DAY: u64 = 86400;

    fn slot(base: usize, n: u64, bytes: u64, age_days: u64) -> Candidate {
        Candidate { path: PathBuf::from(format!("b{base}/dir.{n}")), kind: Kind::Slot(base, n), bytes, age: at(100 * DAY - age_days * DAY) }
    }

    fn stray(name: &str, bytes: u64, age_days: u64) -> Candidate {
        Candidate { path: PathBuf::from(format!("b0/{name}")), kind: Kind::Stray, bytes, age: at(100 * DAY - age_days * DAY) }
    }

    fn numbers(order: &[ScannedSlot]) -> Vec<u64> {
        order.iter().map(|s| s.4).collect()
    }

    #[test_id(t2q7yd4)]
    #[test]
    fn the_most_recently_idled_warm_slot_is_tried_first_then_the_rest_then_the_empty_ones() {
        let scanned = vec![(true, false, at(100), 0, 1), (true, true, at(10), 0, 2), (true, true, at(50), 1, 3), (true, false, at(200), 0, 4)];
        assert_eq!(numbers(&lease_order(scanned)), [3, 2, 4, 1]);
    }

    #[test_id(twxgvig)]
    #[test]
    fn a_single_idle_warm_slot_is_first_in_the_lease_order_on_its_own_base() {
        let order = lease_order(vec![(true, true, at(5), 1, 7)]);
        assert_eq!(order.first().map(|s| (s.3, s.4)), Some((1, 7)));
    }

    #[test_id(t5z21a3)]
    #[test]
    fn a_budget_of_zero_with_one_idle_directory_removes_it_and_reports_one_event_naming_it() {
        let gone = evict_walk(vec![slot(0, 1, 100, 3)], 0, &mut |_| true);
        assert_eq!(gone, [PathBuf::from("b0/dir.1")]);
        assert_eq!(eviction_samples(&gone), [Sample { name: "build_dir_evicted", value: 1.0, dir: Some("b0/dir.1".into()) }]);
    }

    #[test_id(txutbsf)]
    #[test]
    fn strays_are_ranked_with_idle_slots_oldest_first() {
        let cs = vec![slot(0, 2, 10, 1), stray("hand.1", 10, 3), slot(0, 1, 10, 5)];
        let gone = evict_walk(cs, 0, &mut |_| true);
        assert_eq!(gone, [PathBuf::from("b0/dir.1"), PathBuf::from("b0/hand.1"), PathBuf::from("b0/dir.2")]);
    }

    #[test_id(t20fuzg)]
    #[test]
    fn a_stray_with_a_process_standing_in_it_is_spared_and_the_others_go() {
        let held = [PathBuf::from("b0/hand.1/debug")];
        let cs = vec![slot(0, 2, 10, 1), stray("hand.1", 10, 3), slot(0, 1, 10, 5)];
        let gone = evict_walk(cs, 0, &mut |c| !(c.kind == Kind::Stray && stood_in(&c.path, &held)));
        assert_eq!(gone, [PathBuf::from("b0/dir.1"), PathBuf::from("b0/dir.2")]);
    }

    #[test_id(tgzg11a)]
    #[test]
    fn a_directory_is_stood_in_by_its_own_path_and_anything_beneath_it_not_a_sibling_prefix() {
        let held = [PathBuf::from("b0/hand.1/debug"), PathBuf::from("b0/hand.20")];
        assert!(stood_in(Path::new("b0/hand.1"), &held));
        assert!(stood_in(Path::new("b0/hand.20"), &held));
        assert!(!stood_in(Path::new("b0/hand.2"), &held));
        assert!(!stood_in(Path::new("b0/hand.1"), &[]));
    }

    #[test_id(t7k5pd5)]
    #[test]
    fn a_process_cwd_read_from_a_proc_directory_marks_the_stray_it_stands_in() {
        let d = testkit::TempDir::new("bdir-proc-cwd");
        let proc = d.path().join("proc");
        std::fs::create_dir_all(proc.join("77")).unwrap();
        std::fs::create_dir_all(proc.join("self-ish")).unwrap();
        std::os::unix::fs::symlink("b0/hand.1/debug", proc.join("77/cwd")).unwrap();
        let held = held_by_processes(&proc);
        assert_eq!(held, [PathBuf::from("b0/hand.1/debug")]);
        assert!(stood_in(Path::new("b0/hand.1"), &held));
        assert!(!stood_in(Path::new("b0/hand.2"), &held));
    }

    #[test_id(tmg4e8j)]
    #[test]
    fn eviction_removes_least_recently_used_first_never_a_refused_live_one_and_stops_once_under_budget() {
        let cs = vec![slot(0, 4, 100, 1), slot(0, 1, 100, 9), slot(0, 2, 100, 5), slot(0, 3, 100, 3)];
        let gone = evict_walk(cs, 100, &mut |c| c.path != Path::new("b0/dir.1"));
        assert_eq!(gone, [PathBuf::from("b0/dir.2"), PathBuf::from("b0/dir.3"), PathBuf::from("b0/dir.4")]);
        let cs = vec![slot(0, 1, 100, 9), slot(0, 2, 100, 5), slot(0, 3, 100, 3)];
        assert_eq!(evict_walk(cs, 150, &mut |_| true), [PathBuf::from("b0/dir.1"), PathBuf::from("b0/dir.2")], "dir.3 stays: the total is under budget");
    }

    #[test_id(tguthyx)]
    #[test]
    fn an_eviction_within_budget_removes_nothing_and_empty_directories_are_never_candidates() {
        assert!(evict_walk(vec![slot(0, 1, 100, 9)], 100, &mut |_| panic!("under budget")).is_empty());
        let cs = vec![slot(0, 1, 0, 9), slot(0, 2, 100, 1)];
        assert_eq!(evict_walk(cs, 0, &mut |_| true), [PathBuf::from("b0/dir.2")]);
    }

    #[test_id(t88ueyg)]
    #[test]
    fn eviction_counts_every_base_against_one_budget() {
        let cs = vec![slot(0, 1, 100, 2), slot(1, 1, 100, 1)];
        assert_eq!(evict_walk(cs, 100, &mut |_| true), [PathBuf::from("b0/dir.1")]);
    }

    #[test_id(tkdsla4)]
    #[test]
    fn evicting_an_idle_slot_empties_it_and_keeps_the_directory_and_its_lock() {
        let d = testkit::TempDir::new("bdir-evict-slot");
        let base = d.path().join("base");
        fill(&base.join("dir.1/debug/x"), 64);
        std::fs::write(base.join("dir.1.lock"), "").unwrap();
        let gone = evict_idle_in(std::slice::from_ref(&base), 0, &crate::build::size_of, &empty_proc(d.path()));
        assert_eq!(gone, [base.join("dir.1")]);
        assert!(base.join("dir.1").is_dir() && !holds_anything(&base.join("dir.1")), "emptied in place");
        assert!(base.join("dir.1.lock").exists());
    }

    #[test_id(tm0bptj)]
    #[test]
    fn a_base_value_lists_directories_separated_by_whitespace() {
        assert_eq!(parse_bases(" x/b\ny/c "), [PathBuf::from("x/b"), PathBuf::from("y/c")]);
        assert!(parse_bases("  ").is_empty());
    }
}
