//! The gate queue under `$SPIRA_RUN/gate-worker/`: the seam between the pass, which
//! enqueues a branch needing a local verdict, and `gate-worker`, which gates them (one at a
//! time per worker, up to N workers at once) and files each result. Same idiom as the
//! broker: inbox → claimed → done, refused for what cannot be read.
//!
//! `claimed/` is partitioned by worker slot (`claimed/<slot>/`): each slot's own flock
//! (`gate-worker`'s `worker.lock[.N]`) is the only thing that may hold a job there, so
//! `recover` — "a dead worker's claim goes back to the inbox" — can safely scope itself to
//! one slot's subdirectory without disturbing another slot's live, in-flight claim. `claim`
//! itself races safely with any number of slots regardless of partitioning: it is a single
//! `rename(2)` of the inbox file into the destination, and a rename only ever succeeds once
//! for a given source path — a second worker's rename of the same already-moved path fails
//! and that worker moves on to the next job.

use crate::model::GateRun;
use crate::util::{atomic_write, branch_key};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub repo: String,
    pub branch: String,
    pub bead: String,
    pub tip: String,
    /// A base-fix branch is gated before everything else queued.
    pub fix: bool,
    /// Nanoseconds since the epoch at enqueue: the order jobs are taken in.
    pub queued: u128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub job: Job,
    pub run: GateRun,
    pub started_ms: u64,
    pub finished_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Inbox,
    Claimed,
    Done,
}

pub struct GateQueue {
    root: PathBuf,
}

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

impl Job {
    fn prefix(&self) -> String {
        format!("{}.{}.", branch_key(&self.repo), branch_key(&self.branch))
    }
    fn file(&self) -> String {
        format!("{}{}.json", self.prefix(), branch_key(&self.tip))
    }
    pub fn new(repo: &str, branch: &str, bead: &str, tip: &str, fix: bool) -> Job {
        Job { repo: repo.into(), branch: branch.into(), bead: bead.into(), tip: tip.into(), fix, queued: nanos() }
    }
}

impl GateQueue {
    pub fn new(run: &Path) -> GateQueue {
        GateQueue { root: run.join("gate-worker") }
    }
    fn dir(&self, w: Where) -> PathBuf {
        self.root.join(match w {
            Where::Inbox => "inbox",
            Where::Claimed => "claimed",
            Where::Done => "done",
        })
    }
    fn refused(&self) -> PathBuf {
        self.root.join("refused")
    }

    /// One worker slot's own claimed jobs. Only the flock that slot's `gate-worker` holds
    /// may ever rename a file into or out of this directory (sp-kbjv6 "wave N concurrent
    /// drains"): that is what makes `recover(slot)` safe to call without coordinating with
    /// any other live slot.
    fn claimed_slot(&self, slot: usize) -> PathBuf {
        self.dir(Where::Claimed).join(slot.to_string())
    }

    /// Any slot's claimed directory that currently exists on disk, for `find` — which
    /// answers "is this job claimed by anyone" without knowing how many slots there are.
    fn claimed_dirs(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(self.dir(Where::Claimed)) else { return Vec::new() };
        rd.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).map(|e| e.path()).collect()
    }

    pub fn find(&self, repo: &str, branch: &str, tip: &str) -> Option<Where> {
        let f = Job::new(repo, branch, "", tip, false).file();
        if self.dir(Where::Done).join(&f).exists() {
            return Some(Where::Done);
        }
        if self.claimed_dirs().iter().any(|d| d.join(&f).exists()) {
            return Some(Where::Claimed);
        }
        if self.dir(Where::Inbox).join(&f).exists() {
            return Some(Where::Inbox);
        }
        None
    }

    /// Queue a job. Ok(false): this branch at this tip is already queued, claimed or done.
    /// A job or verdict for the same branch at another tip is dropped: the tree it judged
    /// no longer exists.
    pub fn enqueue(&self, job: &Job) -> std::io::Result<bool> {
        if self.find(&job.repo, &job.branch, &job.tip).is_some() {
            return Ok(false);
        }
        for w in [Where::Inbox, Where::Done] {
            if let Ok(rd) = fs::read_dir(self.dir(w)) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.starts_with(&job.prefix()) {
                        let _ = fs::remove_file(e.path());
                    }
                }
            }
        }
        let body = serde_json::to_string(job).map_err(std::io::Error::other)?;
        atomic_write(&self.dir(Where::Inbox).join(job.file()), &body)?;
        Ok(true)
    }

    /// The verdict for this branch at this tip, consumed.
    pub fn take_done(&self, repo: &str, branch: &str, tip: &str) -> Option<Done> {
        let p = self.dir(Where::Done).join(Job::new(repo, branch, "", tip, false).file());
        let d: Done = serde_json::from_str(&fs::read_to_string(&p).ok()?).ok()?;
        let _ = fs::remove_file(&p);
        Some(d)
    }

    fn read_dir_jobs(&self, dir: &Path) -> Vec<(PathBuf, Job)> {
        let mut v = Vec::new();
        let Ok(rd) = fs::read_dir(dir) else { return v };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Job>(&t).ok()) {
                Some(j) => v.push((p, j)),
                None => {
                    let _ = fs::create_dir_all(self.refused());
                    let _ = fs::rename(&p, self.refused().join(e.file_name()));
                }
            }
        }
        v
    }

    pub fn queued(&self) -> Vec<Job> {
        let mut v: Vec<Job> = self.read_dir_jobs(&self.dir(Where::Inbox)).into_iter().map(|(_, j)| j).collect();
        v.sort_by_key(|j| (!j.fix, j.queued));
        v
    }

    /// Return jobs left claimed in this slot to the inbox. Safe to call unconditionally at
    /// the start of a slot's drain: this slot's own flock (held before `recover` ever runs)
    /// guarantees mutual exclusion on `claimed/<slot>/`, so anything sitting there now was
    /// left by a previous holder of *this same slot* that died before finishing — never by
    /// another slot's worker still live and gating (that worker owns its own subdirectory).
    pub fn recover(&self, slot: usize) {
        for (p, j) in self.read_dir_jobs(&self.claimed_slot(slot)) {
            let _ = fs::create_dir_all(self.dir(Where::Inbox));
            let _ = fs::rename(&p, self.dir(Where::Inbox).join(j.file()));
        }
    }

    /// Take the next job for this slot: base fixes first, then oldest. The move into
    /// `claimed/<slot>/` is a single `rename(2)` of the inbox file: whichever slot's rename
    /// lands first wins the job, and every other slot's rename of that same (now-gone)
    /// source path fails and falls through to the next candidate — no two slots can ever
    /// come away from `claim` with the same job.
    pub fn claim(&self, slot: usize) -> Option<Job> {
        let dest = self.claimed_slot(slot);
        let _ = fs::create_dir_all(&dest);
        let mut v = self.read_dir_jobs(&self.dir(Where::Inbox));
        v.sort_by_key(|(_, j)| (!j.fix, j.queued));
        for (p, j) in v {
            if fs::rename(&p, dest.join(j.file())).is_ok() {
                return Some(j);
            }
        }
        None
    }

    /// Drop a job this slot claimed that will not be gated.
    pub fn discard(&self, slot: usize, job: &Job) {
        let _ = fs::remove_file(self.claimed_slot(slot).join(job.file()));
    }

    pub fn complete(&self, slot: usize, d: &Done) -> std::io::Result<()> {
        let body = serde_json::to_string(d).map_err(std::io::Error::other)?;
        atomic_write(&self.dir(Where::Done).join(d.job.file()), &body)?;
        let _ = fs::remove_file(self.claimed_slot(slot).join(d.job.file()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::tmpdir;

    fn done(job: &Job) -> Done {
        Done { job: job.clone(), run: GateRun::parse(0, "ok".into()), started_ms: 1, finished_ms: 2 }
    }

    #[test]
    fn enqueue_is_idempotent_per_tip_and_replaces_a_moved_tip() {
        let d = tmpdir("gq1");
        let q = GateQueue::new(&d);
        let a = Job::new("r", "spira/sp-a", "sp-a", "aaa", false);
        assert!(q.enqueue(&a).unwrap());
        assert!(!q.enqueue(&a).unwrap());
        let moved = Job::new("r", "spira/sp-a", "sp-a", "bbb", false);
        assert!(q.enqueue(&moved).unwrap());
        assert_eq!(q.queued().iter().map(|j| j.tip.as_str()).collect::<Vec<_>>(), vec!["bbb"]);
    }

    #[test]
    fn claim_takes_base_fixes_first_then_oldest_and_a_verdict_is_taken_once() {
        let d = tmpdir("gq2");
        let q = GateQueue::new(&d);
        let first = Job::new("r", "spira/sp-1", "sp-1", "t1", false);
        let second = Job::new("r", "spira/sp-2", "sp-2", "t2", false);
        let fix = Job::new("r", "spira/sp-3", "sp-3", "t3", true);
        for j in [&first, &second, &fix] {
            q.enqueue(j).unwrap();
        }
        let order: Vec<String> = std::iter::from_fn(|| q.claim(0)).map(|j| j.bead).collect();
        assert_eq!(order, ["sp-3", "sp-1", "sp-2"]);
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Claimed));
        q.complete(0, &done(&first)).unwrap();
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Done));
        assert!(q.take_done("r", "spira/sp-1", "other").is_none());
        assert!(q.take_done("r", "spira/sp-1", "t1").is_some());
        assert!(q.take_done("r", "spira/sp-1", "t1").is_none());
    }

    #[test]
    fn recover_returns_a_dead_workers_claim_and_garbage_is_refused() {
        let d = tmpdir("gq3");
        let q = GateQueue::new(&d);
        q.enqueue(&Job::new("r", "spira/sp-1", "sp-1", "t1", false)).unwrap();
        q.claim(0).unwrap();
        assert!(q.queued().is_empty());
        q.recover(0);
        assert_eq!(q.queued().len(), 1);
        fs::write(d.join("gate-worker/inbox/junk.json"), "{").unwrap();
        assert_eq!(q.queued().len(), 1);
        assert!(d.join("gate-worker/refused/junk.json").exists());
    }

    #[test]
    fn two_slots_claim_the_same_job_only_once() {
        let d = tmpdir("gq4");
        let q = GateQueue::new(&d);
        q.enqueue(&Job::new("r", "spira/sp-1", "sp-1", "t1", false)).unwrap();
        // Both slots see the same inbox listing before either renames; only one rename of
        // the shared source path can succeed (sp-kbjv6 "wave N concurrent drains").
        assert!(q.claim(0).is_some());
        assert!(q.claim(1).is_none());
    }

    #[test]
    fn a_live_slots_claim_survives_another_slots_recover() {
        let d = tmpdir("gq5");
        let q = GateQueue::new(&d);
        q.enqueue(&Job::new("r", "spira/sp-1", "sp-1", "t1", false)).unwrap();
        let job = q.claim(1).unwrap();
        // A fresh worker starting up on slot 0 sweeps its own (empty) claimed/0, never
        // slot 1's still-live claim — the bug this partitioning fixes.
        q.recover(0);
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Claimed), "slot 1's live claim must not be swept back to inbox by slot 0's recover");
        assert!(q.queued().is_empty());
        q.complete(1, &done(&job)).unwrap();
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Done));
    }
}
