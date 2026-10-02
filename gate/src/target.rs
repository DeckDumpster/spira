//! A gate tree's build output on tmpfs (sp-z61hj; spira-config/DESIGN-build-cache.md §2.3,
//! DESIGN.md "The gate tree's target is on tmpfs").
//!
//! `<tree>/target` stays a real directory — git ignores it in every revision a gate tree can
//! hold (`target/`), so nothing inside it is ever an untracked file to a fence — and each of
//! its build directories ([`LINKED`]) is a symlink to `<root>/<tree basename>/<dir>` on a
//! RAM-backed tmpfs. Every path a step or the tools phase reads (`target/aeon/<pkg>`,
//! `target/gate-tools/<id>`) is unchanged; only where the bytes land moves.

use std::fs;
use std::path::{Path, PathBuf};

/// The build directories of a gate tree that live on the tmpfs: the unit/tools profile, the
/// build fence's `make build` (release), a stray dev build, and the tree-keyed tools.
pub const LINKED: [&str; 4] = ["aeon", "release", "debug", "gate-tools"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Total MiB of every tree's directory under the root before the least recently used
    /// unlocked ones are evicted.
    pub cap_mib: u64,
    /// MiB the root must have free for this trial.
    pub min_free_mib: u64,
    /// MiB of MemAvailable this trial needs (tmpfs pages are RAM; short means swap, a disk).
    pub min_mem_mib: u64,
    /// sp-s8v5r: besides the byte cap, keep evicting LRU unlocked trees while the root's
    /// real free space is below this — the same floor testenv sheds idle warm slots
    /// against (`SPIRA_TMPFS_SHED_FREE_MIB`), since both compete for the same tmpfs.
    /// law-reduce-the-count-never-throttle-the-job: reduce the count of cached trees here
    /// rather than refuse the trial.
    pub shed_free_mib: u64,
}

impl Limits {
    /// From the operator's variables (empty or unparsable = the default).
    pub fn from_vars(cap: &str, free: &str, mem: &str, shed: &str) -> Limits {
        let n = |s: &str, d: u64| s.trim().parse::<u64>().unwrap_or(d);
        Limits {
            cap_mib: n(cap, 12288),
            min_free_mib: n(free, 4096),
            min_mem_mib: n(mem, 4096),
            shed_free_mib: n(shed, 6144),
        }
    }
}

pub fn on_tmpfs(p: &Path) -> bool {
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    let mut cur = Some(p);
    while let Some(d) = cur {
        if d.exists() {
            let Ok(c) = std::ffi::CString::new(d.as_os_str().as_encoded_bytes()) else {
                return false;
            };
            // SAFETY: statfs into a zeroed struct we own, on a NUL-terminated path.
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            let r = unsafe { libc::statfs(c.as_ptr(), &mut st) };
            #[allow(clippy::unnecessary_cast)]
            return r == 0 && st.f_type as i64 == TMPFS_MAGIC;
        }
        cur = d.parent();
    }
    false
}

/// Where gate-tree targets live: `explicit` (`$SPIRA_GATE_TARGET_ROOT`) when set, else
/// `/tmp/spira-gate-target-<sha256(run)[..6]>` when `/tmp` is a tmpfs, else None (no tmpfs
/// to offer — the target stays in the tree).
pub fn root(explicit: &str, run: &str, tmp_is_tmpfs: bool) -> Option<PathBuf> {
    if !explicit.trim().is_empty() {
        return Some(PathBuf::from(explicit.trim()));
    }
    if !tmp_is_tmpfs {
        return None;
    }
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(run.as_bytes());
    let hex: String = h.iter().take(3).map(|b| format!("{b:02x}")).collect();
    Some(PathBuf::from(format!("/tmp/spira-gate-target-{hex}")))
}

/// Bytes under `p`, not following symlinks.
pub fn size_of(p: &Path) -> u64 {
    let Ok(md) = fs::symlink_metadata(p) else { return 0 };
    if !md.is_dir() {
        use std::os::unix::fs::MetadataExt;
        return md.blocks() * 512;
    }
    fs::read_dir(p).map(|rd| rd.flatten().map(|e| size_of(&e.path())).sum()).unwrap_or(0)
}

fn mtime(p: &Path) -> std::time::SystemTime {
    fs::metadata(p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH)
}

/// True when nobody holds `lock` (a non-blocking exclusive `flock` succeeds, and is released).
/// A lock file that does not exist is free.
pub fn lock_free(lock: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(f) = fs::OpenOptions::new().read(true).open(lock) else { return true };
    // SAFETY: flock on a descriptor we own; it is closed (and so unlocked) on drop.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// What [`prepare`] did, for the trial's log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    pub dir: PathBuf,
    pub orphans: Vec<String>,
    pub evicted: Vec<String>,
    /// MiB freed on the disk by replacing a tree's real build directories.
    pub disk_freed_mib: u64,
}

/// Put `tree`'s build directories on `root` (DESIGN-build-cache.md §2.3): remove orphans (a
/// directory whose gate tree `trees_dir/<name>` is gone), evict the least recently used
/// unlocked trees' directories while the total is over the cap, check the room, and link.
/// `free_mib`/`mem_mib` are the room probes (statvfs of the root, MemAvailable). Err is the
/// refusal: the trial must not fall back to the disk.
pub fn prepare(
    root: &Path,
    trees_dir: &Path,
    tree: &Path,
    lim: &Limits,
    free_mib: &dyn Fn(&Path) -> Option<u64>,
    mem_mib: &dyn Fn() -> u64,
) -> Result<Prepared, String> {
    let name = tree
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("gate: {} has no name", tree.display()))?;
    fs::create_dir_all(root).map_err(|e| format!("gate: cannot create {}: {e}", root.display()))?;
    let mut out = Prepared { dir: root.join(&name), ..Prepared::default() };

    // 1. Orphans: their gate tree was swept (gate-sweep.sh removes the worktree, and with it
    //    only the symlinks).
    let mut live: Vec<(PathBuf, String)> = Vec::new();
    for e in fs::read_dir(root).map_err(|e| format!("gate: cannot read {}: {e}", root.display()))?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n == name {
            continue;
        }
        if !trees_dir.join(&n).is_dir() {
            let _ = fs::remove_dir_all(e.path());
            out.orphans.push(n);
        } else {
            live.push((e.path(), n));
        }
    }

    // 2. The cap, least recently used first; a tree whose lock is held is a running trial.
    //    Besides the byte cap, sp-s8v5r also keeps evicting while the root's actual free
    //    space is short of `shed_free_mib` — the signal that paged "Disk quota exceeded"
    //    twice in one day, which the byte cap alone does not see (another process on the
    //    same tmpfs, like testenv's warm slots, can eat the room this cap never counted).
    let mut sizes: Vec<(PathBuf, String, u64)> = live.into_iter().map(|(p, n)| { let s = size_of(&p); (p, n, s) }).collect();
    let own = size_of(&out.dir);
    let mut total: u64 = sizes.iter().map(|s| s.2).sum::<u64>() + own;
    let cap = lim.cap_mib * 1024 * 1024;
    sizes.sort_by_key(|(p, _, _)| mtime(p));
    for (p, n, s) in sizes {
        let short_of_floor = free_mib(root).is_some_and(|f| f < lim.shed_free_mib);
        if total <= cap && !short_of_floor {
            break;
        }
        if !lock_free(&trees_dir.join(format!("{n}.lock"))) {
            continue;
        }
        let _ = fs::remove_dir_all(&p);
        total = total.saturating_sub(s);
        out.evicted.push(n);
    }

    // 3. The room this trial needs — short is a refusal, never the disk.
    let free = free_mib(root).ok_or_else(|| format!("gate: cannot statvfs {}", root.display()))?;
    if free < lim.min_free_mib {
        return Err(format!(
            "gate: the gate-tree target root {} has {free} MiB free, below the {} MiB a trial needs (SPIRA_GATE_TARGET_MIN_FREE_MIB) — refusing to build on the disk",
            root.display(),
            lim.min_free_mib
        ));
    }
    let mem = mem_mib();
    if mem < lim.min_mem_mib {
        return Err(format!(
            "gate: MemAvailable is {mem} MiB, below the {} MiB a tmpfs build needs (SPIRA_GATE_TARGET_MIN_MEM_MIB) — refusing to build on the disk",
            lim.min_mem_mib
        ));
    }

    // 4. Link. A real directory from before is removed (its disk freed); a dangling or
    //    foreign link is replaced.
    let target = tree.join("target");
    if fs::symlink_metadata(&target).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        let _ = fs::remove_file(&target);
    }
    fs::create_dir_all(&target).map_err(|e| format!("gate: cannot create {}: {e}", target.display()))?;
    for d in LINKED {
        let dest = out.dir.join(d);
        fs::create_dir_all(&dest).map_err(|e| format!("gate: cannot create {}: {e}", dest.display()))?;
        let link = target.join(d);
        match fs::symlink_metadata(&link) {
            Ok(m) if m.file_type().is_symlink() => {
                if fs::read_link(&link).ok().as_deref() == Some(dest.as_path()) {
                    continue;
                }
                let _ = fs::remove_file(&link);
            }
            Ok(m) if m.is_dir() => {
                out.disk_freed_mib += size_of(&link) / (1024 * 1024);
                fs::remove_dir_all(&link).map_err(|e| format!("gate: cannot clear {}: {e}", link.display()))?;
            }
            Ok(_) => {
                let _ = fs::remove_file(&link);
            }
            Err(_) => {}
        }
        std::os::unix::fs::symlink(&dest, &link)
            .map_err(|e| format!("gate: cannot link {} to {}: {e}", link.display(), dest.display()))?;
    }
    // Recently used: the eviction order of the next trial.
    let _ = fs::File::open(&out.dir).and_then(|d| d.set_modified(std::time::SystemTime::now()));
    Ok(out)
}

/// Remove the tmpfs directories `tree`'s build links point at (a finished gate's output is
/// read by nobody), sparing `release` when the caller still has to stage it. Returns MiB freed.
pub fn release_tree(tree: &Path, keep_release: bool) -> u64 {
    let mut bytes = 0;
    let mut homes: Vec<PathBuf> = Vec::new();
    for d in LINKED {
        let link = tree.join("target").join(d);
        if keep_release && d == "release" {
            continue;
        }
        let Ok(dest) = fs::read_link(&link) else { continue };
        bytes += size_of(&dest);
        let _ = fs::remove_dir_all(&dest);
        let _ = fs::remove_file(&link);
        if let Some(p) = dest.parent() {
            homes.push(p.to_path_buf());
        }
    }
    homes.sort();
    homes.dedup();
    for h in homes {
        let _ = fs::remove_dir(&h); // only when empty
    }
    bytes / (1024 * 1024)
}

/// Backstop for crashed gates: remove every `root/<name>` untouched for `max_age` that no
/// gate holds (`trees_dir/<name>.lock` free and `busy` false). Returns the names removed (or,
/// with `dry`, that would be).
pub fn reap_stale(
    root: &Path,
    trees_dir: &Path,
    max_age: std::time::Duration,
    dry: bool,
    busy: &dyn Fn(&Path) -> bool,
) -> Vec<String> {
    let now = std::time::SystemTime::now();
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if !p.is_dir() || now.duration_since(mtime(&p)).unwrap_or_default() < max_age {
            continue;
        }
        if !lock_free(&trees_dir.join(format!("{n}.lock"))) || busy(&p) || busy(&trees_dir.join(&n)) {
            continue;
        }
        if !dry {
            let _ = fs::remove_dir_all(&p);
        }
        out.push(n);
    }
    out.sort();
    out
}

/// Space this user can still write under `p`: statvfs free or quota headroom, whichever is smaller.
pub fn free_mib(p: &Path) -> Option<u64> {
    spira_config::room::avail_mib(p)
}

/// MemAvailable in MiB (0 when unreadable, which refuses).
pub fn mem_available_mib() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemAvailable:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(testkit::TempDir, PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Scratch {
            let t = testkit::TempDir::new(&format!("gate-target-{tag}"));
            let p = t.path().to_path_buf();
            Scratch(t, p)
        }
    }

    fn lim(cap: u64) -> Limits {
        Limits { cap_mib: cap, min_free_mib: 10, min_mem_mib: 10, shed_free_mib: 0 }
    }
    fn roomy(_: &Path) -> Option<u64> {
        Some(1 << 20)
    }
    fn mem() -> u64 {
        1 << 20
    }
    fn write_mib(p: &Path, mib: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; mib * 1024 * 1024]).unwrap();
    }

    #[test]
    fn links_every_build_dir_and_frees_a_real_one_from_before() {
        let s = Scratch::new("link");
        let (root, trees) = (s.1.join("root"), s.1.join("worktree"));
        let tree = trees.join(".gate.harness.b");
        write_mib(&tree.join("target/aeon/deps/libx.rlib"), 2);
        let p = prepare(&root, &trees, &tree, &lim(100), &roomy, &mem).unwrap();
        assert_eq!(p.dir, root.join(".gate.harness.b"));
        assert_eq!(p.disk_freed_mib, 2);
        for d in LINKED {
            let l = tree.join("target").join(d);
            assert!(fs::symlink_metadata(&l).unwrap().file_type().is_symlink(), "{d}");
            assert_eq!(fs::read_link(&l).unwrap(), root.join(".gate.harness.b").join(d));
        }
        // cargo writes through the link; a second prepare leaves it (and the bytes) alone.
        write_mib(&tree.join("target/aeon/deps/liby.rlib"), 1);
        let p2 = prepare(&root, &trees, &tree, &lim(100), &roomy, &mem).unwrap();
        assert_eq!(p2.disk_freed_mib, 0);
        assert!(root.join(".gate.harness.b/aeon/deps/liby.rlib").is_file());
        // `target` itself stays a real directory: git ignores `target/` in every revision.
        assert!(fs::symlink_metadata(tree.join("target")).unwrap().is_dir());
    }

    #[test]
    fn a_target_symlink_is_replaced_by_a_directory_and_a_dangling_link_is_restored() {
        let s = Scratch::new("dangle");
        let (root, trees) = (s.1.join("root"), s.1.join("worktree"));
        let tree = trees.join(".gate.harness.b");
        fs::create_dir_all(&tree).unwrap();
        std::os::unix::fs::symlink("/nonexistent", tree.join("target")).unwrap();
        prepare(&root, &trees, &tree, &lim(100), &roomy, &mem).unwrap();
        fs::remove_dir_all(root.join(".gate.harness.b")).unwrap(); // evicted by another trial
        prepare(&root, &trees, &tree, &lim(100), &roomy, &mem).unwrap();
        assert!(tree.join("target/aeon").is_dir(), "the link resolves again");
    }

    #[test]
    fn orphans_go_and_the_least_recently_used_unlocked_tree_is_evicted_over_the_cap() {
        let s = Scratch::new("cap");
        let (root, trees) = (s.1.join("root"), s.1.join("worktree"));
        for t in ["old", "mid", "held"] {
            fs::create_dir_all(trees.join(format!(".gate.harness.{t}"))).unwrap();
            write_mib(&root.join(format!(".gate.harness.{t}/aeon/x")), 2);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // "held" is the oldest now, and a running trial holds its lock.
        let _ = fs::File::open(root.join(".gate.harness.held")).and_then(|d| d.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)));
        let lock = trees.join(".gate.harness.held.lock");
        let f = fs::File::create(&lock).unwrap();
        use std::os::unix::io::AsRawFd;
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        write_mib(&root.join(".gate.harness.gone/aeon/x"), 1); // its tree was swept
        let tree = trees.join(".gate.harness.new");
        fs::create_dir_all(&tree).unwrap();
        let p = prepare(&root, &trees, &tree, &lim(5), &roomy, &mem).unwrap();
        assert_eq!(p.orphans, vec![".gate.harness.gone".to_string()]);
        assert_eq!(p.evicted, vec![".gate.harness.old".to_string()], "held is skipped, old goes, then under the cap");
        assert!(root.join(".gate.harness.held/aeon/x").is_file());
        assert!(root.join(".gate.harness.mid/aeon/x").is_file());
        drop(f);
    }

    #[test]
    fn short_room_is_a_refusal_never_the_disk() {
        let s = Scratch::new("short");
        let (root, trees) = (s.1.join("root"), s.1.join("worktree"));
        let tree = trees.join(".gate.harness.b");
        fs::create_dir_all(&tree).unwrap();
        let e = prepare(&root, &trees, &tree, &Limits { cap_mib: 100, min_free_mib: 4096, min_mem_mib: 1, shed_free_mib: 0 }, &|_| Some(100), &mem).unwrap_err();
        assert!(e.contains("100 MiB free") && e.contains("refusing to build on the disk"), "{e}");
        let e = prepare(&root, &trees, &tree, &Limits { cap_mib: 100, min_free_mib: 1, min_mem_mib: 4096, shed_free_mib: 0 }, &roomy, &|| 12).unwrap_err();
        assert!(e.contains("MemAvailable is 12 MiB"), "{e}");
        assert!(!tree.join("target").exists(), "nothing linked, nothing built");
        assert!(prepare(&root, &trees, &tree, &lim(100), &|_| None, &mem).is_err(), "an unreadable root refuses");
    }

    #[test]
    fn the_root_is_explicit_else_a_run_keyed_tmp_dir_else_none() {
        assert_eq!(root("/x/y", "/run", false), Some(PathBuf::from("/x/y")));
        let a = root("", "/srv/a/run", true).unwrap();
        assert!(a.to_string_lossy().starts_with("/tmp/spira-gate-target-"));
        assert_ne!(a, root("", "/srv/b/run", true).unwrap(), "two installs never share a root");
        assert_eq!(root("", "/run", false), None);
        assert_eq!(
            Limits::from_vars("", "x", "7", ""),
            Limits { cap_mib: 12288, min_free_mib: 4096, min_mem_mib: 7, shed_free_mib: 6144 }
        );
        assert_eq!(Limits::from_vars("1", "2", "3", "500").shed_free_mib, 500);
    }

    /// sp-s8v5r, positive control: the byte cap alone (deliberately huge) would evict
    /// nothing; a fake free-space reading below the shared shed floor does, oldest
    /// unlocked tree first, and a held tree is never touched. The same low reading against
    /// a floor it already clears evicts nothing — proof the floor, not the fake itself,
    /// drives it.
    #[test]
    fn the_floor_evicts_further_than_the_cap_but_never_a_held_tree() {
        let s = Scratch::new("floor");
        let (root, trees) = (s.1.join("root"), s.1.join("worktree"));
        for t in ["old", "mid", "held"] {
            fs::create_dir_all(trees.join(format!(".gate.harness.{t}"))).unwrap();
            write_mib(&root.join(format!(".gate.harness.{t}/aeon/x")), 1);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // "held" is the oldest now, and a running trial holds its lock.
        let _ = fs::File::open(root.join(".gate.harness.held"))
            .and_then(|d| d.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)));
        let lock = trees.join(".gate.harness.held.lock");
        let f = fs::File::create(&lock).unwrap();
        use std::os::unix::io::AsRawFd;
        assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);

        let tree = trees.join(".gate.harness.new");
        fs::create_dir_all(&tree).unwrap();

        // RED FIRST: a huge byte cap would evict nothing on its own; 50 MiB free below a
        // 200 MiB floor makes the eviction continue past the cap check — but stops as soon
        // as the (fake) drop clears the floor, and never touches the held tree.
        let calls = std::cell::Cell::new(0u64);
        let short = move |_: &Path| {
            let n = calls.get();
            calls.set(n + 1);
            Some(if n < 2 { 50 } else { 300 })
        };
        let lim_floor = Limits { cap_mib: 1_000_000, min_free_mib: 10, min_mem_mib: 10, shed_free_mib: 200 };
        let p = prepare(&root, &trees, &tree, &lim_floor, &short, &mem).unwrap();
        assert_eq!(p.evicted, vec![".gate.harness.old".to_string()], "held is skipped despite being the LRU; old alone clears the floor");
        assert!(root.join(".gate.harness.held/aeon/x").is_file());
        assert!(root.join(".gate.harness.mid/aeon/x").is_file());

        // CONTROL: the identical 50 MiB reading, but a floor it already clears — nothing
        // is shed, so the behavior above is the floor's, not an eviction that always runs.
        let tree2 = trees.join(".gate.harness.new2");
        fs::create_dir_all(&tree2).unwrap();
        let lim_ok = Limits { cap_mib: 1_000_000, min_free_mib: 10, min_mem_mib: 10, shed_free_mib: 10 };
        let p2 = prepare(&root, &trees, &tree2, &lim_ok, &|_| Some(50u64), &mem).unwrap();
        assert!(p2.evicted.is_empty(), "50 MiB free already clears a 10 MiB floor — nothing to shed");
        drop(f);
    }
    #[test]
    fn a_finished_gate_drops_its_target_but_can_spare_release() {
        let s = Scratch::new("release");
        let (root, trees) = (s.1.join("root"), s.1.join("worktree"));
        let tree = trees.join(".gate.harness.b");
        fs::create_dir_all(&tree).unwrap();
        prepare(&root, &trees, &tree, &lim(100), &roomy, &mem).unwrap();
        write_mib(&tree.join("target/aeon/x"), 2);
        write_mib(&tree.join("target/release/y"), 1);
        assert_eq!(release_tree(&tree, true), 2);
        assert!(!root.join(".gate.harness.b/aeon").exists());
        assert!(root.join(".gate.harness.b/release/y").is_file(), "release is spared for staging");
        release_tree(&tree, false);
        assert!(!root.join(".gate.harness.b").exists(), "nothing left: the dir goes too");
    }

    #[test]
    fn stale_unheld_dirs_are_reaped_and_held_or_fresh_ones_are_not() {
        let s = Scratch::new("stale");
        let (root, trees) = (s.1.join("root"), s.1.join("worktree"));
        fs::create_dir_all(&trees).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 3600);
        for n in ["stale", "held", "busy", "fresh"] {
            write_mib(&root.join(n).join("aeon/x"), 1);
            if n != "fresh" {
                fs::File::open(root.join(n)).unwrap().set_modified(old).unwrap();
            }
        }
        let lock = trees.join("held.lock");
        fs::write(&lock, "").unwrap();
        let _held = {
            use std::os::unix::io::AsRawFd;
            let f = fs::File::open(&lock).unwrap();
            assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
            f
        };
        let busy = |p: &Path| p.ends_with("busy");
        let two_h = std::time::Duration::from_secs(2 * 3600);
        assert_eq!(reap_stale(&root, &trees, two_h, true, &busy), vec!["stale".to_string()]);
        assert!(root.join("stale").exists(), "dry run removes nothing");
        assert_eq!(reap_stale(&root, &trees, two_h, false, &busy), vec!["stale".to_string()]);
        assert!(!root.join("stale").exists());
        for n in ["held", "busy", "fresh"] {
            assert!(root.join(n).join("aeon/x").is_file(), "{n} kept");
        }
    }
}
