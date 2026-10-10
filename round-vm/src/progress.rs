//! The running pass's live progress, `<run>/round-progress.json`: rewritten atomically on every
//! finished suite and at each phase change, whoever drives the pass.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::spool::write_atomic;

pub const FILE: &str = "round-progress.json";
pub const UNIT_FILE: &str = "unit-progress";
pub const UNIT_BUILD_FILE: &str = "unit-build";

/// Seconds a phase may go without a change of phase or detail before it reports itself stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    pub vm: u64,
    pub build: u64,
}

impl Budgets {
    fn of(&self, phase: &str) -> Option<u64> {
        match phase {
            "vm" => Some(self.vm),
            "build" => Some(self.build),
            _ => None,
        }
    }
}

/// The VM's `unit-progress` line, `k M [red binaries...]`.
fn unit_of(results_dir: &Path) -> Option<(usize, usize, Vec<String>)> {
    let t = fs::read_to_string(results_dir.join(UNIT_FILE)).ok()?;
    let mut w = t.split_whitespace();
    let (k, m) = (w.next()?.parse().ok()?, w.next()?.parse().ok()?);
    Some((k, m, w.map(str::to_string).collect()))
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub struct Progress {
    path: PathBuf,
    round: String,
    pass: String,
    total: usize,
    cap: u64,
    baseline: HashSet<String>,
    build_started: u64,
    suites_started: Option<u64>,
    suites_reported: bool,
    last: Option<Value>,
    base: &'static str,
    detail: Option<String>,
    seen_detail: Option<String>,
    budgets: Budgets,
    clock: Box<dyn Fn() -> u64 + Send>,
    current: String,
    phase_started: u64,
    last_change: u64,
    phases: Vec<(String, u64)>,
}

fn result_names(dir: &Path) -> Vec<(String, String)> {
    let Ok(rd) = fs::read_dir(dir) else { return vec![] };
    let mut v: Vec<(String, String)> = rd
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "result").unwrap_or(false))
        .map(|e| {
            let first = fs::read_to_string(e.path()).ok().and_then(|t| t.split_whitespace().next().map(str::to_string)).unwrap_or_default();
            let name = e.path().file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            (name, first)
        })
        .collect();
    v.sort();
    v
}

fn total_done(fresh: &[(String, String)], total: usize) -> bool {
    total > 0 && fresh.len() >= total
}

impl Progress {
    /// Results already in `results_dir` belong to an earlier pass and are not counted.
    pub fn start(run_dir: &Path, results_dir: &Path, round: &str, pass: &str, total: usize, cap: u64, budgets: Budgets) -> Progress {
        Self::start_with_clock(run_dir, results_dir, round, pass, total, cap, budgets, Box::new(now_secs))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_with_clock(
        run_dir: &Path,
        results_dir: &Path,
        round: &str,
        pass: &str,
        total: usize,
        cap: u64,
        budgets: Budgets,
        clock: Box<dyn Fn() -> u64 + Send>,
    ) -> Progress {
        let t0 = clock();
        let mut p = Progress {
            path: run_dir.join(FILE),
            round: round.to_string(),
            pass: pass.to_string(),
            total,
            cap,
            baseline: result_names(results_dir).into_iter().map(|(n, _)| n).collect(),
            build_started: t0,
            suites_started: None,
            suites_reported: false,
            last: None,
            base: "vm",
            detail: Some("leasing a VM".into()),
            seen_detail: None,
            budgets,
            clock,
            current: String::new(),
            phase_started: t0,
            last_change: t0,
            phases: Vec::new(),
        };
        p.write(results_dir, None);
        p
    }

    /// Moves the pass into `phase` (`vm`, `build`, `salvage`), with its own clock.
    pub fn enter(&mut self, results_dir: &Path, phase: &'static str, detail: Option<String>) {
        self.base = phase;
        self.detail = detail;
        self.write(results_dir, None);
    }

    /// Says what the current phase is doing; a changed detail counts as progress.
    pub fn detail(&mut self, results_dir: &Path, detail: String) {
        self.detail = Some(detail);
        self.write(results_dir, None);
    }

    pub fn tick(&mut self, results_dir: &Path) {
        self.write(results_dir, None);
    }

    fn write(&mut self, results_dir: &Path, verdict: Option<&str>) {
        let now = (self.clock)();
        let fresh: Vec<(String, String)> = result_names(results_dir).into_iter().filter(|(n, _)| !self.baseline.contains(n)).collect();
        let started = !fresh.is_empty() || crate::run::build_wall(results_dir).is_some();
        let unit = unit_of(results_dir);
        let phase = match self.base {
            "build" if started => "suites",
            "build" if fs::read_to_string(results_dir.join(UNIT_BUILD_FILE)).map(|t| t.trim() == "building").unwrap_or(false) => "unit-build",
            b => b,
        };
        let phase = match &unit {
            Some((k, m, _)) if phase == "suites" && k < m && total_done(&fresh, self.total) => "unit-tests",
            _ => phase,
        };
        if matches!(phase, "suites" | "unit-tests") && self.suites_started.is_none() {
            self.suites_started = Some(now);
        }
        let detail_changed = self.seen_detail != self.detail;
        self.seen_detail = self.detail.clone();
        if phase != self.current {
            self.current = phase.to_string();
            self.phase_started = now;
            self.last_change = now;
            self.phases.push((phase.to_string(), now));
        } else if detail_changed {
            self.last_change = now;
        }
        let detail = match self.budgets.of(phase) {
            Some(budget) if now.saturating_sub(self.last_change) >= budget => {
                Some(format!("stalled: no progress in {budget}s{}", self.detail.as_deref().map(|d| format!(" (last: {d})")).unwrap_or_default()))
            }
            _ => self.detail.clone(),
        };
        let red: Vec<&str> = fresh.iter().filter(|(_, v)| v == "red").map(|(n, _)| n.as_str()).collect();
        let mut body = json!({
            "round": self.round,
            "pass": self.pass,
            "phase": phase,
            "phase_started": self.phase_started,
            "detail": detail,
            "phases": self.phases.iter().map(|(p, t)| json!({"phase": p, "started": t})).collect::<Vec<_>>(),
            "verdict": verdict,
            "done": fresh.len(),
            "total": self.total,
            "red": red,
            "unit": unit.map(|(k, m, r)| json!({"done": k, "total": m, "red": r})),
            "build_started": self.build_started,
            "suites_started": self.suites_started,
            "cap": self.cap,
        });
        if self.last.as_ref() == Some(&body) {
            return;
        }
        self.last = Some(body.clone());
        body["updated_at"] = json!(now);
        if let Err(e) = write_atomic(&self.path, &format!("{body}\n")) {
            eprintln!("round-vm run: progress: {e}");
        }
    }

    /// True once, the first time the pass is seen past its build.
    pub fn take_suites_began(&mut self) -> bool {
        let began = self.suites_started.is_some() && !self.suites_reported;
        self.suites_reported |= began;
        began
    }

    /// Called after each streaming pull.
    pub fn update(&mut self, results_dir: &Path) {
        self.write(results_dir, None);
    }

    pub fn finish(&mut self, results_dir: &Path, rc: i32) {
        self.base = "done";
        self.detail = None;
        self.write(results_dir, Some(verdict_for(rc)));
    }
}

pub fn verdict_for(rc: i32) -> &'static str {
    match rc {
        0 => "green",
        1 => "red",
        _ => "fault",
    }
}

/// A run cut by a signal: whatever the file last said becomes `done` with verdict `killed`.
pub fn mark_killed(run_dir: &Path) {
    let path = run_dir.join(FILE);
    let Some(mut v) = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return };
    if v["phase"] == "done" {
        return;
    }
    v["phase"] = json!("done");
    v["verdict"] = json!("killed");
    v["updated_at"] = json!(now_secs());
    let _ = write_atomic(&path, &format!("{v}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const BUDGETS: Budgets = Budgets { vm: 300, build: 600 };

    fn read(p: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(p.join(FILE)).unwrap()).unwrap()
    }

    #[test]
    fn the_file_follows_the_pass_through_build_suites_and_done() {
        let d = TempDir::new();
        let (run, res) = (d.path().join("run"), d.path().join("res"));
        fs::create_dir_all(&res).unwrap();
        fs::write(res.join("test-old.sh.result"), "red 1 5 - p e 1\n").unwrap();
        let mut p = Progress::start(&run, &res, "r1", "p1", 3, 777, BUDGETS);
        assert_eq!(read(&run)["phase"], "vm");
        p.enter(&res, "build", None);
        let v = read(&run);
        assert_eq!((v["phase"].as_str(), v["done"].as_u64(), v["total"].as_u64(), v["cap"].as_u64()), (Some("build"), Some(0), Some(3), Some(777)));
        assert!(v["suites_started"].is_null() && v["verdict"].is_null());

        fs::write(res.join("runner.meta"), "build_wall_s=90\n").unwrap();
        p.update(&res);
        let v = read(&run);
        assert_eq!(v["phase"], "suites");
        assert!(v["suites_started"].as_u64().unwrap() > 0);

        fs::write(res.join("test-a.sh.result"), "ok 1 30 - p e 0\n").unwrap();
        fs::write(res.join("test-b.sh.result"), "red 1 12 fp p e 1\n").unwrap();
        p.update(&res);
        let v = read(&run);
        assert_eq!((v["done"].as_u64(), v["red"].clone()), (Some(2), json!(["test-b.sh"])), "the earlier pass's red is not this pass's");

        fs::write(res.join(UNIT_FILE), "1 4 alpha\n").unwrap();
        fs::write(res.join("test-c.sh.result"), "ok 1 3 - p e 0\n").unwrap();
        let mut p3 = Progress::start(&run, &res, "r1", "p1", 3, 777, BUDGETS);
        p3.enter(&res, "build", None);
        p3.baseline.clear();
        p3.update(&res);
        let v = read(&run);
        assert_eq!(v["phase"], "unit-tests", "suites all in, unit binaries still running");
        assert_eq!((v["unit"]["done"].as_u64(), v["unit"]["total"].as_u64(), v["unit"]["red"].clone()), (Some(1), Some(4), json!(["alpha"])));

        p.finish(&res, 1);
        let v = read(&run);
        assert_eq!((v["phase"].as_str(), v["verdict"].as_str()), (Some("done"), Some("red")));
        assert_eq!(read(&run)["round"], "r1");
    }

    #[test]
    fn a_killed_run_is_left_done_with_its_verdict() {
        let d = TempDir::new();
        let (run, res) = (d.path().join("run"), d.path().join("res"));
        fs::create_dir_all(&res).unwrap();
        let _p = Progress::start(&run, &res, "r", "p", 1, 9, BUDGETS);
        assert_eq!(read(&run)["phase"], "vm", "seen not-done first");
        mark_killed(&run);
        let v = read(&run);
        assert_eq!((v["phase"].as_str(), v["verdict"].as_str()), (Some("done"), Some("killed")));
        mark_killed(&run);
        assert_eq!(read(&run)["verdict"], "killed");
    }

    #[test]
    fn verdicts_follow_the_exit_code() {
        assert_eq!((verdict_for(0), verdict_for(1), verdict_for(2), verdict_for(4)), ("green", "red", "fault", "fault"));
    }

    fn clocked(run: &Path, res: &Path) -> (Progress, std::sync::Arc<std::sync::atomic::AtomicU64>) {
        let t = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1000));
        let c = t.clone();
        let p = Progress::start_with_clock(run, res, "r", "p", 2, 9, BUDGETS, Box::new(move || c.load(std::sync::atomic::Ordering::SeqCst)));
        (p, t)
    }

    fn at(t: &std::sync::atomic::AtomicU64, v: u64) {
        t.store(v, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn every_phase_of_a_pass_carries_its_own_start() {
        let d = TempDir::new();
        let (run, res) = (d.path().join("run"), d.path().join("res"));
        fs::create_dir_all(&res).unwrap();
        let (mut p, t) = clocked(&run, &res);
        assert_eq!((read(&run)["phase"].as_str(), read(&run)["phase_started"].as_u64()), (Some("vm"), Some(1000)));
        at(&t, 1040);
        p.detail(&res, "provisioning VM 109".into());
        assert_eq!(read(&run)["detail"], "provisioning VM 109");
        assert_eq!(read(&run)["phase_started"], 1000, "a detail is not a new phase");
        at(&t, 1100);
        p.enter(&res, "build", None);
        assert_eq!((read(&run)["phase"].as_str(), read(&run)["phase_started"].as_u64()), (Some("build"), Some(1100)));
        fs::write(res.join(UNIT_BUILD_FILE), "building\n").unwrap();
        at(&t, 1200);
        p.update(&res);
        assert_eq!((read(&run)["phase"].as_str(), read(&run)["phase_started"].as_u64()), (Some("unit-build"), Some(1200)));
        fs::write(res.join(UNIT_BUILD_FILE), "built\n").unwrap();
        fs::write(res.join("runner.meta"), "build_wall_s=9\n").unwrap();
        at(&t, 1300);
        p.update(&res);
        assert_eq!((read(&run)["phase"].as_str(), read(&run)["phase_started"].as_u64()), (Some("suites"), Some(1300)));
        fs::write(res.join(UNIT_FILE), "1 4\n").unwrap();
        fs::write(res.join("test-a.sh.result"), "ok 1 3 - p e 0\n").unwrap();
        fs::write(res.join("test-b.sh.result"), "ok 1 3 - p e 0\n").unwrap();
        at(&t, 1400);
        p.update(&res);
        assert_eq!((read(&run)["phase"].as_str(), read(&run)["phase_started"].as_u64()), (Some("unit-tests"), Some(1400)));
        at(&t, 1500);
        p.enter(&res, "salvage", None);
        assert_eq!((read(&run)["phase"].as_str(), read(&run)["phase_started"].as_u64()), (Some("salvage"), Some(1500)));
        at(&t, 1600);
        p.finish(&res, 0);
        let v = read(&run);
        assert_eq!((v["phase"].as_str(), v["phase_started"].as_u64()), (Some("done"), Some(1600)));
        let seq: Vec<(&str, u64)> = v["phases"].as_array().unwrap().iter().map(|e| (e["phase"].as_str().unwrap(), e["started"].as_u64().unwrap())).collect();
        assert_eq!(
            seq,
            [("vm", 1000), ("build", 1100), ("unit-build", 1200), ("suites", 1300), ("unit-tests", 1400), ("salvage", 1500), ("done", 1600)]
        );
    }

    #[test]
    fn a_wedged_vm_phase_reports_stalled_when_its_budget_runs_out() {
        let d = TempDir::new();
        let (run, res) = (d.path().join("run"), d.path().join("res"));
        fs::create_dir_all(&res).unwrap();
        let (mut p, t) = clocked(&run, &res);
        p.detail(&res, "provisioning VM 109".into());
        at(&t, 1299);
        p.tick(&res);
        assert_eq!(read(&run)["detail"], "provisioning VM 109", "inside its budget");
        at(&t, 1300);
        p.tick(&res);
        let d = read(&run)["detail"].as_str().unwrap().to_string();
        assert_eq!(d, "stalled: no progress in 300s (last: provisioning VM 109)");
        p.detail(&res, "VM 109 ready".into());
        assert_eq!(read(&run)["detail"], "VM 109 ready", "a new detail is progress");
    }

    #[test]
    fn a_wedged_build_reports_stalled_on_its_own_budget() {
        let d = TempDir::new();
        let (run, res) = (d.path().join("run"), d.path().join("res"));
        fs::create_dir_all(&res).unwrap();
        let (mut p, t) = clocked(&run, &res);
        p.enter(&res, "build", None);
        at(&t, 1599);
        p.tick(&res);
        assert!(read(&run)["detail"].is_null());
        at(&t, 1600);
        p.tick(&res);
        assert_eq!(read(&run)["detail"], "stalled: no progress in 600s");
    }
}
