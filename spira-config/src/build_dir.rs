//! Owned build directories (sp-ke0uk8): each dwarf session builds in ONE cargo target it owns
//! for its whole bead, as a single developer would — change, build, change, build.
//!
//! The directories live under one base on the compile disk (`SPIRA_DWARF_BUILD_BASE`), as
//! `dir.<n>`, each guarded by `dir.<n>.lock`. A lease is a non-blocking exclusive `flock` on that
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
//! Dependencies still come from sccache, which is shared, content-addressed and append-only, so
//! no lease can make another session's dependencies cold.

use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Registry key: the directory holding every owned build directory (on the compile disk).
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

    /// Release the directory to idle, stamping now as its idle time so the next lease prefers it.
    pub fn release(self) {
        let _ = self.lock.set_len(0);
        let _ = self.lock.set_modified(SystemTime::now());
        drop(self);
    }
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

/// Lease a build directory under `base` for `who`: the most recently idled one that holds a
/// build, else an empty idle one, else a new one. Never waits and never touches a live one.
pub fn lease(base: &Path, who: &str) -> std::io::Result<Lease> {
    use std::io::Write;
    std::fs::create_dir_all(base)?;
    // Warm ones first, most recently idled first; then empty ones. Taking each lock in that
    // order means the first one won is the best idle directory, whatever races us.
    let mut order: Vec<(bool, SystemTime, u64)> = slots(base).into_iter().map(|n| (holds_anything(&dir_path(base, n)), idle_since(base, n), n)).collect();
    order.sort_by(|a, b| b.cmp(a));
    let mut won = order.iter().find_map(|&(warm, _, n)| try_lock(base, n).map(|f| (n, f, warm)));
    if won.is_none() {
        let mut n = slots(base).last().copied().unwrap_or(0) + 1;
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(lock_path(base, n)) {
                Ok(_) => {
                    if let Some(f) = try_lock(base, n) {
                        won = Some((n, f, false));
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            n += 1;
        }
    }
    let (n, mut lock, reused) = won.expect("a lock was won above");
    let dir = dir_path(base, n);
    std::fs::create_dir_all(&dir)?;
    // The holder's name, for whoever reads the base; writing it stamps the lock file, so a
    // holder that dies without releasing reads as idle since it leased.
    let _ = lock.set_len(0);
    let _ = lock.write_all(who.as_bytes());
    Ok(Lease { dir, lock, reused })
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

/// Evict idle directories under `base`, least recently used first, until every directory
/// together holds at most `budget_bytes`. A live directory is measured but never touched.
/// Returns the directories removed, in the order they went. Bytes are each directory's own
/// blocks ([`crate::build::size_of`]); the filesystem's fullness is never read.
pub fn evict_idle(base: &Path, budget_bytes: u64) -> Vec<PathBuf> {
    evict_idle_with(base, budget_bytes, &crate::build::size_of)
}

/// [`evict_idle`] with the measurement injected: `size` answers the bytes one directory holds.
pub fn evict_idle_with(base: &Path, budget_bytes: u64, size: &dyn Fn(&Path) -> u64) -> Vec<PathBuf> {
    let sized: Vec<(u64, u64)> = slots(base).into_iter().map(|n| (n, size(&dir_path(base, n)))).collect();
    let mut total: u64 = sized.iter().map(|(_, b)| b).sum();
    let mut gone = Vec::new();
    if total <= budget_bytes {
        return gone;
    }
    let mut idle: Vec<(SystemTime, u64, u64)> = sized.into_iter().filter(|&(_, b)| b > 0).map(|(n, b)| (idle_since(base, n), n, b)).collect();
    idle.sort();
    for (_, n, bytes) in idle {
        if total <= budget_bytes {
            break;
        }
        // Under the slot's own lock: a directory a session takes meanwhile is skipped, and one
        // evicted here is an empty directory the next lease takes last.
        let Some(_held) = try_lock(base, n) else { continue };
        let dir = dir_path(base, n);
        empty(&dir);
        total = total.saturating_sub(bytes);
        gone.push(dir);
    }
    gone
}

/// The owned build directory a caller's `CARGO_TARGET_DIR` names, when it is one: an absolute
/// directory strictly under `base`. Any other value is ignored, so an inherited override can
/// never redirect a build elsewhere.
pub fn owned(var: Option<std::ffi::OsString>, base: Option<&Path>) -> Option<PathBuf> {
    let p = PathBuf::from(var?);
    let ours = p.is_absolute() && p.is_dir() && base.is_some_and(|b| p != b && p.starts_with(b));
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

    #[test]
    fn a_second_lease_while_one_is_live_never_touches_the_live_directory() {
        let held = |p: &Path| u64::from(holds_anything(p)) * 100;
        let d = testkit::TempDir::new("bdir-live");
        let base = d.path().join("base");
        let a = lease(&base, "sp-a").expect("first lease");
        fill(&a.dir().join("debug/deps/liba.rlib"), 64);
        let b = lease(&base, "sp-b").expect("second lease");
        assert_ne!(a.dir(), b.dir(), "a live directory is never handed out twice");
        assert!(a.dir().join("debug/deps/liba.rlib").exists());
        assert!(evict_idle_with(&base, 0, &held).is_empty(), "nothing idle: a budget of zero removes nothing live");
        assert!(a.dir().join("debug/deps/liba.rlib").exists(), "the live directory is untouched under budget pressure");
        b.release();
        let c = lease(&base, "sp-c").expect("third lease");
        assert_ne!(a.dir(), c.dir(), "still live: the released one is taken instead");
        assert!(!c.reused, "the released directory held nothing, so it is no warm start");
    }

    #[test]
    fn eviction_under_budget_pressure_removes_only_idle_directories_least_recently_used_first() {
        let d = testkit::TempDir::new("bdir-evict");
        let base = d.path().join("base");
        let ls: Vec<Lease> = (0..4).map(|i| lease(&base, &format!("sp-{i}")).unwrap()).collect();
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
        assert!(evict_idle_with(&base, 1000, &held).is_empty(), "under the budget nothing goes");
        let gone = evict_idle_with(&base, 250, &held);
        assert_eq!(gone, vec![dirs[1].clone(), dirs[2].clone()], "least recently used idle first, stopping once under");
        assert!(dirs[0].join("debug/deps/x").exists(), "the live directory is never evicted, though it is the oldest");
        assert!(dirs[3].join("debug/deps/x").exists(), "under the budget once two went: the newest idle stays");
        let gone = evict_idle_with(&base, 0, &held);
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
        let first = lease(&base, "sp-one").unwrap();
        let dir = first.dir().to_path_buf();
        clean(&script(), &one, &dir, &path, false).expect("first clean");
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "clean --workspace -q", "no record yet: the workspace crates are cleaned");
        fill(&dir.join("debug/deps/libdep.rlib"), 4);
        first.release();

        let two = d.path().join("wt-two");
        workspace(&two, "pub fn b() { let _ = 1; }\n");
        std::fs::remove_file(&log).unwrap();
        let second = lease(&base, "sp-two").unwrap();
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
        assert_eq!(owned(some(&base.join("dir.1")), Some(&base)), Some(base.join("dir.1")));
        assert_eq!(owned(some(&base), Some(&base)), None, "the base itself is no directory of its own");
        assert_eq!(owned(some(&d.path().join("elsewhere")), Some(&base)), None);
        assert_eq!(owned(some(&base.join("dir.1")), None), None, "no base configured: nothing is owned");
        assert_eq!(owned(Some("rel/dir.1".into()), Some(Path::new("rel"))), None);
    }
}
