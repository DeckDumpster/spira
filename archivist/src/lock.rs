//! Two file locks, held for the duration of one archive run — the same shape
//! `queue::lock` uses for the per-repo queue lock. Both are released the moment the
//! process holding them exits, however it exits, because the guard is a file descriptor
//! closed on drop rather than a flag a trap has to remember to clear.
//!
//! THE ARCHIVIST-WIDE LOCK: one archive at a time across both entry points (`sweep` and
//! `now`). The sweep takes it non-blocking and skips the session if busy; the manual
//! `now` path waits, because an operator told "busy, try later" will simply run it again
//! in a loop.
//!
//! THE PER-SESSION LOCK: prevents two callers archiving the SAME session at once.
//! Non-blocking always — a caller that cannot take it reads "this pass did not archive
//! this session" and moves on, it never waits for another archive of the same session to
//! finish.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub struct Guard {
    _f: File,
}

pub enum Acquire {
    Held(Guard),
    Busy,
    Unopenable,
}

fn open(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    OpenOptions::new().create(true).write(true).truncate(false).open(path)
}

/// Non-blocking: `Busy` immediately if another process holds it.
pub fn try_lock(path: &Path) -> Acquire {
    let f = match open(path) {
        Ok(f) => f,
        Err(_) => return Acquire::Unopenable,
    };
    // SAFETY: flock on a descriptor this process owns exclusively; no shared memory.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Acquire::Held(Guard { _f: f })
    } else {
        Acquire::Busy
    }
}

/// Blocking: waits for the lock to become free.
pub fn wait_lock(path: &Path) -> Acquire {
    let f = match open(path) {
        Ok(f) => f,
        Err(_) => return Acquire::Unopenable,
    };
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Acquire::Held(Guard { _f: f })
    } else {
        Acquire::Busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// sp-os3of: used to take the FIRST lock in-process (`try_lock(&p)`), assert a second
    /// `try_lock` is `Busy`, `drop` the first, then assert a third `try_lock` is `Held` —
    /// the drop-then-assert-released shape that flipped
    /// `spira-config::admission::tests::gate_occupancy_reads_flocks_and_their_holder_sidecars`
    /// at round 209 (a concurrent fork elsewhere in the binary can duplicate an in-process
    /// fd and keep the flock held past this process's own drop). Converted the same way:
    /// the first holder is a CHILD PROCESS this test owns, so this process never itself
    /// holds the lock and no fork anywhere in the binary can inherit a copy of it. Also
    /// more faithful to the real contract — in production, two different *processes*
    /// contend for the archivist lock, never one process calling `try_lock` on itself twice.
    #[test]
    fn a_second_try_lock_is_refused_until_the_first_drops() {
        let dir = testkit::TempDir::new("archivist-lock");
        let p = dir.path().join("archivist.lock");
        let mut holder = testkit::ChildGuard::spawn(Command::new("flock").arg("-x").arg(&p).arg("sleep").arg("60"));
        // Wait for the child to actually have the lock before asserting Busy: a Held
        // return here would otherwise hand this process a competing in-process guard.
        let mut tries = 0;
        loop {
            match try_lock(&p) {
                Acquire::Busy => break,
                Acquire::Held(g) => {
                    drop(g);
                    assert!(tries < 500, "the child never took the lock on {}", p.display());
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Acquire::Unopenable => panic!("could not open {}", p.display()),
            }
        }
        assert!(matches!(try_lock(&p), Acquire::Busy));
        // Kill and reap the child before the release assertion. `flock -x <file> sleep 60`
        // itself forks: the grandchild `sleep` is the actual holder, inheriting the
        // locked fd across the launcher's exec. `kill()` SIGKILLs the whole process
        // group (launcher and grandchild alike) and reaps the launcher, but the
        // grandchild's own fd-closing teardown is a separate task the kernel schedules
        // independently — same signal, not provably the same instant — so poll briefly
        // rather than asserting the instant `kill()` returns.
        holder.kill();
        let mut tries = 0;
        let mut last = try_lock(&p);
        while matches!(last, Acquire::Busy) {
            assert!(tries < 500, "the lock was never released after the holder was killed");
            tries += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
            last = try_lock(&p);
        }
        assert!(matches!(last, Acquire::Held(_)));
    }

    #[test]
    fn two_different_lock_files_do_not_contend() {
        let dir = testkit::TempDir::new("archivist-lock");
        let wide = try_lock(&dir.path().join("archivist.lock"));
        let per_session = try_lock(&dir.path().join("sess-1.lock"));
        assert!(matches!(wide, Acquire::Held(_)));
        assert!(matches!(per_session, Acquire::Held(_)));
    }
}
