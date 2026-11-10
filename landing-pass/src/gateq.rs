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

use crate::model::{GateRun, DIED_TWICE};
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
    /// The bead's priority, lower first; a job queued without one sorts last.
    #[serde(default = "no_priority")]
    pub priority: i64,
    /// Gate runs of this tip that died at the cap, each as its `ran=` and `phases=` evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deaths: Vec<String>,
}

fn no_priority() -> i64 {
    9999
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Done {
    pub job: Job,
    pub run: GateRun,
    pub started_ms: u64,
    pub finished_ms: u64,
    /// The release whose gate filed this verdict; `complete` stamps it. A verdict filed by
    /// any other gate is not this gate's to replay.
    #[serde(default)]
    pub gate: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    Filed,
    Requeued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    Inbox,
    Claimed,
    Done,
}

const RETAINED_OUTPUTS: usize = 300;

pub struct GateQueue {
    root: PathBuf,
    gate: String,
}

/// The release this process runs, canonical; empty when none is named.
pub fn release_id() -> String {
    std::env::var_os(spira_config::RELEASE_ENV)
        .and_then(|r| fs::canonicalize(r).ok())
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn pick_key(j: &Job, running: &std::collections::HashMap<String, usize>) -> (bool, usize, i64, u128) {
    order_key(j.fix, running.get(&j.repo).copied().unwrap_or(0), j.priority, j.queued)
}

/// The order jobs are taken in, smallest first: base fixes, then the repository running the
/// fewest, then the lowest priority number, then the oldest.
pub fn order_key(fix: bool, running_in_repo: usize, priority: i64, queued: u128) -> (bool, usize, i64, u128) {
    (!fix, running_in_repo, priority, queued)
}

impl Job {
    fn prefix(&self) -> String {
        format!("{}.{}.", branch_key(&self.repo), branch_key(&self.branch))
    }
    fn file(&self) -> String {
        format!("{}{}.json", self.prefix(), branch_key(&self.tip))
    }
    pub fn new(repo: &str, branch: &str, bead: &str, tip: &str, fix: bool) -> Job {
        Job { repo: repo.into(), branch: branch.into(), bead: bead.into(), tip: tip.into(), fix, queued: nanos(), priority: no_priority(), deaths: Vec::new() }
    }
    pub fn with_priority(mut self, priority: i64) -> Job {
        self.priority = priority;
        self
    }
}

impl GateQueue {
    pub fn new(run: &Path) -> GateQueue {
        GateQueue { root: run.join("gate-worker"), gate: release_id() }
    }
    pub fn with_gate(mut self, gate: &str) -> GateQueue {
        self.gate = gate.to_string();
        self
    }
    fn current(&self, d: &Done) -> bool {
        self.gate.is_empty() || d.gate == self.gate
    }
    fn read_done(&self, repo: &str, branch: &str, tip: &str) -> Option<(PathBuf, Done)> {
        let p = self.dir(Where::Done).join(Job::new(repo, branch, "", tip, false).file());
        let d: Done = serde_json::from_str(&fs::read_to_string(&p).ok()?).ok()?;
        self.current(&d).then_some((p, d))
    }
    /// Delete the verdict filed for this branch at this tip, whichever gate filed it.
    pub fn forget_done(&self, repo: &str, branch: &str, tip: &str) -> bool {
        fs::remove_file(self.dir(Where::Done).join(Job::new(repo, branch, "", tip, false).file())).is_ok()
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
        if self.read_done(repo, branch, tip).is_some() {
            return Some(Where::Done);
        }
        // A slot's own subdirectory, or — the pre-N-worker layout, until slot 0's recover
        // sweeps it — a flat file straight under claimed/ (sp-kbjv6 "wave N concurrent
        // drains", the deploy migration).
        if self.dir(Where::Claimed).join(&f).exists() || self.claimed_dirs().iter().any(|d| d.join(&f).exists()) {
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

    /// The verdict for this branch at this tip, left in place for the pass that will judge it.
    pub fn peek_done(&self, repo: &str, branch: &str, tip: &str) -> Option<Done> {
        self.read_done(repo, branch, tip).map(|(_, d)| d)
    }

    /// The verdict for this branch at this tip, consumed.
    pub fn take_done(&self, repo: &str, branch: &str, tip: &str) -> Option<Done> {
        let (p, d) = self.read_done(repo, branch, tip)?;
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
        let running = self.running_by_repo();
        v.sort_by_key(|j| pick_key(j, &running));
        v
    }

    /// Jobs currently claimed by any slot, counted per repository. Read-only: a claimed
    /// file that cannot be parsed is not this call's to refuse.
    pub fn running_by_repo(&self) -> std::collections::HashMap<String, usize> {
        let mut m = std::collections::HashMap::new();
        for d in self.claimed_dirs() {
            let Ok(rd) = fs::read_dir(d) else { continue };
            for e in rd.flatten() {
                if let Some(j) = fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Job>(&t).ok()) {
                    *m.entry(j.repo).or_insert(0) += 1;
                }
            }
        }
        m
    }

    /// Return jobs left claimed in this slot to the inbox. Safe to call unconditionally at
    /// the start of a slot's drain: this slot's own flock (held before `recover` ever runs)
    /// guarantees mutual exclusion on `claimed/<slot>/`, so anything sitting there now was
    /// left by a previous holder of *this same slot* that died before finishing — never by
    /// another slot's worker still live and gating (that worker owns its own subdirectory).
    ///
    /// Slot 0 additionally recovers any flat `claimed/<job>.json` — the pre-N-worker
    /// layout's own claim directory, before `claimed/` held per-slot subdirectories. Slot
    /// 0's lock is the pre-existing `worker.lock` name, the same one the single-worker
    /// design used, so a flat claim is only ever live under that same flock: a deploy
    /// cutover (or any claim an old release left behind) is recovered exactly once, by the
    /// one slot mutually exclusive with whoever could have left it. Slots 1.. never touch
    /// a flat claim — only slot 0 may, so two slots can never race to recover the same one.
    pub fn recover(&self, slot: usize) {
        for (p, j) in self.read_dir_jobs(&self.claimed_slot(slot)) {
            let _ = fs::create_dir_all(self.dir(Where::Inbox));
            let _ = fs::rename(&p, self.dir(Where::Inbox).join(j.file()));
        }
        if slot == 0 {
            for (p, j) in self.read_dir_jobs(&self.dir(Where::Claimed)) {
                let _ = fs::create_dir_all(self.dir(Where::Inbox));
                let _ = fs::rename(&p, self.dir(Where::Inbox).join(j.file()));
            }
        }
    }

    /// Take the next job for this slot: base fixes first, then the repository holding the
    /// fewest running slots, then lowest priority number, then oldest. The move into
    /// `claimed/<slot>/` is a single `rename(2)` of the inbox file: whichever slot's rename
    /// lands first wins the job, and every other slot's rename of that same (now-gone)
    /// source path fails and falls through to the next candidate — no two slots can ever
    /// come away from `claim` with the same job.
    pub fn claim(&self, slot: usize) -> Option<Job> {
        let dest = self.claimed_slot(slot);
        let _ = fs::create_dir_all(&dest);
        let mut v = self.read_dir_jobs(&self.dir(Where::Inbox));
        let running = self.running_by_repo();
        v.sort_by_key(|(_, j)| pick_key(j, &running));
        for (p, j) in v {
            if fs::rename(&p, dest.join(j.file())).is_ok() {
                return Some(j);
            }
        }
        None
    }

    /// Claim one chosen job for this slot: the same single `rename(2)` as [`GateQueue::claim`],
    /// so a job already claimed (or gone) is refused. Returns the job as queued.
    pub fn claim_job(&self, slot: usize, repo: &str, branch: &str, tip: &str) -> Option<Job> {
        let file = Job::new(repo, branch, "", tip, false).file();
        let dest = self.claimed_slot(slot);
        let _ = fs::create_dir_all(&dest);
        fs::rename(self.dir(Where::Inbox).join(&file), dest.join(&file)).ok()?;
        serde_json::from_str(&fs::read_to_string(dest.join(&file)).ok()?).ok()
    }

    /// The job this slot holds for this branch at this tip, if it holds it.
    pub fn claimed_job(&self, slot: usize, repo: &str, branch: &str, tip: &str) -> Option<Job> {
        let p = self.claimed_slot(slot).join(Job::new(repo, branch, "", tip, false).file());
        serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
    }

    /// Every job this slot holds.
    pub fn claimed(&self, slot: usize) -> Vec<Job> {
        let mut v: Vec<Job> = self.read_dir_jobs(&self.claimed_slot(slot)).into_iter().map(|(_, j)| j).collect();
        v.sort_by_key(Job::file);
        v
    }

    /// The slots that hold a claimed directory, by number.
    pub fn claimed_slots(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.claimed_dirs().iter().filter_map(|d| d.file_name()?.to_str()?.parse().ok()).collect();
        v.sort();
        v
    }

    /// Drop a job this slot claimed that will not be gated.
    pub fn discard(&self, slot: usize, job: &Job) {
        let _ = fs::remove_file(self.claimed_slot(slot).join(job.file()));
    }

    /// Keeps the gate's whole output for a verdict that is not a pass, so a fault can be read
    /// after the verdict has been taken. Returns where it went.
    pub fn retain_output(&self, job: &Job, out: &str) -> std::io::Result<PathBuf> {
        let dir = self.root.join("output");
        fs::create_dir_all(&dir)?;
        let path = dir.join(job.file().replace(".json", ".out"));
        atomic_write(&path, out)?;
        let mut kept: Vec<_> = fs::read_dir(&dir)?
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        kept.sort();
        let excess = kept.len().saturating_sub(RETAINED_OUTPUTS);
        for (_, old) in kept.into_iter().take(excess) {
            let _ = fs::remove_file(old);
        }
        Ok(path)
    }

    /// File a finished run. A first death at the cap goes back to the inbox, behind what is
    /// queued now, instead of reaching the pass; a second is filed as `died-twice`, with both
    /// deaths' timings in its output, so the pass moves the bead out of SUBMITTED at once.
    pub fn settle(&self, slot: usize, d: &Done) -> std::io::Result<Settled> {
        if !d.run.cap_death() {
            self.complete(slot, d)?;
            return Ok(Settled::Filed);
        }
        let mut job = d.job.clone();
        job.deaths.push(self.death_evidence(&job, d));
        if job.deaths.len() < 2 {
            job.queued = nanos();
            let body = serde_json::to_string(&job).map_err(std::io::Error::other)?;
            atomic_write(&self.dir(Where::Inbox).join(job.file()), &body)?;
            let _ = fs::remove_file(self.claimed_slot(slot).join(job.file()));
            return Ok(Settled::Requeued);
        }
        let out = format!(
            "{}\ngate: VERDICT=NO_VERDICT reason={} branch={} repo={} suite=-\ngate-worker: the gate died at its cap on {} runs of {}: {}\n",
            d.run.out,
            DIED_TWICE,
            job.branch,
            job.repo,
            job.deaths.len(),
            job.tip,
            job.deaths.join("; ")
        );
        let run = GateRun::parse(crate::model::GATE_NOVERDICT, out);
        self.complete(slot, &Done { job, run, started_ms: d.started_ms, finished_ms: d.finished_ms })?;
        Ok(Settled::Filed)
    }

    fn death_evidence(&self, job: &Job, d: &Done) -> String {
        let log = self.root.parent().map(|r| r.join("gate.log")).and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
        let mark = format!(" {} ", job.branch);
        let line = log.lines().rev().find(|l| l.contains(&mark) && l.contains(" phases="));
        match line.and_then(|l| l.find(" ran=").map(|i| l[i..].trim().to_string())) {
            Some(t) => t,
            None => format!("ran={}ms (no phase timings in gate.log)", d.finished_ms.saturating_sub(d.started_ms)),
        }
    }

    pub fn complete(&self, slot: usize, d: &Done) -> std::io::Result<()> {
        let d = &Done { gate: self.gate.clone(), ..d.clone() };
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
        Done { job: job.clone(), run: GateRun::parse(0, "ok".into()), started_ms: 1, finished_ms: 2, gate: String::new() }
    }

    #[test]
    fn a_verdict_filed_by_another_gate_is_not_this_gates_to_replay_and_the_tip_is_queued_again() {
        let dir = tmpdir("gq-gate");
        let old = GateQueue::new(&dir).with_gate("release-a");
        let job = Job::new("r", "spira/sp-1", "sp-1", "t1", false);
        old.enqueue(&job).unwrap();
        let claimed = old.claim(0).unwrap();
        old.complete(0, &done(&claimed)).unwrap();
        assert!(old.peek_done("r", "spira/sp-1", "t1").is_some());
        let new = GateQueue::new(&dir).with_gate("release-b");
        assert!(new.peek_done("r", "spira/sp-1", "t1").is_none());
        assert!(new.take_done("r", "spira/sp-1", "t1").is_none());
        assert!(new.enqueue(&job).unwrap(), "judged afresh");
        assert!(new.peek_done("r", "spira/sp-1", "t1").is_none());
    }

    #[test]
    fn forgetting_a_done_verdict_lets_the_same_tip_be_queued() {
        let dir = tmpdir("gq-gate");
        let q = GateQueue::new(&dir).with_gate("g");
        let job = Job::new("r", "spira/sp-1", "sp-1", "t1", false);
        q.enqueue(&job).unwrap();
        let c = q.claim(0).unwrap();
        q.complete(0, &done(&c)).unwrap();
        assert!(!q.enqueue(&job).unwrap());
        assert!(q.forget_done("r", "spira/sp-1", "t1"));
        assert!(q.enqueue(&job).unwrap());
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
    fn a_p0_queued_after_a_p2_is_claimed_first() {
        let d = tmpdir("gq-pri");
        let q = GateQueue::new(&d);
        q.enqueue(&Job::new("r", "spira/sp-p2", "sp-p2", "t1", false).with_priority(2)).unwrap();
        q.enqueue(&Job::new("r", "spira/sp-p0", "sp-p0", "t2", false).with_priority(0)).unwrap();
        assert_eq!(q.claim(0).unwrap().bead, "sp-p0");
    }

    #[test]
    fn a_free_slot_goes_to_the_repo_holding_fewest_even_behind_older_jobs() {
        let d = tmpdir("gq-fair");
        let q = GateQueue::new(&d);
        for (i, b) in ["a1", "a2", "a3", "a4"].iter().enumerate() {
            q.enqueue(&Job::new("A", &format!("spira/{b}"), b, &format!("t{i}"), false).with_priority(0)).unwrap();
        }
        q.enqueue(&Job::new("B", "spira/b1", "b1", "tb", false).with_priority(4)).unwrap();
        assert_eq!(q.claim(0).unwrap().repo, "A");
        assert_eq!(q.claim(1).unwrap().repo, "B");
        assert_eq!(q.claim(2).unwrap().repo, "A");
    }

    #[test]
    fn all_slots_held_by_a_repo_still_yield_to_a_queued_other_repo() {
        let d = tmpdir("gq-fair2");
        let q = GateQueue::new(&d);
        for i in 0..3 {
            q.enqueue(&Job::new("A", &format!("spira/a{i}"), "a", &format!("t{i}"), false)).unwrap();
        }
        assert_eq!(q.claim(0).unwrap().repo, "A");
        assert_eq!(q.claim(1).unwrap().repo, "A");
        q.enqueue(&Job::new("B", "spira/b", "b", "tb", false).with_priority(4)).unwrap();
        assert_eq!(q.claim(2).unwrap().repo, "B");
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

    #[test]
    fn a_flat_legacy_claim_is_recovered_by_slot_0_and_not_by_slot_1() {
        let d = tmpdir("gq6");
        let q = GateQueue::new(&d);
        // The pre-N-worker layout: a job claimed straight under claimed/, no slot
        // subdirectory — left behind by the single-worker release, or still live in an
        // old-release worker caught mid-deploy.
        let job = Job::new("r", "spira/sp-1", "sp-1", "t1", false);
        fs::create_dir_all(d.join("gate-worker/claimed")).unwrap();
        fs::write(d.join("gate-worker/claimed").join(job.file()), serde_json::to_string(&job).unwrap()).unwrap();
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Claimed));

        // Slot 1 never touches a flat claim — only slot 0 (the pre-existing worker.lock
        // name, mutually exclusive with whoever could have left it) may recover one.
        q.recover(1);
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Claimed), "slot 1 must not recover a flat legacy claim");
        assert!(q.queued().is_empty());

        q.recover(0);
        assert_eq!(q.find("r", "spira/sp-1", "t1"), Some(Where::Inbox), "slot 0 returns the flat legacy claim to the inbox");
        assert_eq!(q.queued().len(), 1);
    }
}
