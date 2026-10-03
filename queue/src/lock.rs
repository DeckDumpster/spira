//! The per-repo queue lock: `$SPIRA_QUEUE_DIR/<repo>/lock`, an exclusive non-blocking
//! flock — the same file and the same lock queue verdict, batch.sh and batcher-cut's
//! `try_lock` take, so every writer of a repo's queue state is serialised by one lock.

use std::fs::{self, File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub struct Guard {
    _f: File,
}

pub enum Acquire {
    Held(Guard),
    /// Another process holds it.
    Busy,
    /// The lock file could not be opened.
    Unopenable,
}

pub fn try_lock(queue_dir: &Path, repo: &str) -> Acquire {
    try_lock_file(queue_dir, repo, "lock")
}

/// The step lock (`<repo>/step.lock`): one stepper per repository. Deliberately NOT the
/// queue lock — queue verdict and batch.sh take that one themselves inside a step.
pub fn try_step_lock(queue_dir: &Path, repo: &str) -> Acquire {
    try_lock_file(queue_dir, repo, "step.lock")
}

fn try_lock_file(queue_dir: &Path, repo: &str, file: &str) -> Acquire {
    let dir = queue_dir.join(repo);
    let _ = fs::create_dir_all(&dir);
    let f = match OpenOptions::new().create(true).write(true).truncate(false).open(dir.join(file)) {
        Ok(f) => f,
        Err(_) => return Acquire::Unopenable,
    };
    // SAFETY: flock on a descriptor we own; no memory is shared with the kernel call.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Acquire::Held(Guard { _f: f })
    } else {
        Acquire::Busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tmpdir;

    #[test]
    fn second_taker_is_refused_until_the_first_drops() {
        let _serial = crate::testutil::serial();
        let d = tmpdir("lock");
        let a = try_lock(&d, "spira");
        assert!(matches!(a, Acquire::Held(_)));
        assert!(matches!(try_lock(&d, "spira"), Acquire::Busy));
        drop(a);
        assert!(matches!(try_lock(&d, "spira"), Acquire::Held(_)));
    }

    #[test]
    fn the_step_lock_is_a_different_file_from_the_queue_lock() {
        let _serial = crate::testutil::serial();
        let d = tmpdir("steplock");
        let s = try_step_lock(&d, "spira");
        assert!(matches!(s, Acquire::Held(_)));
        // a step holding its lock never blocks the queue lock queue verdict/batch.sh take
        assert!(matches!(try_lock(&d, "spira"), Acquire::Held(_)));
        assert!(matches!(try_step_lock(&d, "spira"), Acquire::Busy));
        assert!(matches!(try_step_lock(&d, "other"), Acquire::Held(_)));
        drop(s);
        assert!(matches!(try_step_lock(&d, "spira"), Acquire::Held(_)));
        assert!(d.join("spira/step.lock").is_file());
    }
}
