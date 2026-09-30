//! The pass's own files under $SPIRA_RUN (DESIGN.md §2.4). Landstate is READ here; every
//! landstate WRITE goes through lib.sh `land_mark` (its TSD dual-write). The rest are this
//! pass's own records.

use crate::model::{LandState, RunRecord, StatusFile, Submitted};
use crate::util::{atomic_write, branch_key};
use std::fs;
use std::path::{Path, PathBuf};

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

    /// Bump a branch's budget-deferral counter; returns the new count.
    pub fn bump_deferred(&self, branch: &str) -> u32 {
        let f = self.deferred_file(branch);
        let n = fs::read_to_string(&f).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0) + 1;
        let _ = atomic_write(&f, &format!("{n}\n"));
        n
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
