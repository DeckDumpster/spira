//! The per-repo queue lock: `$SPIRA_QUEUE_DIR/<repo>/lock`, an exclusive non-blocking
//! flock — the same file and the same lock queue verdict, batch.sh and batcher-cut's
//! `try_lock` take, so every writer of a repo's queue state is serialised by one lock.
//!
//! Every hold a verb takes is recorded as it is taken and given back ([`take_holds`]), so the
//! binary pushes each verb's hold over OTLP and a test sees where each hold began and ended.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

pub struct Guard {
    f: File,
    recorded: bool,
    beat: Option<Beat>,
    hold: Option<Taken>,
}

/// A verb's hold of a repo's queue lock: taken, then given back after `held`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hold {
    Taken {
        repo: String,
        verb: String,
    },
    Released {
        repo: String,
        verb: String,
        held: Duration,
    },
}

struct Taken {
    repo: String,
    verb: String,
    at: Instant,
}

thread_local! {
    /// The holds this thread's verbs took and gave back, in order, until [`take_holds`].
    static HOLDS: RefCell<Vec<Hold>> = const { RefCell::new(Vec::new()) };
}

/// Kept per thread: a verb takes and drops its guard on the thread it runs on, and `queue` runs
/// its verb on the main thread, which drains the record once the verb returns. A guard dropped
/// as its thread exits records nothing rather than panic in a drop.
fn note(h: Hold) {
    let _ = HOLDS.try_with(|v| v.borrow_mut().push(h));
}

/// Every queue-lock hold this thread's verbs took or gave back since the last call, in order.
pub fn take_holds() -> Vec<Hold> {
    HOLDS
        .try_with(|v| std::mem::take(&mut *v.borrow_mut()))
        .unwrap_or_default()
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
    if let Ok(f) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
    {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

impl Beat {
    fn start(path: std::path::PathBuf, every: std::time::Duration) -> Beat {
        Beat::start_notifying(path, every, || {})
    }

    fn start_notifying(
        path: std::path::PathBuf,
        every: std::time::Duration,
        after_touch: impl Fn() + Send + 'static,
    ) -> Beat {
        touch(&path);
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        let p = path.clone();
        let thread = std::thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(every) {
                touch(&p);
                after_touch();
            }
        });
        Beat {
            stop,
            thread: Some(thread),
            path,
        }
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
    /// Unlocks before the descriptor closes: flock belongs to the open file description, which
    /// a child forked by any thread shares until its exec, so closing alone can leave the lock
    /// held after its guard is gone. The unlock comes last: the beat is stopped and its file
    /// removed first, so the next holder's beat is never deleted by this guard's.
    fn drop(&mut self) {
        if self.recorded {
            let _ = self.f.set_len(0);
        }
        self.beat.take();
        if let Some(t) = self.hold.take() {
            note(Hold::Released {
                repo: t.repo,
                verb: t.verb,
                held: t.at.elapsed(),
            });
        }
        // SAFETY: flock on a descriptor we own; no memory is shared with the kernel call.
        unsafe { libc::flock(self.f.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub enum Acquire {
    Held(Guard),
    /// Another process holds it.
    Busy,
    /// The lock file could not be opened.
    Unopenable,
}

/// The lock that serialises release activations of `repo`'s landings
/// (`<queue dir>/<repo>/lock.activate`), waited on without a bound: an activation is bounded
/// by its own timeouts, and it is held apart from the queue lock so a claim or a round verb never
/// waits behind one (over-broad-scopes #3, sp-ipwlic). `None` when the file cannot be opened.
pub fn activation_lock(queue_dir: &Path, repo: &str) -> Option<File> {
    let dir = queue_dir.join(repo);
    let _ = fs::create_dir_all(&dir);
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join("lock.activate"))
        .ok()?;
    // SAFETY: flock on a descriptor we own; no memory is shared with the kernel call.
    (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } == 0).then_some(f)
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
    try_lock_file(
        queue_dir,
        repo,
        &format!("certify-{batch}.lock"),
        Some("round certify"),
    )
}

/// The certify lock's holder record (`pid=<pid> verb=...`); empty when none is recorded.
pub fn certify_holder(queue_dir: &Path, repo: &str, batch: &str) -> String {
    fs::read_to_string(queue_dir.join(repo).join(format!("certify-{batch}.lock")))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Deletes the certify lock of a round that has ended. A lock another process holds is kept:
/// unlinking it would let a second certify take a fresh file beside the held one.
pub fn remove_certify_lock(queue_dir: &Path, repo: &str, batch: &str) -> bool {
    let path = queue_dir.join(repo).join(format!("certify-{batch}.lock"));
    let Ok(f) = OpenOptions::new().write(true).open(&path) else {
        return false;
    };
    let mine =
        certify_holder(queue_dir, repo, batch).starts_with(&format!("pid={} ", std::process::id()));
    // SAFETY: flock on a descriptor we own; no memory is shared with the kernel call.
    let free = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    (free || mine) && fs::remove_file(&path).is_ok()
}

/// The batch of every certify lock file `repo` has on disk, sorted.
pub fn certify_lock_batches(queue_dir: &Path, repo: &str) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(queue_dir.join(repo))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_prefix("certify-")?
                .strip_suffix(".lock")
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out
}

/// The step lock (`<repo>/step.lock`): one stepper per repository. Deliberately NOT the
/// queue lock — queue verdict and batch.sh take that one themselves inside a step.
pub fn try_step_lock(queue_dir: &Path, repo: &str) -> Acquire {
    try_lock_file(queue_dir, repo, "step.lock", None)
}

/// Held by a `round stage` for its whole run, so a second stage is refused while the first one's
/// gate runs without the queue lock.
pub fn try_stage_lock(queue_dir: &Path, repo: &str) -> Acquire {
    try_lock_file(queue_dir, repo, "stage.lock", None)
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
        let queue_lock = file == "lock" && verb.is_some();
        let beat = queue_lock.then(|| Beat::start(dir.join(BEAT_FILE), BEAT_EVERY));
        let hold = verb.filter(|_| queue_lock).map(|v| {
            note(Hold::Taken {
                repo: repo.into(),
                verb: v.into(),
            });
            Taken {
                repo: repo.into(),
                verb: v.into(),
                at: Instant::now(),
            }
        });
        Acquire::Held(Guard {
            f,
            recorded: true,
            beat,
            hold,
        })
    } else {
        Acquire::Busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tmpdir;

    /// A forked child holds a copy of every descriptor until its exec closes it, and flock
    /// belongs to the open file description that copy shares: a duplicate stands in for one.
    #[test]
    fn a_dropped_lock_is_free_while_a_forked_copy_of_its_descriptor_lives() {
        let d = tmpdir("lockfork");
        for take in [try_lock as fn(&Path, &str) -> Acquire, try_step_lock] {
            let Acquire::Held(g) = take(&d, "spira") else {
                panic!("not held")
            };
            // SAFETY: dup of a descriptor we own; closed below.
            let copy = unsafe { libc::dup(g.f.as_raw_fd()) };
            assert!(copy >= 0);
            drop(g);
            assert!(
                matches!(take(&d, "spira"), Acquire::Held(_)),
                "the copy kept the lock"
            );
            // SAFETY: closing the duplicate made above.
            unsafe { libc::close(copy) };
        }
    }

    #[test]
    fn second_taker_is_refused_until_the_first_drops() {
        let d = tmpdir("lock");
        let a = try_lock(&d, "spira");
        assert!(matches!(a, Acquire::Held(_)));
        assert!(matches!(try_lock(&d, "spira"), Acquire::Busy));
        drop(a);
        assert!(matches!(try_lock(&d, "spira"), Acquire::Held(_)));
    }

    #[test]
    fn a_verb_s_hold_of_the_queue_lock_is_recorded_taken_then_released_and_nothing_else_is() {
        let d = tmpdir("lockhold");
        take_holds();
        let g = try_lock_as(&d, "spira", "round open");
        assert!(matches!(
            try_lock_as(&d, "spira", "round abandon"),
            Acquire::Busy
        ));
        assert_eq!(
            take_holds(),
            [Hold::Taken {
                repo: "spira".into(),
                verb: "round open".into()
            }],
            "a refused taker records nothing"
        );
        drop(g);
        let got = take_holds();
        assert!(
            matches!(got.as_slice(), [Hold::Released { repo, verb, .. }] if repo == "spira" && verb == "round open"),
            "{got:?}"
        );
        drop(try_lock(&d, "spira"));
        drop(try_certify_lock(&d, "spira", "b1"));
        drop(try_step_lock(&d, "spira"));
        assert_eq!(
            take_holds(),
            [],
            "an unrecorded taker, the certify lock and the step lock are no verb's queue-lock hold"
        );
    }

    #[test]
    fn a_recording_queue_lock_holder_beats_until_it_drops() {
        let d = tmpdir("lockbeat");
        let a = try_lock_as(&d, "spira", "land-local");
        assert!(d.join("spira").join(BEAT_FILE).is_file());
        drop(a);
        assert!(!d.join("spira").join(BEAT_FILE).exists());
        let (tx, ticked) = std::sync::mpsc::channel::<()>();
        let b = Beat::start_notifying(
            d.join("beat"),
            std::time::Duration::from_millis(20),
            move || {
                let _ = tx.send(());
            },
        );
        let t0 = fs::metadata(d.join("beat")).unwrap().modified().unwrap();
        ticked.recv().unwrap();
        assert!(fs::metadata(d.join("beat")).unwrap().modified().unwrap() >= t0);
        drop(b);
    }

    #[test]
    fn a_recorded_holder_is_named_while_held_and_cleared_on_drop() {
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
        let d = tmpdir("certlock");
        let a = try_certify_lock(&d, "spira", "r-1");
        assert!(matches!(a, Acquire::Held(_)));
        assert!(matches!(
            try_certify_lock(&d, "spira", "r-1"),
            Acquire::Busy
        ));
        assert!(
            certify_holder(&d, "spira", "r-1").starts_with(&format!("pid={}", std::process::id()))
        );
        assert!(matches!(
            try_certify_lock(&d, "spira", "r-2"),
            Acquire::Held(_)
        ));
        assert!(matches!(try_lock(&d, "spira"), Acquire::Held(_)));
        drop(a);
        assert!(matches!(
            try_certify_lock(&d, "spira", "r-1"),
            Acquire::Held(_)
        ));
    }

    #[test]
    fn an_ended_rounds_certify_lock_is_removed_unless_another_process_holds_it() {
        let d = tmpdir("certrm");
        drop(try_certify_lock(&d, "spira", "r-1"));
        let held = try_certify_lock(&d, "spira", "r-2");
        let ours = try_certify_lock(&d, "spira", "r-3");
        fs::write(
            d.join("spira/certify-r-2.lock"),
            "pid=1 verb=round certify\n",
        )
        .unwrap();
        assert_eq!(certify_lock_batches(&d, "spira"), ["r-1", "r-2", "r-3"]);
        assert!(remove_certify_lock(&d, "spira", "r-1"), "a free lock goes");
        assert!(
            !remove_certify_lock(&d, "spira", "r-2"),
            "another holder's lock stays"
        );
        assert!(
            remove_certify_lock(&d, "spira", "r-3"),
            "this process's own certify ended the round"
        );
        assert!(
            !remove_certify_lock(&d, "spira", "r-9"),
            "no file, nothing removed"
        );
        assert_eq!(certify_lock_batches(&d, "spira"), ["r-2"]);
        drop((held, ours));
    }

    #[test]
    fn the_step_lock_is_a_different_file_from_the_queue_lock() {
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
