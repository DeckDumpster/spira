//! The pass's own files under $SPIRA_RUN (DESIGN.md §2.4).

use crate::model::{RunRecord, StatusFile};
use crate::util::atomic_write;
use spira_config::branch_key::branch_key;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredRecord {
    pub branch: String,
    pub repo: String,
    pub count: u32,
    pub since: u64,
    pub last: u64,
}

impl DeferredRecord {
    pub fn render(&self) -> String {
        format!(
            "branch={}\nrepo={}\ncount={}\nsince={}\nlast={}\n",
            self.branch, self.repo, self.count, self.since, self.last
        )
    }

    /// A bare-number file (the pre-record counter) reads as a count with no branch.
    pub fn parse(t: &str) -> Option<DeferredRecord> {
        let mut r = DeferredRecord { branch: String::new(), repo: String::new(), count: 0, since: 0, last: 0 };
        if let Ok(n) = t.trim().parse::<u32>() {
            r.count = n;
            return Some(r);
        }
        for l in t.lines() {
            let (k, v) = l.split_once('=')?;
            match k {
                "branch" => r.branch = v.to_string(),
                "repo" => r.repo = v.to_string(),
                "count" => r.count = v.parse().ok()?,
                "since" => r.since = v.parse().ok()?,
                "last" => r.last = v.parse().ok()?,
                _ => {}
            }
        }
        (r.count > 0).then_some(r)
    }
}

pub struct Files {
    pub run: PathBuf,
}

impl Files {
    pub fn new(run: &Path) -> Files {
        Files { run: run.to_path_buf() }
    }
    pub fn status(&self) -> PathBuf {
        self.run.join("landing.status")
    }
    pub fn mailbox(&self) -> PathBuf {
        self.run.join("landing.progress")
    }
    pub fn run_record(&self) -> PathBuf {
        self.run.join("landing.run")
    }
    pub fn containers(&self) -> PathBuf {
        self.run.join("landing.containers")
    }
    pub fn cursor(&self) -> PathBuf {
        self.run.join("landing.cursor")
    }
    pub fn deferred_dir(&self) -> PathBuf {
        self.run.join("landing.deferred")
    }
    pub fn interrupted(&self) -> PathBuf {
        self.run.join("landing.interrupted")
    }
    pub fn lock(&self) -> PathBuf {
        self.run.join("landing.lock")
    }

    /// A PASS clears the machinery-fault counters for this branch.
    pub fn clear_noverdict(&self, branch: &str) {
        let _ = fs::remove_file(self.backoff_file(branch));
        let _ = fs::remove_file(self.dead_file(branch));
        let _ = fs::remove_file(self.exhaust_file(branch));
        spira_config::branch_key::clear_noverdict(&self.run, branch);
    }

    fn exhaust_file(&self, branch: &str) -> PathBuf {
        self.run.join("gate-exhaust").join(branch_key(branch))
    }

    fn backoff_file(&self, branch: &str) -> PathBuf {
        self.run.join("gate-backoff").join(branch_key(branch))
    }

    fn unrounded_file(&self, branch: &str) -> PathBuf {
        self.run.join("unrounded-since").join(branch_key(branch))
    }

    /// Seconds `branch` has been seen in no round: the first sighting is stamped, later ones age from it.
    pub fn unrounded_age(&self, branch: &str, now: u64) -> u64 {
        let file = self.unrounded_file(branch);
        if let Some(at) = fs::read_to_string(&file).ok().and_then(|t| t.trim().parse::<u64>().ok()) {
            return now.saturating_sub(at);
        }
        let _ = fs::create_dir_all(self.run.join("unrounded-since"));
        let _ = atomic_write(&file, &format!("{now}\n"));
        0
    }

    /// `branch` is in a round: the next time it is in none, its wait starts again.
    pub fn clear_unrounded(&self, branch: &str) {
        let _ = fs::remove_file(self.unrounded_file(branch));
    }

    /// Note that the gate could not judge `branch` at `tip` for want of the host's room.
    pub fn back_off(&self, branch: &str, tip: &str, until: u64) {
        let _ = fs::create_dir_all(self.run.join("gate-backoff"));
        let _ = atomic_write(&self.backoff_file(branch), &format!("{tip}\n{until}\n"));
    }

    fn marked_file(&self, branch: &str) -> PathBuf {
        self.run.join("certifies-nothing").join(branch_key(branch))
    }

    /// The back-off on `branch` at `tip` rests on the row carrying the certifies-nothing mark.
    pub fn note_marked(&self, branch: &str, tip: &str) {
        let _ = fs::create_dir_all(self.run.join("certifies-nothing"));
        let _ = atomic_write(&self.marked_file(branch), &format!("{tip}\n"));
    }

    pub fn marked(&self, branch: &str, tip: &str) -> bool {
        fs::read_to_string(self.marked_file(branch)).map(|t| t.trim() == tip).unwrap_or(false)
    }

    pub fn forget_marked(&self, branch: &str) {
        let _ = fs::remove_file(self.marked_file(branch));
    }

    fn pass_refused_file(&self, branch: &str) -> PathBuf {
        self.run.join("pass-refused").join(branch_key(branch))
    }

    /// Record that the lifecycle refused a GatePass for `branch` at `tip`: the same tip is
    /// not offered again.
    pub fn refuse_pass(&self, branch: &str, tip: &str) {
        let _ = fs::create_dir_all(self.run.join("pass-refused"));
        let _ = atomic_write(&self.pass_refused_file(branch), &format!("{tip}\n"));
    }

    pub fn pass_refused(&self, branch: &str, tip: &str) -> bool {
        fs::read_to_string(self.pass_refused_file(branch)).map(|t| t.trim() == tip).unwrap_or(false)
    }

    fn content_settled_file(&self, branch: &str) -> PathBuf {
        self.run.join("content-settled").join(branch_key(branch))
    }

    /// Record that `branch` at `tip` was found to add nothing to the base and was answered
    /// for: the same tip is not announced or closed again.
    pub fn settle_content(&self, branch: &str, tip: &str) {
        let _ = fs::create_dir_all(self.run.join("content-settled"));
        let _ = atomic_write(&self.content_settled_file(branch), &format!("{tip}\n"));
    }

    /// The "nothing to land" line was written for `branch` at `tip`; a retried close stays quiet.
    pub fn announce_content(&self, branch: &str, tip: &str) -> bool {
        let f = self.run.join("content-announced").join(branch_key(branch));
        if fs::read_to_string(&f).is_ok_and(|t| t.trim() == tip) {
            return false;
        }
        let _ = fs::create_dir_all(self.run.join("content-announced"));
        let _ = atomic_write(&f, &format!("{tip}\n"));
        true
    }

    pub fn content_settled(&self, branch: &str, tip: &str) -> bool {
        fs::read_to_string(self.content_settled_file(branch)).map(|t| t.trim() == tip).unwrap_or(false)
    }

    fn gated_file(&self, branch: &str) -> PathBuf {
        self.run.join("gate-judged").join(branch_key(branch))
    }

    /// Record that a gate verdict on `branch` at `tip` was applied: a round cut later needs no second gate.
    pub fn note_gated(&self, branch: &str, tip: &str) {
        let _ = fs::create_dir_all(self.run.join("gate-judged"));
        let _ = atomic_write(&self.gated_file(branch), &format!("{tip}\n"));
    }

    pub fn gated(&self, branch: &str, tip: &str) -> bool {
        fs::read_to_string(self.gated_file(branch)).map(|t| t.trim() == tip).unwrap_or(false)
    }

    /// Forget the unjudged-run record: the tip gets a fresh count.
    pub fn clear_exhaust(&self, branch: &str) {
        let _ = fs::remove_file(self.exhaust_file(branch));
    }

    /// Record an unjudged verdict on this exact tip; the reasons so far, a different tip starting again.
    pub fn note_exhaust(&self, branch: &str, tip: &str, reason: &str) -> Vec<String> {
        let file = self.exhaust_file(branch);
        let text = fs::read_to_string(&file).unwrap_or_default();
        let mut lines = text.lines();
        let mut seen: Vec<String> = match lines.next() {
            Some(was) if was == tip => lines.map(str::to_string).collect(),
            _ => Vec::new(),
        };
        seen.push(reason.to_string());
        let _ = fs::create_dir_all(self.run.join("gate-exhaust"));
        let _ = atomic_write(&file, &format!("{tip}\n{}\n", seen.join("\n")));
        seen
    }

    fn dead_file(&self, branch: &str) -> PathBuf {
        self.run.join("gate-dead").join(branch_key(branch))
    }

    fn dead_seq_file(&self) -> PathBuf {
        self.run.join("gate-dead-seq")
    }

    /// Count a gate that died on `branch`, whatever its tip. The count holds only while no other
    /// branch's gate has died since: a death in between makes the fault the host's, so it restarts at 1.
    pub fn note_dead(&self, branch: &str) -> u32 {
        let read = |p: PathBuf| fs::read_to_string(p).unwrap_or_default();
        let seq = read(self.dead_seq_file()).trim().parse::<u64>().unwrap_or(0);
        let prior = read(self.dead_file(branch));
        let mut l = prior.lines();
        let held = match (l.next().and_then(|n| n.parse::<u32>().ok()), l.next().and_then(|n| n.parse::<u64>().ok())) {
            (Some(n), Some(at)) if at == seq => n,
            _ => 0,
        };
        let n = held + 1;
        let _ = fs::create_dir_all(self.run.join("gate-dead"));
        let _ = atomic_write(&self.dead_seq_file(), &format!("{}\n", seq + 1));
        let _ = atomic_write(&self.dead_file(branch), &format!("{n}\n{}\n", seq + 1));
        n
    }

    fn unjudged_incident_file(&self, repo: &str) -> PathBuf {
        self.run.join("gate-unjudged-incident").join(repo)
    }

    pub fn unjudged_incidents(&self, repo: &str) -> Vec<String> {
        let t = fs::read_to_string(self.unjudged_incident_file(repo)).unwrap_or_default();
        t.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
    }

    pub fn note_unjudged_incident(&self, repo: &str, id: &str) {
        let mut ids = self.unjudged_incidents(repo);
        if ids.iter().any(|i| i == id) {
            return;
        }
        ids.push(id.to_string());
        self.write_unjudged_incidents(repo, &ids);
    }

    pub fn forget_unjudged_incident(&self, repo: &str, id: &str) {
        let ids: Vec<String> = self.unjudged_incidents(repo).into_iter().filter(|i| i != id).collect();
        if ids.is_empty() {
            let _ = fs::remove_file(self.unjudged_incident_file(repo));
        } else {
            self.write_unjudged_incidents(repo, &ids);
        }
    }

    fn write_unjudged_incidents(&self, repo: &str, ids: &[String]) {
        let _ = fs::create_dir_all(self.run.join("gate-unjudged-incident"));
        let _ = atomic_write(&self.unjudged_incident_file(repo), &format!("{}\n", ids.join("\n")));
    }

    fn unjudged_branch_file(&self, repo: &str) -> PathBuf {
        self.run.join("gate-unjudged-branch").join(repo)
    }

    pub fn unjudged_branches(&self, repo: &str) -> Vec<String> {
        let t = fs::read_to_string(self.unjudged_branch_file(repo)).unwrap_or_default();
        t.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
    }

    pub fn note_unjudged_branch(&self, repo: &str, branch: &str) {
        let mut brs = self.unjudged_branches(repo);
        if brs.iter().any(|b| b == branch) {
            return;
        }
        brs.push(branch.to_string());
        let _ = fs::create_dir_all(self.run.join("gate-unjudged-branch"));
        let _ = atomic_write(&self.unjudged_branch_file(repo), &format!("{}\n", brs.join("\n")));
    }

    pub fn forget_unjudged_branches(&self, repo: &str) {
        let _ = fs::remove_file(self.unjudged_branch_file(repo));
    }

    /// The gate may judge `branch` again before its backoff ends.
    pub fn clear_backoff(&self, branch: &str) {
        let _ = fs::remove_file(self.backoff_file(branch));
    }

    /// A gate reached a verdict for `branch`: its run of dead gates is over.
    pub fn note_gate_judged(&self, branch: &str) {
        let _ = fs::remove_file(self.dead_file(branch));
    }

    /// True while `now` is before the `until` that [`Files::back_off`] recorded for this very tip.
    pub fn backing_off(&self, branch: &str, tip: &str, now: u64) -> bool {
        let Ok(t) = fs::read_to_string(self.backoff_file(branch)) else { return false };
        parse_backoff(&t).is_some_and(|(was, until)| was == tip && now < until)
    }

    pub fn write_status(&self, st: &StatusFile) {
        let _ = atomic_write(&self.status(), &st.render());
    }

    pub fn write_run(&self, r: &RunRecord) {
        let _ = atomic_write(&self.run_record(), &r.render());
    }

    pub fn read_run(&self) -> Option<RunRecord> {
        fs::read_to_string(self.run_record()).ok().map(|t| RunRecord::parse(&t))
    }

    pub fn clear_run(&self) {
        let _ = fs::remove_file(self.run_record());
        let _ = fs::remove_file(self.containers());
    }

    pub fn cursor_repo(&self) -> Option<String> {
        fs::read_to_string(self.cursor()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    pub fn set_cursor(&self, repo: &str) {
        let _ = atomic_write(&self.cursor(), &format!("{repo}\n"));
    }

    fn deferred_file(&self, branch: &str) -> PathBuf {
        self.deferred_dir().join(branch.replace('/', "_"))
    }

    /// Bump a branch's deferral record — branch, repo, consecutive count, first and last
    /// epoch, `key=value` per line so a watcher reads it without this crate. Returns the count.
    pub fn bump_deferred(&self, branch: &str, repo: &str, now: u64) -> u32 {
        let f = self.deferred_file(branch);
        let prev = fs::read_to_string(&f).ok().and_then(|t| DeferredRecord::parse(&t));
        let (n, since) = prev.map(|r| (r.count + 1, r.since)).unwrap_or((1, now));
        let rec = DeferredRecord { branch: branch.to_string(), repo: repo.to_string(), count: n, since, last: now };
        let _ = atomic_write(&f, &rec.render());
        n
    }

    /// Every branch currently carrying a deferral record, oldest first.
    pub fn deferred_records(&self) -> Vec<DeferredRecord> {
        let Ok(rd) = fs::read_dir(self.deferred_dir()) else { return Vec::new() };
        let mut v: Vec<DeferredRecord> =
            rd.flatten().filter_map(|e| DeferredRecord::parse(&fs::read_to_string(e.path()).ok()?)).collect();
        v.sort_by_key(|r| (r.since, r.branch.clone()));
        v
    }

    pub fn clear_deferred(&self, branch: &str) {
        let _ = fs::remove_file(self.deferred_file(branch));
    }

    /// Delete verdict-cache entries older than the gate's TTL, one file at a time (a gate may
    /// be writing the directory). Age is the gate's own `at=` stamp, else the mtime.
    pub fn prune_verdicts(dir: &Path, ttl_secs: u64, now: u64) {
        // Strictly older than N whole minutes.
        let mins = ttl_secs / 60;
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let stamped = fs::read_to_string(e.path()).ok().and_then(|t| {
                let at = t.lines().filter_map(|l| l.strip_prefix("at=")).next_back()?;
                at.bytes().all(|b| b.is_ascii_digit()).then(|| at.parse::<u64>().ok()).flatten()
            });
            let mtime = || md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs());
            let Some(at) = stamped.or_else(mtime) else { continue };
            let age_min_ceil = now.saturating_sub(at).div_ceil(60);
            if age_min_ceil > mins {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(test)]
mod deferred_tests {
    use super::*;

    #[test_id(t1p0jsb)]
    #[test]
    fn a_deferral_is_a_record_a_watcher_can_read() {
        let d = testkit::TempDir::new("lp-deferred-rec");
        let f = Files::new(d.path());
        assert!(f.deferred_records().is_empty());
        assert_eq!(f.bump_deferred("spira/sp-a", "spira", 100), 1);
        assert_eq!(f.bump_deferred("spira/sp-a", "spira", 160), 2);
        let recs = f.deferred_records();
        assert_eq!(
            recs,
            vec![DeferredRecord { branch: "spira/sp-a".into(), repo: "spira".into(), count: 2, since: 100, last: 160 }]
        );
        f.clear_deferred("spira/sp-a");
        assert!(f.deferred_records().is_empty());
    }

    #[test_id(tsn43gv)]
    #[test]
    fn a_legacy_bare_counter_still_counts() {
        let d = testkit::TempDir::new("lp-deferred-legacy");
        let f = Files::new(d.path());
        fs::create_dir_all(f.deferred_dir()).unwrap();
        fs::write(f.deferred_dir().join("spira_sp-a"), "4\n").unwrap();
        assert_eq!(f.bump_deferred("spira/sp-a", "spira", 9), 5);
    }
}

fn parse_backoff(text: &str) -> Option<(String, u64)> {
    let mut l = text.lines();
    Some((l.next()?.to_string(), l.next()?.parse().ok()?))
}

/// Every (tip, until) a [`Files::back_off`] left under `run`; an unparseable file is skipped.
pub fn gate_backoffs(run: &Path) -> Vec<(String, u64)> {
    let Ok(dir) = fs::read_dir(run.join("gate-backoff")) else { return vec![] };
    dir.flatten().filter_map(|e| parse_backoff(&fs::read_to_string(e.path()).ok()?)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(ttw9458)]
    #[test]
    fn a_backed_off_tip_waits_out_its_window_and_a_moved_tip_does_not() {
        let d = crate::testutil::tmpdir("backoff");
        let f = Files::new(&d);
        assert!(!f.backing_off("spira/sp-a", "t1", 100));
        f.back_off("spira/sp-a", "t1", 400);
        assert!(f.backing_off("spira/sp-a", "t1", 399));
        assert!(!f.backing_off("spira/sp-a", "t1", 400));
        assert!(!f.backing_off("spira/sp-a", "t2", 150));
        f.clear_noverdict("spira/sp-a");
        assert!(!f.backing_off("spira/sp-a", "t1", 150));
    }

    #[test_id(tajlzqy)]
    #[test]
    fn gate_backoffs_lists_every_recorded_tip_and_skips_a_garbled_file() {
        let d = crate::testutil::tmpdir("backoffs");
        let f = Files::new(&d);
        assert_eq!(gate_backoffs(&d), vec![]);
        f.back_off("spira/sp-a", "t1", 400);
        f.back_off("spira/sp-b", "t2", 500);
        fs::write(d.join("gate-backoff").join("junk"), "no-until\n").unwrap();
        let mut got = gate_backoffs(&d);
        got.sort();
        assert_eq!(got, vec![("t1".to_string(), 400), ("t2".to_string(), 500)]);
    }

    #[test_id(t7v3pmh)]
    #[test]
    fn clearing_a_branch_keeps_the_counters_of_a_branch_that_extends_its_name() {
        let d = crate::testutil::tmpdir("clear-prefix");
        let f = Files::new(&d);
        fs::create_dir_all(d.join("noverdict")).unwrap();
        for n in ["spira-sp-a-conflict", "spira-sp-ab-conflict"] {
            fs::write(d.join("noverdict").join(n), "2").unwrap();
        }
        f.clear_noverdict("spira/sp-a");
        assert!(!d.join("noverdict/spira-sp-a-conflict").exists());
        assert!(d.join("noverdict/spira-sp-ab-conflict").exists());
    }

    #[test_id(tad0kyp)]
    #[test]
    fn a_verdict_ages_by_the_stamp_the_gate_wrote_not_by_its_mtime() {
        let d = crate::testutil::tmpdir("prune-stamp");
        let now = 1_800_000_000;
        let write = |name: &str, at: u64, mtime: std::time::SystemTime| {
            let p = d.join(name);
            fs::write(&p, format!("when=w\nat={at}\nby=b\nrepo=r\nbranch=spira/sp-a\nsuites=-\n")).unwrap();
            fs::File::options().write(true).open(&p).unwrap().set_modified(mtime).unwrap();
        };
        let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        write("young", now - 60, long_ago);
        write("old", now - 7_200, std::time::SystemTime::now());
        let unstamped = |name: &str, mtime: u64| {
            let p = d.join(name);
            fs::write(&p, "when=w\nat=soon\n").unwrap();
            fs::File::options().write(true).open(&p).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime)).unwrap();
        };
        unstamped("unstamped-young", now - 60);
        unstamped("unstamped-old", now - 7_200);
        Files::prune_verdicts(&d, 3_600, now);
        assert!(d.join("young").is_file(), "a verdict stamped a minute ago was pruned by its mtime");
        assert!(!d.join("old").exists(), "a verdict stamped two hours ago outlived a one-hour TTL");
        assert!(d.join("unstamped-young").is_file(), "an entry with no numeric at= is aged by its mtime");
        assert!(!d.join("unstamped-old").exists(), "an entry with no numeric at= is aged by its mtime");
    }
}
