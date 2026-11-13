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
//!   directory was last made consistent with. An empty one is a new directory.
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

/// Lease a build directory under `bases` for `who`: the most recently idled one that holds a
/// build, else an empty idle one, else a new one on the base holding the fewest slots (the
/// first listed on a tie). Never waits and never touches a live one.
pub fn lease(bases: &[PathBuf], who: &str) -> std::io::Result<Lease> {
    for b in bases {
        let _ = std::fs::create_dir_all(b);
    }
    // Warm ones first, most recently idled first; then empty ones. Taking each lock in that
    // order means the first one won is the best idle directory, whatever races us.
    let mut order: Vec<(bool, SystemTime, usize, u64)> = bases
        .iter()
        .enumerate()
        .flat_map(|(i, b)| slots(b).into_iter().map(move |n| (holds_anything(&dir_path(b, n)), idle_since(b, n), i, n)))
        .collect();
    order.sort_by(|a, b| b.cmp(a));
    let mut won = order.iter().find_map(|&(warm, _, i, n)| try_lock(&bases[i], n).map(|f| (i, n, f, warm)));
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

/// Evict idle directories under `bases`, least recently used first, until every directory
/// together holds at most `budget_bytes`. A live directory is measured but never touched.
/// Returns the directories removed, in the order they went. Bytes are each directory's own
/// blocks ([`crate::build::size_of`]); the filesystem's fullness is never read.
pub fn evict_idle(bases: &[PathBuf], budget_bytes: u64) -> Vec<PathBuf> {
    evict_idle_with(bases, budget_bytes, &crate::build::size_of)
}

/// [`evict_idle`] with the measurement injected: `size` answers the bytes one directory holds.
pub fn evict_idle_with(bases: &[PathBuf], budget_bytes: u64, size: &dyn Fn(&Path) -> u64) -> Vec<PathBuf> {
    let sized: Vec<(usize, u64, u64)> = bases.iter().enumerate().flat_map(|(i, b)| slots(b).into_iter().map(move |n| (i, n, size(&dir_path(b, n))))).collect();
    let mut total: u64 = sized.iter().map(|(_, _, b)| b).sum();
    let mut gone = Vec::new();
    if total <= budget_bytes {
        return gone;
    }
    let mut idle: Vec<(SystemTime, usize, u64, u64)> =
        sized.into_iter().filter(|&(_, _, b)| b > 0).map(|(i, n, b)| (idle_since(&bases[i], n), i, n, b)).collect();
    idle.sort();
    for (_, i, n, bytes) in idle {
        if total <= budget_bytes {
            break;
        }
        // Under the slot's own lock: a directory a session takes meanwhile is skipped, and one
        // evicted here is an empty directory the next lease takes last.
        let Some(_held) = try_lock(&bases[i], n) else { continue };
        let dir = dir_path(&bases[i], n);
        empty(&dir);
        total = total.saturating_sub(bytes);
        gone.push(dir);
    }
    gone
}

/// The owned build directory a caller's `CARGO_TARGET_DIR` names, when it is one: an absolute
/// directory strictly under one of `bases`. Any other value is ignored, so an inherited override can
/// never redirect a build elsewhere.
pub fn owned(var: Option<std::ffi::OsString>, bases: &[PathBuf]) -> Option<PathBuf> {
    let p = PathBuf::from(var?);
    let ours = p.is_absolute() && p.is_dir() && bases.iter().any(|b| p != *b && p.starts_with(b));
    ours.then_some(p)
}

/// Run the shared selective clean (`script`, [`CLEAN_SCRIPT`]) for the tree at `root` against
/// the build directory `dir`: only the workspace crates whose bytes differ from the tree the
/// directory was last made consistent with are cleaned, and `root`'s manifest is recorded.
/// `record_only` records without cleaning (the release path). `path` is the PATH cargo is
/// found on. Ok(the script's output) or Err(why).
pub fn clean(script: &Path, root: &Path, dir: &Path, path: &str, record_only: bool) -> Result<String, String> {
    // A whole-workspace `cargo clean` of a large target is slow: bounded by a deadline of its own.
    let mut cmd = crate::bounded::bounded_for("bash", CLEAN_DEADLINE_SECS);
    cmd.arg(script);
    if record_only {
        cmd.arg("--record");
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

    fn set_idle(base: &Path, dir: &Path, secs_ago: u64) {
        let n = dir.file_name().unwrap().to_str().unwrap().to_string();
        let f = std::fs::OpenOptions::new().write(true).open(base.join(format!("{n}.lock"))).unwrap();
        f.set_modified(SystemTime::now() - std::time::Duration::from_secs(secs_ago)).unwrap();
    }

    fn touch(p: &Path, accessed_ago: u64, modified_ago: u64) {
        fill(p, 1);
        let ago = |s: u64| SystemTime::now() - std::time::Duration::from_secs(s);
        let times = std::fs::FileTimes::new().set_accessed(ago(accessed_ago)).set_modified(ago(modified_ago));
        std::fs::File::options().write(true).open(p).unwrap().set_times(times).unwrap();
    }

    const H: u64 = 3600;

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
        assert!(t.incremental > 0 && t.stale == 0 && t.idle_profiles == 0 && t.after < t.before, "{t:?}");
    }

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
        assert!(t.stale > 0 && t.incremental == 0, "{t:?}");
    }

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
        assert!(t.idle_profiles > 0, "{t:?}");
    }

    #[test]
    fn trimmed_display_names_bytes_before_after_and_each_step() {
        let t = Trimmed { before: 900, after: 100, idle_profiles: 5, incremental: 7, stale: 11 };
        let line = t.to_string();
        for want in ["900 bytes before", "100 after", "5 in idle profiles", "7 of stale incremental", "11 of stale deps"] {
            assert!(line.contains(want), "{line}");
        }
        assert!(!line.contains('\n'));
    }

    #[test]
    fn trim_drops_an_idle_profile_even_if_the_lease_only_read_it() {
        let d = testkit::TempDir::new("bdir-trim-read-profile");
        touch(&d.path().join("unittest/deps/x"), 0, 8 * H);
        trim(d.path(), SystemTime::now() - std::time::Duration::from_secs(H), SystemTime::now());
        assert!(!d.path().join("unittest").exists(), "the idle test is on writes: a read does not keep a profile");
    }

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
        assert!(evict_idle_with(std::slice::from_ref(&base), 0, &held).is_empty(), "nothing idle: a budget of zero removes nothing live");
        assert!(a.dir().join("debug/deps/liba.rlib").exists(), "the live directory is untouched under budget pressure");
        b.release();
        let c = lease(std::slice::from_ref(&base), "sp-c").expect("third lease");
        assert_ne!(a.dir(), c.dir(), "still live: the released one is taken instead");
        assert!(!c.reused, "the released directory held nothing, so it is no warm start");
    }

    #[test]
    fn eviction_under_budget_pressure_removes_only_idle_directories_least_recently_used_first() {
        let d = testkit::TempDir::new("bdir-evict");
        let base = d.path().join("base");
        let ls: Vec<Lease> = (0..4).map(|i| lease(std::slice::from_ref(&base), &format!("sp-{i}")).unwrap()).collect();
        let dirs: Vec<PathBuf> = ls.iter().map(|l| l.dir().to_path_buf()).collect();
        for p in &dirs {
            fill(&p.join("debug/deps/x"), 256);
        }
        let mut ls = ls.into_iter();
        let live = ls.next().unwrap();
        for l in ls {
            l.release();
        }
        // The measurement is injected (100 bytes for a directory that holds anything), so the
        // host disk never decides the result.
        let held = |p: &Path| u64::from(holds_anything(p)) * 100;
        // dirs[0] is live and the oldest-stamped of all; the idle ones idled 3, 2 and 1 hours ago.
        set_idle(&base, &dirs[0], 9 * 3600);
        set_idle(&base, &dirs[1], 3 * 3600);
        set_idle(&base, &dirs[2], 2 * 3600);
        set_idle(&base, &dirs[3], 3600);
        assert!(evict_idle_with(std::slice::from_ref(&base), 1000, &held).is_empty(), "under the budget nothing goes");
        let gone = evict_idle_with(std::slice::from_ref(&base), 250, &held);
        assert_eq!(gone, vec![dirs[1].clone(), dirs[2].clone()], "least recently used idle first, stopping once under");
        assert!(dirs[0].join("debug/deps/x").exists(), "the live directory is never evicted, though it is the oldest");
        assert!(dirs[3].join("debug/deps/x").exists(), "under the budget once two went: the newest idle stays");
        let gone = evict_idle_with(std::slice::from_ref(&base), 0, &held);
        assert_eq!(gone, vec![dirs[3].clone()], "a budget the live one alone exceeds evicts every idle one and stops");
        assert!(dirs[0].join("debug/deps/x").exists());
        drop(live);
    }

    fn workspace(root: &Path, b_body: &str) {
        std::fs::create_dir_all(root.join("a/src")).unwrap();
        std::fs::create_dir_all(root.join("b/src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"a\", \"b\"]\n").unwrap();
        std::fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
        std::fs::write(root.join("a/Cargo.toml"), "[package]\nname = \"crate-a\"\n").unwrap();
        std::fs::write(root.join("a/src/lib.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(root.join("b/Cargo.toml"), "[package]\nname = \"crate-b\"\n").unwrap();
        std::fs::write(root.join("b/src/lib.rs"), b_body).unwrap();
    }

    /// A `cargo` that only logs its arguments, one call per line.
    fn fake_cargo(d: &Path) -> (String, PathBuf) {
        let bin = d.join("bin");
        let log = d.join("cargo.log");
        std::fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("cargo"), &format!("#!/bin/sh\necho \"$*\" >> {}\n", log.display()));
        (format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()), log)
    }

    fn script() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira").join(CLEAN_SCRIPT)
    }

    #[test]
    fn a_lease_after_an_idle_release_reuses_that_directory_and_cleans_only_the_changed_crates() {
        let d = testkit::TempDir::new("bdir-reuse");
        let base = d.path().join("base");
        let (path, log) = fake_cargo(d.path());
        let one = d.path().join("wt-one");
        workspace(&one, "pub fn b() {}\n");
        let first = lease(std::slice::from_ref(&base), "sp-one").unwrap();
        let dir = first.dir().to_path_buf();
        clean(&script(), &one, &dir, &path, false).expect("first clean");
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "clean --workspace -q", "no record yet: the workspace crates are cleaned");
        fill(&dir.join("debug/deps/libdep.rlib"), 4);
        first.release();

        let two = d.path().join("wt-two");
        workspace(&two, "pub fn b() { let _ = 1; }\n");
        std::fs::remove_file(&log).unwrap();
        let second = lease(std::slice::from_ref(&base), "sp-two").unwrap();
        assert_eq!(second.dir(), dir, "the idle directory is reused, not a new empty one");
        assert!(second.reused);
        let out = clean(&script(), &two, second.dir(), &path, false).expect("second clean");
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "clean -q -p crate-b", "only the crate whose bytes changed: {out}");
        assert!(dir.join("debug/deps/libdep.rlib").exists(), "dependencies stay warm");

        std::fs::remove_file(&log).unwrap();
        clean(&script(), &two, second.dir(), &path, false).expect("third clean");
        assert!(!log.exists(), "the same tree again cleans nothing");
        std::fs::write(two.join("Cargo.lock"), "# lock 2\n").unwrap();
        clean(&script(), &two, second.dir(), &path, true).expect("record only");
        assert!(!log.exists(), "recording never cleans");
        clean(&script(), &two, second.dir(), &path, false).expect("fourth clean");
        assert!(!log.exists(), "the release recorded the changed lock: nothing to clean");
    }

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

    #[test]
    fn an_existing_slot_stays_where_it_is_and_a_new_slot_goes_to_the_emptier_base() {
        let d = testkit::TempDir::new("bdir-two-bases-stay");
        let bases = [d.path().join("b"), d.path().join("c")];
        let a = lease(&bases, "sp-a").unwrap();
        let b = lease(&bases, "sp-b").unwrap();
        let (da, db) = (a.dir().to_path_buf(), b.dir().to_path_buf());
        assert_eq!((da.parent(), db.parent()), (Some(bases[0].as_path()), Some(bases[1].as_path())));
        a.release();
        assert_eq!(lease(&bases, "sp-c").unwrap().dir(), da, "an idle slot is reused where it lives");
    }

    #[test]
    fn a_new_slot_goes_to_the_base_with_fewer_slots_not_the_first_listed() {
        let d = testkit::TempDir::new("bdir-uneven");
        let bases = [d.path().join("b"), d.path().join("c")];
        let _crowded: Vec<Lease> = (0..3).map(|i| lease(&bases[..1], &format!("sp-{i}")).unwrap()).collect();
        assert_eq!(lease(&bases, "sp-new").unwrap().dir().parent(), Some(bases[1].as_path()));
    }

    #[test]
    fn a_base_that_cannot_hold_a_slot_is_skipped_for_the_next() {
        let d = testkit::TempDir::new("bdir-unusable");
        let file = d.path().join("not-a-dir");
        std::fs::write(&file, "").unwrap();
        let bases = [file.join("b"), d.path().join("c")];
        assert_eq!(lease(&bases, "sp-a").unwrap().dir().parent(), Some(bases[1].as_path()));
        assert!(lease(&bases[..1], "sp-b").is_err(), "positive control: with only the unusable base there is no slot");
    }

    #[test]
    fn eviction_counts_every_base_against_one_budget() {
        let d = testkit::TempDir::new("bdir-two-bases-evict");
        let bases = [d.path().join("b"), d.path().join("c")];
        let ls: Vec<Lease> = ["sp-a", "sp-b"].iter().map(|who| lease(&bases, who).unwrap()).collect();
        for (i, l) in ls.iter().enumerate() {
            fill(&l.dir().join("debug/x"), 1);
            assert_eq!(l.dir().parent(), Some(bases[i].as_path()));
        }
        for l in ls {
            l.release();
        }
        let held = |p: &Path| u64::from(holds_anything(p)) * 100;
        assert_eq!(evict_idle_with(&bases, 100, &held).len(), 1);
    }

    #[test]
    fn a_base_value_lists_directories_separated_by_whitespace() {
        assert_eq!(parse_bases(" /x/b\n/y/c "), [PathBuf::from("/x/b"), PathBuf::from("/y/c")]);
        assert!(parse_bases("  ").is_empty());
    }
}
