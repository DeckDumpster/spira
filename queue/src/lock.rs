//! The per-repo queue lock: `$SPIRA_QUEUE_DIR/<repo>/lock`, an exclusive non-blocking
//! flock — the same file and the same lock verdict.sh, batch.sh and batcher-cut's
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
    let dir = queue_dir.join(repo);
    let _ = fs::create_dir_all(&dir);
    let f = match OpenOptions::new().create(true).write(true).truncate(false).open(dir.join("lock")) {
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
}
