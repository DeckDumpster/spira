use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::{Store, Verdict};

pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: PathBuf) -> FileStore {
        FileStore { dir }
    }

    fn verdict_path(&self, id: &str, tip: &str) -> PathBuf {
        self.dir.join("verdicts").join(format!("{id}-{tip}.json"))
    }

    fn count_path(&self, id: &str) -> PathBuf {
        self.dir.join("send-backs").join(id)
    }

    fn capped_path(&self, id: &str, tip: &str) -> PathBuf {
        self.dir.join("capped").join(format!("{id}-{tip}"))
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

impl Store for FileStore {
    fn get(&self, id: &str, tip: &str) -> Option<Verdict> {
        let v: Value = serde_json::from_str(&fs::read_to_string(self.verdict_path(id, tip)).ok()?).ok()?;
        Some(Verdict {
            id: id.to_string(),
            tip: tip.to_string(),
            base: v.get("base")?.as_str()?.to_string(),
            conflict: v.get("conflict").filter(|c| !c.is_null()).map(strings),
            patch_id: v.get("patch_id")?.as_str()?.to_string(),
            stacked: strings(v.get("stacked")?),
            lint: strings(v.get("lint")?),
        })
    }

    fn put(&self, v: &Verdict) -> Result<(), String> {
        let path = self.verdict_path(&v.id, &v.tip);
        let body = json!({"base": v.base, "conflict": v.conflict, "patch_id": v.patch_id, "stacked": v.stacked, "lint": v.lint});
        write_atomic(&path, &body.to_string())
    }

    fn send_backs(&self, id: &str) -> u32 {
        fs::read_to_string(self.count_path(id)).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0)
    }

    fn record_send_back(&self, id: &str) -> Result<u32, String> {
        let n = self.send_backs(id) + 1;
        write_atomic(&self.count_path(id), &n.to_string())?;
        Ok(n)
    }

    fn first_cap_report(&self, id: &str, tip: &str) -> bool {
        let path = self.capped_path(id, tip);
        if path.exists() {
            return false;
        }
        write_atomic(&path, "").is_ok()
    }
}

fn write_atomic(path: &PathBuf, body: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent dir")?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}
