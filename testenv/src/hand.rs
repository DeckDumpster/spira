//! Build output that hand-driven work leaves behind (a hotfix build, a partition merge, a by-hand
//! testenv run) outside any worktree the landing-based reaper owns: reaped by last-write age.
//!
//! Under each configured root a target is a direct child named `target` or `*-target`, or the
//! `target` of a direct child directory that is not a bead worktree. Only that directory is ever
//! removed: the sibling logs and source checkout the child holds are never touched, and a root
//! is never a candidate itself. A target is spared while any process holds a path under it or
//! under the directory that holds it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::{busy, reap};

/// One reapable build output: the directory removed, and the directory whose liveness also
/// protects it (the checkout a build is run from, which can be busy while its target is quiet).
#[derive(Clone, Debug, PartialEq, Eq, Ord, PartialOrd)]
pub struct HandTarget {
    pub path: PathBuf,
    pub guard: PathBuf,
}

pub fn is_target_name(name: &str) -> bool {
    name == "target" || name.ends_with("-target")
}

fn real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// Every candidate under `roots`, sorted. A link is never one: removing it would not free what
/// it points at, and following it could reach outside the root.
pub fn candidates(roots: &[PathBuf]) -> Vec<HandTarget> {
    let mut out = vec![];
    for root in roots {
        let Ok(rd) = fs::read_dir(root) else { continue };
        for e in rd.flatten() {
            let (name, dir) = (e.file_name().to_string_lossy().into_owned(), e.path());
            if !real_dir(&dir) {
                continue;
            }
            if is_target_name(&name) {
                out.push(HandTarget { path: dir.clone(), guard: dir });
            } else if reap::worktree_id(&name).is_none() && real_dir(&dir.join("target")) {
                out.push(HandTarget { path: dir.join("target"), guard: dir });
            }
        }
    }
    out.sort();
    out
}

/// Whether anything under `p` (or `p`) was written at or after `cutoff`; stops at the first
/// such entry, so a live build is answered without walking it.
fn written_since(p: &Path, cutoff: SystemTime) -> bool {
    let Ok(md) = fs::symlink_metadata(p) else { return false };
    if md.modified().is_ok_and(|m| m >= cutoff) {
        return true;
    }
    md.is_dir() && fs::read_dir(p).is_ok_and(|rd| rd.flatten().any(|e| written_since(&e.path(), cutoff)))
}

/// When `t` was last written (Unix seconds), or `None` when that was within `min_idle_secs` of
/// `now` or cannot be read: the caller keeps it.
pub fn stale_written_at(t: &Path, now: SystemTime, min_idle_secs: u64) -> Option<u64> {
    let cutoff = now.checked_sub(Duration::from_secs(min_idle_secs))?;
    if written_since(t, cutoff) {
        return None;
    }
    newest_write(t)?.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn newest_write(p: &Path) -> Option<SystemTime> {
    let md = fs::symlink_metadata(p).ok()?;
    let mut acc = md.modified().ok()?;
    if md.is_dir() {
        for e in fs::read_dir(p).ok()?.flatten() {
            if let Some(m) = newest_write(&e.path()) {
                acc = acc.max(m);
            }
        }
    }
    Some(acc)
}

pub fn held(t: &HandTarget) -> bool {
    busy::worktree_busy(&t.path) || busy::worktree_busy(&t.guard)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reaped {
    pub removed: Vec<PathBuf>,
    pub bytes: u64,
    pub kept_busy: Vec<PathBuf>,
}

/// Remove every candidate under `roots` last written at least `min_idle_secs` before `now` and
/// not held by `is_held`. `is_held` is asked again immediately before each removal.
pub fn reap(roots: &[PathBuf], now: SystemTime, min_idle_secs: u64, is_held: &dyn Fn(&HandTarget) -> bool) -> Result<Reaped, String> {
    let mut out = Reaped::default();
    for t in candidates(roots) {
        if stale_written_at(&t.path, now, min_idle_secs).is_none() {
            continue;
        }
        if is_held(&t) {
            out.kept_busy.push(t.path);
            continue;
        }
        out.bytes += reap::remove_target(&t.path)?;
        out.removed.push(t.path);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn put(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn later(days: u64) -> SystemTime {
        SystemTime::now() + Duration::from_secs(days * 86_400)
    }

    const DAY: u64 = 86_400;

    fn parents(t: &Path) -> Vec<PathBuf> {
        let (scratch, var_tmp, wt) = (t.join("scratch"), t.join("var-tmp"), t.join("worktree"));
        put(&scratch.join("house-arrest/target/debug/a.o"));
        put(&scratch.join("house-arrest/logs/build.log"));
        put(&scratch.join("house-arrest/src/main.rs"));
        put(&var_tmp.join("nxsurz-target/debug/b.o"));
        put(&var_tmp.join("notes.txt"));
        put(&wt.join("hotfix-target/debug/c.o"));
        put(&wt.join("sp-abc123/target/debug/live.o"));
        vec![scratch, var_tmp, wt]
    }

    #[test]
    fn a_stale_target_under_each_parent_goes_and_its_siblings_stay() {
        let t = testkit::TempDir::new("hand-reap");
        let roots = parents(t.path());
        let r = reap(&roots, later(10), 2 * DAY, &|_| false).unwrap();
        assert_eq!(r.removed.len(), 3, "{r:?}");
        assert!(r.bytes > 0);
        for gone in ["scratch/house-arrest/target", "var-tmp/nxsurz-target", "worktree/hotfix-target"] {
            assert!(!t.path().join(gone).exists(), "{gone} survived");
        }
        for kept in ["scratch/house-arrest/logs/build.log", "scratch/house-arrest/src/main.rs", "var-tmp/notes.txt", "worktree/sp-abc123/target/debug/live.o"] {
            assert!(t.path().join(kept).exists(), "{kept} was removed");
        }
    }

    #[test]
    fn a_target_written_within_the_threshold_stays() {
        let t = testkit::TempDir::new("hand-fresh");
        let roots = parents(t.path());
        let r = reap(&roots, later(1), 2 * DAY, &|_| false).unwrap();
        assert!(r.removed.is_empty(), "{r:?}");
        assert!(t.path().join("var-tmp/nxsurz-target/debug/b.o").exists());
    }

    #[test]
    fn one_fresh_file_deep_in_a_target_keeps_the_whole_target() {
        let t = testkit::TempDir::new("hand-deep");
        let roots = parents(t.path());
        let old = SystemTime::now() - Duration::from_secs(10 * DAY);
        for p in ["var-tmp/nxsurz-target/debug/b.o", "var-tmp/nxsurz-target/debug", "var-tmp/nxsurz-target"] {
            File::open(t.path().join(p)).unwrap().set_modified(old).unwrap();
        }
        let roots = vec![roots[1].clone()];
        assert_eq!(reap(&roots, SystemTime::now(), 2 * DAY, &|_| false).unwrap().removed.len(), 1);
        put(&t.path().join("var-tmp/other-target/debug/fresh.o"));
        assert!(reap(&roots, SystemTime::now(), 2 * DAY, &|_| false).unwrap().removed.is_empty());
    }

    #[test]
    fn a_target_a_live_process_holds_is_not_removed() {
        let t = testkit::TempDir::new("hand-busy");
        let roots = parents(t.path());
        let held_file = File::open(t.path().join("var-tmp/nxsurz-target/debug/b.o")).unwrap();
        let r = reap(&roots, later(10), 2 * DAY, &|h| held(h)).unwrap();
        assert!(t.path().join("var-tmp/nxsurz-target/debug/b.o").exists(), "{r:?}");
        assert_eq!(r.kept_busy, [t.path().join("var-tmp/nxsurz-target")]);
        assert_eq!(r.removed.len(), 2, "the others are not held");
        drop(held_file);
    }

    #[test]
    fn a_checkout_a_process_works_in_protects_its_quiet_target() {
        let t = testkit::TempDir::new("hand-guard");
        let roots = parents(t.path());
        let held_file = File::open(t.path().join("scratch/house-arrest/src/main.rs")).unwrap();
        let r = reap(&roots, later(10), 2 * DAY, &|h| held(h)).unwrap();
        assert!(t.path().join("scratch/house-arrest/target").exists(), "{r:?}");
        drop(held_file);
    }

    #[test]
    fn a_root_and_a_linked_target_are_never_candidates() {
        let t = testkit::TempDir::new("hand-links");
        let elsewhere = t.path().join("elsewhere");
        put(&elsewhere.join("keep.txt"));
        let root = t.path().join("an-odd-target");
        fs::create_dir_all(root.join("proj")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("proj/target")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link-target")).unwrap();
        assert_eq!(candidates(std::slice::from_ref(&root)), vec![]);
        reap(&[root], later(10), DAY, &|_| false).unwrap();
        assert!(elsewhere.join("keep.txt").exists());
    }
}
