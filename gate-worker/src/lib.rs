//! gate-worker: the local gate, off the landing pass's clock. One worker drains the queue
//! (`landing_pass::gateq`) a branch at a time, so two branches for one gate tree are never
//! gated together, and files each verdict. The pass applies it: attribution of the four
//! outcomes stays in `landing_pass::pass::certify_judge`.

use landing_pass::gateq::{Done, GateQueue, Job};
use landing_pass::model::{GateOutcome, GateRun, GATE_NOVERDICT};

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
}

impl Worker<'_> {
    /// Gate every queued job, one at a time. Returns how many verdicts were filed.
    pub fn drain(&self) -> usize {
        self.queue.recover();
        let mut filed = 0;
        while let Some(job) = self.queue.claim() {
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
                self.queue.discard(job);
                return false;
            }
            Some(t) if t != job.tip => {
                (self.log)(&format!("gate-worker: {} moved from {} to {t} — dropping the stale job", job.branch, job.tip));
                self.queue.discard(job);
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
        if run.outcome == GateOutcome::NoVerdict {
            (self.log)(&format!("gate-worker: {} — not the branch's fault ({})", job.branch, run.reason_or("unspecified")));
        }
        self.queue.complete(&Done { job: job.clone(), run, started_ms, finished_ms }).is_ok()
    }
}

#[cfg(test)]
mod tests;
