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
    last: Option<Value>,
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
    pub fn start(run_dir: &Path, results_dir: &Path, round: &str, pass: &str, total: usize, cap: u64) -> Progress {
        let mut p = Progress {
            path: run_dir.join(FILE),
            round: round.to_string(),
            pass: pass.to_string(),
            total,
            cap,
            baseline: result_names(results_dir).into_iter().map(|(n, _)| n).collect(),
            build_started: now_secs(),
            suites_started: None,
            last: None,
        };
        p.write(results_dir, "build", None);
        p
    }

    fn write(&mut self, results_dir: &Path, phase: &str, verdict: Option<&str>) {
        let fresh: Vec<(String, String)> = result_names(results_dir).into_iter().filter(|(n, _)| !self.baseline.contains(n)).collect();
        let started = fresh.len() > 0 || crate::run::build_wall(results_dir).is_some();
        let phase = if phase == "build" && started { "suites" } else { phase };
        if phase != "build" && self.suites_started.is_none() {
            self.suites_started = Some(now_secs());
        }
        let unit = unit_of(results_dir);
        let phase = match &unit {
            Some((k, m, _)) if phase == "suites" && k < m && total_done(&fresh, self.total) => "unit",
            _ => phase,
        };
        let red: Vec<&str> = fresh.iter().filter(|(_, v)| v == "red").map(|(n, _)| n.as_str()).collect();
        let mut body = json!({
            "round": self.round,
            "pass": self.pass,
            "phase": phase,
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
        body["updated_at"] = json!(now_secs());
        if let Err(e) = write_atomic(&self.path, &format!("{body}\n")) {
            eprintln!("round-vm run: progress: {e}");
        }
    }

    /// Called after each streaming pull.
    pub fn update(&mut self, results_dir: &Path) {
        self.write(results_dir, "build", None);
    }

    pub fn finish(&mut self, results_dir: &Path, rc: i32) {
        self.write(results_dir, "done", Some(verdict_for(rc)));
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

    fn read(p: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(p.join(FILE)).unwrap()).unwrap()
    }

    #[test]
    fn the_file_follows_the_pass_through_build_suites_and_done() {
        let d = TempDir::new();
        let (run, res) = (d.path().join("run"), d.path().join("res"));
        fs::create_dir_all(&res).unwrap();
        fs::write(res.join("test-old.sh.result"), "red 1 5 - p e 1\n").unwrap();
        let mut p = Progress::start(&run, &res, "r1", "p1", 3, 777);
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
        let mut p3 = Progress::start(&run, &res, "r1", "p1", 3, 777);
        p3.baseline.clear();
        p3.update(&res);
        let v = read(&run);
        assert_eq!(v["phase"], "unit", "suites all in, unit binaries still running");
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
        let _p = Progress::start(&run, &res, "r", "p", 1, 9);
        assert_eq!(read(&run)["phase"], "build", "seen not-done first");
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
}
