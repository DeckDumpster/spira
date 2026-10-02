//! The tail reader lock (`_wd_tail_lockfile`/`_wd_tail_holder`): a kernel `flock`, never a
//! pid file — the kernel drops it when the holder's last fd closes, which covers a crash, a
//! kill and a session that simply went away, the three cases a pid file gets wrong. ONE
//! READER PER WATCHER is the contract, not a convenience: two concurrent tails would each
//! mark lines read on the other's behalf and both would deliver every line.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

/// Tries to take the lock without blocking. `Ok(file)` holds it for as long as `file` lives;
/// `Err(())` means somebody else already has it.
pub fn try_lock(path: &Path) -> std::io::Result<Result<File, ()>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = OpenOptions::new().create(true).append(true).read(true).open(path)?;
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Ok(f))
    } else {
        Ok(Err(()))
    }
}

/// Polls for the lock with a deadline (the Rust equivalent of `flock -w <secs>`; `libc`
/// exposes no blocking-with-timeout form). Returns the file once acquired, or `None` past
/// the deadline.
pub fn wait_for_lock(path: &Path, timeout: Duration) -> std::io::Result<Option<File>> {
    let deadline = Instant::now() + timeout;
    loop {
        match try_lock(path)? {
            Ok(f) => return Ok(Some(f)),
            Err(()) => {
                if Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Records the pid holding the lock, for the message another reader prints — a courtesy;
/// the kernel, not this line, is what decides who holds it.
pub fn record_holder(file: &mut File) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "{}", std::process::id())
}

/// The pid recorded in a lock file we do NOT hold, or `"?"` if it cannot be read — a
/// courtesy for the refusal message, never trusted for anything that decides behaviour.
pub fn read_holder_pid(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::TempDir;

    #[test]
    fn a_free_lock_is_taken() {
        let d = TempDir::new("watchd-lock");
        let p = d.join("x.tail.lock");
        assert!(try_lock(&p).unwrap().is_ok());
    }

    #[test]
    fn a_held_lock_refuses_a_second_reader() {
        let d = TempDir::new("watchd-lock");
        let p = d.join("x.tail.lock");
        let _held = try_lock(&p).unwrap().unwrap();
        assert!(try_lock(&p).unwrap().is_err());
    }

    /// sp-os3of: used to take the lock in-process, drop it at the end of a scope, then
    /// assert a fresh `try_lock` succeeds — the drop-then-assert-released shape that
    /// flipped `spira-config::admission::tests::gate_occupancy_reads_flocks_and_their_holder_sidecars`
    /// at round 209 (a concurrent fork elsewhere in the binary can duplicate an in-process
    /// fd and keep the flock held past this process's own drop). Converted the same way:
    /// the holder is a CHILD PROCESS this test owns, so this process never itself holds
    /// the lock and no fork anywhere in the binary can inherit a copy of it.
    #[test]
    fn the_lock_is_free_again_once_the_holder_is_dropped() {
        let d = TempDir::new("watchd-lock");
        let p = d.join("x.tail.lock");
        let mut holder = testkit::ChildGuard::spawn(std::process::Command::new("flock").arg("-x").arg(&p).arg("sleep").arg("60"));
        // Wait for the child to actually have the lock: a successful try_lock here just
        // hands this process its own brief, immediately-dropped hold.
        let mut tries = 0;
        while try_lock(&p).unwrap().is_ok() {
            assert!(tries < 500, "the child never took the lock on {}", p.display());
            tries += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Kill and reap the child before the release assertion. `flock -x <file> sleep 60`
        // itself forks: the grandchild `sleep` is the actual holder, inheriting the
        // locked fd across the launcher's exec. `kill()` SIGKILLs the whole process
        // group (launcher and grandchild alike) and reaps the launcher, but the
        // grandchild's own fd-closing teardown is a separate task the kernel schedules
        // independently — same signal, not provably the same instant — so poll briefly
        // rather than asserting the instant `kill()` returns.
        holder.kill();
        let mut tries = 0;
        let mut last = try_lock(&p).unwrap();
        while last.is_err() {
            assert!(tries < 500, "the lock was never released after the holder was killed");
            tries += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
            last = try_lock(&p).unwrap();
        }
        assert!(last.is_ok());
    }

    #[test]
    fn wait_for_lock_times_out_on_a_held_lock() {
        let d = TempDir::new("watchd-lock");
        let p = d.join("x.tail.lock");
        let _held = try_lock(&p).unwrap().unwrap();
        let start = Instant::now();
        let got = wait_for_lock(&p, Duration::from_millis(150)).unwrap();
        assert!(got.is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn wait_for_lock_succeeds_once_the_holder_releases_it() {
        let d = TempDir::new("watchd-lock");
        let p = d.join("x.tail.lock");
        let held = try_lock(&p).unwrap().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
        });
        let got = wait_for_lock(&p, Duration::from_secs(5)).unwrap();
        assert!(got.is_some());
    }

    #[test]
    fn holder_pid_is_recorded_and_read_back() {
        let d = TempDir::new("watchd-lock");
        let p = d.join("x.tail.lock");
        let mut f = try_lock(&p).unwrap().unwrap();
        record_holder(&mut f).unwrap();
        assert_eq!(read_holder_pid(&p), std::process::id().to_string());
    }

    #[test]
    fn an_unreadable_or_garbage_pid_reads_as_unknown() {
        let d = TempDir::new("watchd-lock");
        assert_eq!(read_holder_pid(&d.join("nope")), "?");
        std::fs::write(d.join("garbage"), "not-a-pid\n").unwrap();
        assert_eq!(read_holder_pid(&d.join("garbage")), "?");
    }
}
