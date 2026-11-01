//! The pass's own files under $SPIRA_RUN (DESIGN.md §2.4).

use crate::model::{RunRecord, StatusFile};
use crate::util::{atomic_write, branch_key};
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
        let key = branch_key(branch);
        if let Ok(rd) = fs::read_dir(self.run.join("noverdict")) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&key) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    fn exhaust_file(&self, branch: &str) -> PathBuf {
        self.run.join("gate-exhaust").join(branch_key(branch))
    }

    /// Forget a deferral: the branch is being gated again, so no marker may keep it from strand.
    pub fn clear_round_deferral(&self, branch: &str) {
        let _ = fs::remove_file(self.round_deferred_file(branch));
    }

    fn backoff_file(&self, branch: &str) -> PathBuf {
        self.run.join("gate-backoff").join(branch_key(branch))
    }

    /// Note that the gate could not judge `branch` at `tip` for want of the host's room.
    pub fn back_off(&self, branch: &str, tip: &str, now: u64) {
        let _ = fs::create_dir_all(self.run.join("gate-backoff"));
        let _ = atomic_write(&self.backoff_file(branch), &format!("{tip}\n{now}\n"));
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

    fn round_deferred_file(&self, branch: &str) -> PathBuf {
        self.run.join("gate-deferred").join(branch_key(branch))
    }

    /// Record that the host gate could not judge `branch` at `tip` within its cap: the round
    /// is its trial, and the same tip is not gated again.
    pub fn defer_to_round(&self, branch: &str, tip: &str) {
        let _ = fs::create_dir_all(self.run.join("gate-deferred"));
        let _ = atomic_write(&self.round_deferred_file(branch), &format!("{tip}\n"));
    }

    /// Record a host-cause verdict on this exact tip; the reasons so far, a different tip starting again.
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

    /// Count a NO_VERDICT on this exact tip; a different tip starts again at 1.
    pub fn bump_infra(&self, branch: &str, tip: &str) -> u32 {
        let file = self.run.join("gate-infra").join(branch_key(branch));
        let prior = fs::read_to_string(&file).ok().and_then(|t| {
            let mut l = t.lines();
            let was = l.next()?.to_string();
            Some((was, l.next()?.parse::<u32>().ok()?))
        });
        let n = match prior {
            Some((was, n)) if was == tip => n + 1,
            _ => 1,
        };
        let _ = fs::create_dir_all(self.run.join("gate-infra"));
        let _ = atomic_write(&file, &format!("{tip}\n{n}\n"));
        n
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

    /// A gate reached a verdict for `branch`: its run of dead gates is over.
    pub fn note_gate_judged(&self, branch: &str) {
        let _ = fs::remove_file(self.dead_file(branch));
    }

    pub fn deferred_to_round(&self, branch: &str, tip: &str) -> bool {
        fs::read_to_string(self.round_deferred_file(branch)).map(|t| t.trim() == tip).unwrap_or(false)
    }

    /// True while `window` seconds have not passed since [`Files::back_off`] for this very tip.
    pub fn backing_off(&self, branch: &str, tip: &str, now: u64, window: i64) -> bool {
        let Ok(t) = fs::read_to_string(self.backoff_file(branch)) else { return false };
        let mut l = t.lines();
        let (Some(was), Some(at)) = (l.next(), l.next().and_then(|n| n.parse::<u64>().ok())) else { return false };
        was == tip && (now.saturating_sub(at) as i64) < window
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

    #[test]
    fn a_legacy_bare_counter_still_counts() {
        let d = testkit::TempDir::new("lp-deferred-legacy");
        let f = Files::new(d.path());
        fs::create_dir_all(f.deferred_dir()).unwrap();
        fs::write(f.deferred_dir().join("spira_sp-a"), "4\n").unwrap();
        assert_eq!(f.bump_deferred("spira/sp-a", "spira", 9), 5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backed_off_tip_waits_out_its_window_and_a_moved_tip_does_not() {
        let d = crate::testutil::tmpdir("backoff");
        let f = Files::new(&d);
        assert!(!f.backing_off("spira/sp-a", "t1", 100, 300));
        f.back_off("spira/sp-a", "t1", 100);
        assert!(f.backing_off("spira/sp-a", "t1", 399, 300));
        assert!(!f.backing_off("spira/sp-a", "t1", 400, 300));
        assert!(!f.backing_off("spira/sp-a", "t2", 150, 300));
        f.clear_noverdict("spira/sp-a");
        assert!(!f.backing_off("spira/sp-a", "t1", 150, 300));
    }

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
