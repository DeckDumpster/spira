//! Where the watchers' snapshots come from: files under the run directory and the lifecycle
//! store through spira-lc's socket. No process is launched.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use runtime::socket::lc_call;
use serde_json::Value;

use crate::watchers::round_duty::{RoundState, Snapshot};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct World {
    pub run_dir: PathBuf,
    pub queue_dir: PathBuf,
    pub repo_map: PathBuf,
    pub home_repo: PathBuf,
    pub inbox: PathBuf,
    pub lc_socket: PathBuf,
}

struct MapRow {
    name: String,
    path: String,
    land: String,
}

fn map_rows(text: &str) -> Vec<MapRow> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('|').map(str::trim).collect();
            (f.len() >= 3 && !f[0].is_empty()).then(|| MapRow { name: f[0].into(), path: f[1].into(), land: f[2].into() })
        })
        .collect()
}

fn lane_stopped(run_dir: &Path, repo: &str, lane: &str) -> bool {
    let Ok(text) = fs::read_to_string(run_dir.join("scoped-stops").join(format!("repo-{repo}.json"))) else { return false };
    serde_json::from_str::<Value>(&text).ok().and_then(|v| v["lanes"].as_array().map(|a| a.iter().any(|l| l == lane))).unwrap_or(false)
}

fn kv_get(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')).map(str::to_string))
}

impl World {
    fn lc(&self, argv: &[&str]) -> Option<String> {
        lc_call(&self.lc_socket, argv).ok()
    }

    /// Unheld certified beads of one repo; `None` when the lifecycle did not answer.
    fn certified(&self, repo: &str) -> Option<u64> {
        let rows: Value = serde_json::from_str(&self.lc(&["list", "--state", "CERTIFIED", "--repo", repo])?).ok()?;
        let held = |r: &Value| r["holds"].as_array().is_some_and(|h| !h.is_empty());
        Some(rows.as_array()?.iter().filter(|r| r.get("bead_id").is_some() && !held(r)).count() as u64)
    }

    /// The rounds the lifecycle still holds open for the home repo, from the queue's records.
    fn open_rounds(&self) -> Option<u64> {
        let rows = fs::read_to_string(&self.repo_map).ok().map(|t| map_rows(&t))?;
        let home = rows.iter().find(|r| Path::new(&r.path) == self.home_repo)?;
        let dir = self.queue_dir.join(&home.name);
        let mut live = 0;
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(0),
            Err(_) => return None,
        };
        let mut records: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n == "round" || n.starts_with("round."))).collect();
        records.sort();
        for p in records {
            let Ok(text) = fs::read_to_string(&p) else { continue };
            let Some(batch) = kv_get(&text, "batch_id").filter(|b| !b.is_empty()) else { continue };
            let dead = self.lc(&["show-batch", &batch]).and_then(|b| serde_json::from_str::<Value>(&b).ok()).is_some_and(|v| matches!(v["state"].as_str(), Some("ABANDONED" | "DISCARDED")));
            if !dead {
                live += 1;
            }
        }
        Some(live)
    }

    fn results(&self, known: &BTreeMap<String, u64>) -> (BTreeMap<String, u64>, Vec<(String, String)>) {
        let mut lines = BTreeMap::new();
        let mut fresh = vec![];
        let mut files: Vec<PathBuf> = fs::read_dir(self.run_dir.join("rounds")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "result")).collect();
        files.sort();
        for f in files {
            let Ok(text) = fs::read_to_string(&f) else { continue };
            let all: Vec<&str> = text.lines().collect();
            let name = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            if let Some(&seen) = known.get(&name) {
                fresh.extend(all.iter().skip(seen as usize).map(|l| (name.clone(), l.to_string())));
            }
            lines.insert(name, all.len() as u64);
        }
        (lines, fresh)
    }

    pub fn round_snapshot(&self, input: &str) -> Result<String, String> {
        let known: BTreeMap<String, u64> = serde_json::from_str(input).unwrap_or_default();
        let rows = fs::read_to_string(&self.repo_map).map(|t| map_rows(&t)).map_err(|e| format!("{}: {e}", self.repo_map.display()))?;
        let (mut certified, mut by_repo, mut stopped) = (0, vec![], vec![]);
        for r in rows.iter().filter(|r| r.land == "queue.local") {
            if lane_stopped(&self.run_dir, &r.name, "rounds") {
                stopped.push(r.name.clone());
                continue;
            }
            let n = self.certified(&r.name).ok_or_else(|| format!("lifecycle did not answer for {}", r.name))?;
            if n > 0 {
                certified += n;
                by_repo.push(format!("{}={n}", r.name));
            }
        }
        let open = self.open_rounds();
        let (lines, fresh) = self.results(&known);
        let snap = Snapshot {
            certified,
            by_repo: by_repo.join(" "),
            stopped: stopped.join(","),
            state: match open {
                None => RoundState::Unknown,
                Some(0) => RoundState::None,
                Some(_) => RoundState::Open,
            },
            open: open.unwrap_or(0),
            lines,
            fresh,
        };
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    pub fn inbox_append(&self, line: &str) -> Result<String, String> {
        if let Some(dir) = self.inbox.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.inbox).map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| e.to_string())?;
        Ok("appended".into())
    }
}
