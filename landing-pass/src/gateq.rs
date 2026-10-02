//! The gate queue under `$SPIRA_RUN/gate-worker/`: the seam between the pass, which
//! enqueues a branch needing a local verdict, and `gate-worker`, which gates one at a time
//! and files the result. Same idiom as the broker: inbox → claimed → done, refused for
//! what cannot be read.

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

    pub fn find(&self, repo: &str, branch: &str, tip: &str) -> Option<Where> {
        let f = Job::new(repo, branch, "", tip, false).file();
        [Where::Done, Where::Claimed, Where::Inbox].into_iter().find(|w| self.dir(*w).join(&f).exists())
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

    fn read_dir_jobs(&self, w: Where) -> Vec<(PathBuf, Job)> {
        let mut v = Vec::new();
        let Ok(rd) = fs::read_dir(self.dir(w)) else { return v };
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
        let mut v: Vec<Job> = self.read_dir_jobs(Where::Inbox).into_iter().map(|(_, j)| j).collect();
        v.sort_by_key(|j| (!j.fix, j.queued));
        v
    }

    /// Return jobs a dead worker left claimed to the inbox.
    pub fn recover(&self) {
        for (p, j) in self.read_dir_jobs(Where::Claimed) {
            let _ = fs::create_dir_all(self.dir(Where::Inbox));
            let _ = fs::rename(&p, self.dir(Where::Inbox).join(j.file()));
        }
    }

    /// Take the next job: base fixes first, then oldest.
    pub fn claim(&self) -> Option<Job> {
        let _ = fs::create_dir_all(self.dir(Where::Claimed));
        let mut v = self.read_dir_jobs(Where::Inbox);
        v.sort_by_key(|(_, j)| (!j.fix, j.queued));
        for (p, j) in v {
            if fs::rename(&p, self.dir(Where::Claimed).join(j.file())).is_ok() {
                return Some(j);
            }
        }
        None
    }

    /// Drop a claimed job that will not be gated.
    pub fn discard(&self, job: &Job) {
        let _ = fs::remove_file(self.dir(Where::Claimed).join(job.file()));
    }

    pub fn complete(&self, d: &Done) -> std::io::Result<()> {
        let body = serde_json::to_string(d).map_err(std::io::Error::other)?;
        atomic_write(&self.dir(Where::Done).join(d.job.file()), &body)?;
        let _ = fs::remove_file(self.dir(Where::Claimed).join(d.job.file()));
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
        let order: Vec<String> = std::iter::from_fn(|| q.claim()).map(|j| j.bead).collect();
        assert_eq!(order, ["sp-3", "sp-1", "sp-2"]);
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Claimed));
        q.complete(&done(&first)).unwrap();
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
        q.claim().unwrap();
        assert!(q.queued().is_empty());
        q.recover();
        assert_eq!(q.queued().len(), 1);
        fs::write(d.join("gate-worker/inbox/junk.json"), "{").unwrap();
        assert_eq!(q.queued().len(), 1);
        assert!(d.join("gate-worker/refused/junk.json").exists());
    }
}
