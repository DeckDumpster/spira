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

    #[test]
    fn a_second_try_lock_is_refused_until_the_first_drops() {
        let dir = testkit::TempDir::new("archivist-lock");
        let p = dir.path().join("archivist.lock");
        let a = try_lock(&p);
        assert!(matches!(a, Acquire::Held(_)));
        assert!(matches!(try_lock(&p), Acquire::Busy));
        drop(a);
        assert!(matches!(try_lock(&p), Acquire::Held(_)));
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
