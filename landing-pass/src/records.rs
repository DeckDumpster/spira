//! The pass's own files under $SPIRA_RUN (DESIGN.md §2.4). Landstate is READ here; every
//! landstate WRITE goes through this crate's own `landstate::mark` (its TSD dual-write),
//! lib.sh `land_mark` before sp-cnnt6 ("wave 4.16"). The rest are this pass's own records.

use crate::model::{LandState, RunRecord, StatusFile, Submitted};
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
    pub fn landstate_dir(&self) -> PathBuf {
        self.run.join("landstate")
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

    pub fn land_state(&self, id: &str) -> Option<LandState> {
        fs::read_to_string(self.landstate_dir().join(id)).ok().and_then(|t| LandState::parse(&t))
    }

    pub fn drop_ejected(&self, id: &str) {
        let _ = fs::remove_file(self.landstate_dir().join(format!("{id}.ejected")));
    }

    pub fn submitted(&self, id: &str) -> Option<Submitted> {
        fs::read_to_string(self.run.join("submitted").join(id)).ok().and_then(|t| Submitted::parse(&t))
    }

    /// lib.sh `mark_submitted <id> <tip> <state>` (refreshes 0), written atomically.
    pub fn mark_submitted(&self, id: &str, tip: &str, state: &str, now: u64) {
        self.mark_submitted_refreshed(id, tip, state, now, 0);
    }

    /// lib.sh `mark_submitted <id> <tip> <state> <refreshes>` — the pr pass's own refresh
    /// counter (DESIGN.md §6, land_pr).
    pub fn mark_submitted_refreshed(&self, id: &str, tip: &str, state: &str, now: u64, refreshes: u32) {
        let rec = Submitted { tip: tip.into(), at: now, state: state.into(), refreshes };
        let _ = atomic_write(&self.run.join("submitted").join(id), &rec.render());
    }

    /// A PASS clears the machinery-fault counters for this branch.
    pub fn clear_noverdict(&self, branch: &str) {
        let key = branch_key(branch);
        if let Ok(rd) = fs::read_dir(self.run.join("noverdict")) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&key) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
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

    /// Delete verdict-cache entries older than the gate's own TTL — one file at a time,
    /// never the directory (a gate may be writing into it).
    pub fn prune_verdicts(dir: &Path, ttl_secs: u64, now: u64) {
        // find -mmin +N: strictly older than N whole minutes.
        let mins = ttl_secs / 60;
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let Ok(mt) = md.modified() else { continue };
            let mtime = mt.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(now);
            let age_min_ceil = (now.saturating_sub(mtime) + 59) / 60;
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
