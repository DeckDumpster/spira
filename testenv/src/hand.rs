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
        put(&scratch.join("proj/target/debug/a.o"));
        put(&scratch.join("proj/logs/build.log"));
        put(&scratch.join("proj/src/main.rs"));
        put(&var_tmp.join("old-target/debug/b.o"));
        put(&var_tmp.join("notes.txt"));
        put(&wt.join("spare-target/debug/c.o"));
        put(&wt.join("sp-abc123/target/debug/live.o"));
        vec![scratch, var_tmp, wt]
    }

    #[test]
    fn only_a_target_unwritten_past_the_threshold_is_stale() {
        let t = testkit::TempDir::new("hand-stale");
        let roots = parents(t.path());
        let target = t.path().join("var-tmp/old-target");
        assert!(stale_written_at(&target, later(10), 2 * DAY).is_some());
        assert!(stale_written_at(&target, later(1), 2 * DAY).is_none());
        let old = SystemTime::now() - Duration::from_secs(10 * DAY);
        for p in ["debug/b.o", "debug", ""] {
            File::open(target.join(p)).unwrap().set_modified(old).unwrap();
        }
        assert!(stale_written_at(&target, SystemTime::now(), 2 * DAY).is_some());
        put(&target.join("debug/deep/fresh.o"));
        assert!(stale_written_at(&target, SystemTime::now(), 2 * DAY).is_none(), "one fresh file keeps the whole target");
        assert_eq!(candidates(&roots).len(), 3);
    }

    #[test]
    fn a_target_or_its_checkout_a_process_holds_is_held() {
        let t = testkit::TempDir::new("hand-held");
        let roots = parents(t.path());
        let find = |name: &str| candidates(&roots).into_iter().find(|c| c.path.ends_with(name)).unwrap();
        let (quiet, scratch) = (find("proj/target"), find("var-tmp/old-target"));
        assert!(!held(&quiet) && !held(&scratch));
        let _in_target = File::open(scratch.path.join("debug/b.o")).unwrap();
        let _in_checkout = File::open(t.path().join("scratch/proj/src/main.rs")).unwrap();
        assert!(held(&scratch), "a file open under the target");
        assert!(held(&quiet), "a file open in the checkout that builds into it");
    }

    #[test]
    fn a_bead_worktree_and_a_linked_target_are_never_candidates() {
        let t = testkit::TempDir::new("hand-links");
        let roots = parents(t.path());
        assert!(candidates(&roots).iter().all(|c| !c.path.starts_with(t.path().join("worktree/sp-abc123"))));
        let elsewhere = t.path().join("elsewhere");
        put(&elsewhere.join("keep.txt"));
        let root = t.path().join("an-odd-target");
        fs::create_dir_all(root.join("proj")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("proj/target")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("link-target")).unwrap();
        assert_eq!(candidates(std::slice::from_ref(&root)), vec![]);
    }
}
