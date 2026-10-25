//! The per-repo queue lock: `$SPIRA_QUEUE_DIR/<repo>/lock`, an exclusive non-blocking
//! flock — the same file and the same lock queue verdict, batch.sh and batcher-cut's
//! `try_lock` take, so every writer of a repo's queue state is serialised by one lock.

use std::fs::{self, File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub struct Guard {
    f: File,
    recorded: bool,
    beat: Option<Beat>,
}

/// A recording holder touches `<lock>.beat` while it lives, so the reconciler tells a landing
/// that is still working from a holder that died with the lock held.
const BEAT_FILE: &str = "lock.beat";
const BEAT_EVERY: std::time::Duration = std::time::Duration::from_secs(20);

struct Beat {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    path: std::path::PathBuf,
}

fn touch(path: &Path) {
    if let Ok(f) = OpenOptions::new().create(true).write(true).truncate(false).open(path) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

impl Beat {
    fn start(path: std::path::PathBuf, every: std::time::Duration) -> Beat {
        touch(&path);
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        let p = path.clone();
        let thread = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(every) {
                touch(&p);
            }
        });
        Beat { stop, thread: Some(thread), path }
    }
}

impl Drop for Beat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.recorded {
            let _ = self.f.set_len(0);
        }
        self.beat.take();
    }
}

pub enum Acquire {
    Held(Guard),
    /// Another process holds it.
    Busy,
    /// The lock file could not be opened.
    Unopenable,
}

pub fn try_lock(queue_dir: &Path, repo: &str) -> Acquire {
    try_lock_file(queue_dir, repo, "lock", None)
}

/// As `try_lock`, recording `pid=<pid> verb=<verb>` in the lock file for the life of the guard,
/// so a waiter that times out can name who it waited on.
pub fn try_lock_as(queue_dir: &Path, repo: &str, verb: &str) -> Acquire {
    try_lock_file(queue_dir, repo, "lock", Some(verb))
}

/// The holder record of a held lock; empty when the holder does not record one.
pub fn holder(queue_dir: &Path, repo: &str) -> String {
    fs::read_to_string(queue_dir.join(repo).join("lock"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The per-batch certify lock (`<repo>/certify-<batch>.lock`), held for a whole `round certify`.
/// `Busy` carries the holder's record: another certify of the batch is running.
pub fn try_certify_lock(queue_dir: &Path, repo: &str, batch: &str) -> Acquire {
    try_lock_file(queue_dir, repo, &format!("certify-{batch}.lock"), Some("round certify"))
}

/// The certify lock's holder record (`pid=<pid> verb=...`); empty when none is recorded.
pub fn certify_holder(queue_dir: &Path, repo: &str, batch: &str) -> String {
    fs::read_to_string(queue_dir.join(repo).join(format!("certify-{batch}.lock")))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The step lock (`<repo>/step.lock`): one stepper per repository. Deliberately NOT the
/// queue lock — queue verdict and batch.sh take that one themselves inside a step.
pub fn try_step_lock(queue_dir: &Path, repo: &str) -> Acquire {
    try_lock_file(queue_dir, repo, "step.lock", None)
}

fn try_lock_file(queue_dir: &Path, repo: &str, file: &str, verb: Option<&str>) -> Acquire {
    let dir = queue_dir.join(repo);
    let _ = fs::create_dir_all(&dir);
    let f = match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(file))
    {
        Ok(f) => f,
        Err(_) => return Acquire::Unopenable,
    };
    // SAFETY: flock on a descriptor we own; no memory is shared with the kernel call.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        let _ = f.set_len(0);
        if let Some(v) = verb {
            use std::io::Write;
            let _ = (&f).write_all(format!("pid={} verb={v}\n", std::process::id()).as_bytes());
        }
        let beat = (file == "lock" && verb.is_some())
            .then(|| Beat::start(dir.join(BEAT_FILE), BEAT_EVERY));
        Acquire::Held(Guard { f, recorded: true, beat })
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
    fn a_recording_queue_lock_holder_beats_until_it_drops() {
        let _serial = crate::testutil::serial();
        let d = tmpdir("lockbeat");
        let a = try_lock_as(&d, "spira", "land-local");
        assert!(d.join("spira").join(BEAT_FILE).is_file());
        drop(a);
        assert!(!d.join("spira").join(BEAT_FILE).exists());
        let b = Beat::start(d.join("beat"), std::time::Duration::from_millis(20));
        let t0 = fs::metadata(d.join("beat")).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(fs::metadata(d.join("beat")).unwrap().modified().unwrap() > t0);
        drop(b);
    }

    #[test]
    fn a_recorded_holder_is_named_while_held_and_cleared_on_drop() {
        let _serial = crate::testutil::serial();
        let d = tmpdir("lockholder");
        let a = try_lock_as(&d, "spira", "round certify");
        assert!(matches!(a, Acquire::Held(_)));
        assert_eq!(
            holder(&d, "spira"),
            format!("pid={} verb=round certify", std::process::id())
        );
        drop(a);
        assert_eq!(holder(&d, "spira"), "");
        let b = try_lock(&d, "spira");
        assert_eq!(
            holder(&d, "spira"),
            "",
            "an unrecording taker leaves no stale record"
        );
        drop(b);
    }

    #[test]
    fn a_batch_certify_lock_is_per_batch_and_names_its_holder() {
        let _serial = crate::testutil::serial();
        let d = tmpdir("certlock");
        let a = try_certify_lock(&d, "spira", "r-1");
        assert!(matches!(a, Acquire::Held(_)));
        assert!(matches!(try_certify_lock(&d, "spira", "r-1"), Acquire::Busy));
        assert!(certify_holder(&d, "spira", "r-1").starts_with(&format!("pid={}", std::process::id())));
        assert!(matches!(try_certify_lock(&d, "spira", "r-2"), Acquire::Held(_)));
        assert!(matches!(try_lock(&d, "spira"), Acquire::Held(_)));
        drop(a);
        assert!(matches!(try_certify_lock(&d, "spira", "r-1"), Acquire::Held(_)));
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
