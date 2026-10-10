//! gate-worker: the local gate, off the landing pass's clock. Up to N workers each drain
//! the queue (`landing_pass::gateq`) a branch at a time — one worker never runs two gates
//! at once, so two branches for one gate tree are never gated together by *that* worker,
//! and N is exactly `certify_par` (DESIGN.md §8 D14): the host's own gate-admission slots
//! (`run/gate-admission`) already bound how many gates may run together, so draining the
//! queue with N workers instead of one is safe at the host level (sp-kbjv6 "wave N
//! concurrent drains"). Each worker files each verdict it gates; the pass applies it:
//! attribution of the four outcomes stays in `landing_pass::pass::certify_judge`.

use landing_pass::gateq::{Done, GateQueue, Job};
use landing_pass::model::{GateOutcome, GateRun, GATE_NOVERDICT};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// `certify_par` (0 or absent reads as 1): the number of worker slots to try. Never a new
/// env var — `certify_par` is the one knob that already governs how many gates run at once
/// (`landing_pass::model::certify_par`, `landing_pass::pass::walk_concurrent`).
pub fn worker_count(certify_par: usize) -> usize {
    certify_par.max(1)
}

/// Slot 0 keeps the pre-existing `worker.lock` name; every other slot gets its own numbered
/// lock file. All N are plain sibling files under the same `gate-worker/` run directory.
pub fn lock_path(dir: &Path, slot: usize) -> PathBuf {
    if slot == 0 {
        dir.join("worker.lock")
    } else {
        dir.join(format!("worker.lock.{slot}"))
    }
}

/// Try slots `0..n` in order and keep the first free one (`LOCK_EX|LOCK_NB`, same idiom as
/// the single-lock design this replaces). The held `File` must stay alive for the lock to
/// stay held — dropping it releases the flock. None: every slot is taken.
pub fn acquire_slot(dir: &Path, n: usize) -> Option<(usize, std::fs::File)> {
    for slot in 0..n.max(1) {
        let p = lock_path(dir, slot);
        let Ok(f) = OpenOptions::new().create(true).write(true).truncate(false).open(&p) else { continue };
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some((slot, f));
        }
    }
    None
}

/// Whether `path` lies in the release its releases directory's `current` link names. The
/// release root is the nearest ancestor with a sibling `current` symlink, found on the
/// canonical path so a path spelled through `current` itself still names its real release.
/// A path under no such directory (a dev checkout) is never superseded.
pub fn release_is_current(path: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(path) else { return true };
    for root in real.ancestors() {
        let Some(parent) = root.parent() else { break };
        let link = parent.join("current");
        if std::fs::symlink_metadata(&link).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            return std::fs::canonicalize(&link).map(|cur| cur == root).unwrap_or(true);
        }
    }
    true
}

pub trait Gate {
    fn gate(&self, branch: &str, repo: &str, lock_wait: &str, bead: &str) -> (i32, String);
}

pub trait Branches {
    /// The branch's tip now; None when the branch (or its repository) is gone.
    fn tip(&self, repo: &str, branch: &str) -> Option<String>;
}

pub trait Clock {
    fn now_ms(&self) -> u64;
}

/// A gate lock holder may legitimately run two full trials, so a waiter waits for both.
pub fn lock_wait(timeout: Option<&str>, explicit: Option<&str>) -> u64 {
    let t: u64 = timeout.and_then(|v| v.trim().parse().ok()).filter(|n| *n > 0).unwrap_or(2700);
    let floor = 2 * t;
    explicit.and_then(|v| v.trim().parse::<u64>().ok()).map_or(floor, |e| e.max(floor))
}

/// The one place a status is read as a verdict. Only a status the gate itself chose may
/// reach `Fail`: not finding the gate, being unable to start it, or its being killed says
/// nothing about the branch, so each is NO_VERDICT.
pub fn classify(rc: i32, out: String) -> GateRun {
    let gate_chose = matches!(rc, 0 | 75 | 76) || (1..126).contains(&rc);
    if gate_chose {
        return GateRun::parse(rc, out);
    }
    let why = if rc < 0 { "gate-did-not-start" } else if rc == 126 || rc == 127 { "gate-not-found" } else { "gate-killed" };
    let out = format!("{out}\ngate: VERDICT=NO_VERDICT reason={why}\ngate-worker: gate status {rc} — the harness, not the branch\n");
    GateRun::parse(GATE_NOVERDICT, out)
}

pub struct Worker<'a> {
    pub queue: &'a GateQueue,
    pub gate: &'a dyn Gate,
    pub branches: &'a dyn Branches,
    pub clock: &'a dyn Clock,
    pub lock_wait: u64,
    pub log: &'a dyn Fn(&str),
    /// Which of the N worker-lock slots this instance holds. Scopes `recover` and the
    /// claimed-job bookkeeping to this slot alone, so concurrent slots never step on each
    /// other's in-flight claims.
    pub slot: usize,
}

impl Worker<'_> {
    /// Gate every queued job this slot can claim, one at a time. Returns how many verdicts
    /// were filed. Concurrency across the queue comes from running several `Worker`s (one
    /// per process, one per slot) at once, never from parallelizing inside one drain.
    pub fn drain(&self) -> usize {
        self.drain_while(&|| true)
    }

    /// `drain`, but checks `keep_going` before each claim: a worker whose release has been
    /// superseded finishes its in-flight job and exits, so the next tick runs the new code.
    pub fn drain_while(&self, keep_going: &dyn Fn() -> bool) -> usize {
        self.queue.recover(self.slot);
        let mut filed = 0;
        while keep_going() {
            let Some(job) = self.queue.claim(self.slot) else { break };
            if self.run(&job) {
                filed += 1;
            }
        }
        filed
    }

    fn run(&self, job: &Job) -> bool {
        match self.branches.tip(&job.repo, &job.branch) {
            None => {
                (self.log)(&format!("gate-worker: {} in {} is gone — dropping its job", job.branch, job.repo));
                self.queue.discard(self.slot, job);
                return false;
            }
            Some(t) if t != job.tip => {
                (self.log)(&format!("gate-worker: {} moved from {} to {t} — dropping the stale job", job.branch, job.tip));
                self.queue.discard(self.slot, job);
                return false;
            }
            Some(_) => {}
        }
        let started_ms = self.clock.now_ms();
        let (rc, out) = self.gate.gate(&job.branch, &job.repo, &self.lock_wait.to_string(), &job.bead);
        let run = classify(rc, out);
        let finished_ms = self.clock.now_ms();
        (self.log)(&format!(
            "gate-worker: {} in {} at {} → {} in {}ms",
            job.branch,
            job.repo,
            job.tip,
            run.outcome.word(),
            finished_ms.saturating_sub(started_ms)
        ));
        if run.outcome != GateOutcome::Pass {
            match self.queue.retain_output(job, &run.out) {
                Ok(p) => (self.log)(&format!("gate-worker: {} — the gate's whole output is kept at {}", job.branch, p.display())),
                Err(e) => (self.log)(&format!("gate-worker: {} — the gate's output could not be kept: {e}", job.branch)),
            }
        }
        if run.outcome == GateOutcome::NoVerdict {
            (self.log)(&format!("gate-worker: {} — not the branch's fault ({})", job.branch, run.reason_or("unspecified")));
        }
        self.queue.complete(self.slot, &Done { job: job.clone(), run, started_ms, finished_ms }).is_ok()
    }
}

#[cfg(test)]
mod tests;
