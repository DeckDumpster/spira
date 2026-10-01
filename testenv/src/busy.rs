//! A second, structural guard against reaping a worktree still in flight (sp-x9kbg): any
//! live process with the worktree as its cwd, or holding a file open anywhere inside it, is
//! reason enough to keep it — independent of what [`crate::landed`] concluded. Best effort:
//! a `/proc/<pid>` entry this process cannot read (raced exit, permissions) is skipped, not
//! assumed either busy or free — the landed-ness check is the gate of record; this is only
//! a net under it.

use std::fs;
use std::path::Path;

fn resolves_under(link: &Path, dir: &Path) -> bool {
    fs::read_link(link).map(|target| target.starts_with(dir)).unwrap_or(false)
}

/// Whether any process on this box has `dir` (or anything under it) as its cwd, or as an
/// open file descriptor. `dir` not existing, or `/proc` not being readable at all, reads as
/// "not busy" — there is nothing left to protect, or there is no way to tell and the
/// landed-ness check already decided.
pub fn worktree_busy(dir: &Path) -> bool {
    let Ok(dir) = dir.canonicalize() else { return false };
    let Ok(procs) = fs::read_dir("/proc") else { return false };
    for e in procs.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let base = Path::new("/proc").join(pid.to_string());
        if resolves_under(&base.join("cwd"), &dir) {
            return true;
        }
        if let Ok(fds) = fs::read_dir(base.join("fd")) {
            for fd in fds.flatten() {
                if resolves_under(&fd.path(), &dir) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    fn wait_until(mut pred: impl FnMut() -> bool, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            if pred() {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn spawn_sleeping_in(dir: &Path) -> Child {
        Command::new("sleep").arg("30").current_dir(dir).spawn().expect("spawn sleep")
    }

    #[test]
    fn an_empty_or_missing_directory_is_never_busy() {
        let t = testkit::TempDir::new("busy-empty");
        assert!(!worktree_busy(t.path()));
        assert!(!worktree_busy(&t.path().join("does-not-exist")));
    }

    #[test]
    fn a_live_process_cwd_inside_the_dir_makes_it_busy_until_it_exits() {
        let t = testkit::TempDir::new("busy-cwd");
        let mut child = spawn_sleeping_in(t.path());
        assert!(wait_until(|| worktree_busy(t.path()), Duration::from_secs(2)), "the live sleep's cwd should be seen");
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(wait_until(|| !worktree_busy(t.path()), Duration::from_secs(2)), "busy-ness should clear once the process is gone");
    }

    #[test]
    fn a_process_busy_elsewhere_does_not_make_an_unrelated_dir_busy() {
        let t = testkit::TempDir::new("busy-elsewhere");
        let other = testkit::TempDir::new("busy-elsewhere-other");
        let mut child = spawn_sleeping_in(other.path());
        assert!(wait_until(|| worktree_busy(other.path()), Duration::from_secs(2)));
        assert!(!worktree_busy(t.path()), "a process busy in one directory must not mark an unrelated one busy");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
