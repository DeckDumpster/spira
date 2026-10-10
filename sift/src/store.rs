use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::machine::{Recorded, SiftEvent, Sifted};
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

    fn events_path(&self, id: &str) -> PathBuf {
        self.dir.join("events").join(format!("{id}.json"))
    }

    /// The recorded events of `id`. A bead counted before events existed is brought forward
    /// from its count file, each event saying it was backfilled.
    pub fn load(&self, id: &str) -> Option<Sifted> {
        if let Some(s) = fs::read_to_string(self.events_path(id)).ok().and_then(|t| Sifted::from_json(&t).ok()) {
            return Some(s);
        }
        let n: u32 = fs::read_to_string(self.count_path(id)).ok().and_then(|t| t.trim().parse().ok()).filter(|n| *n > 0)?;
        let mut s = Sifted::new(id);
        for k in 1..=n {
            s.events.push(Recorded { event: SiftEvent::SentBack { n: k, tip: String::new(), reason: "backfilled: counted before events were recorded".into() }, at: 0 });
        }
        Some(s)
    }

    /// Every bead with a recorded event, by id.
    pub fn all(&self) -> Vec<Sifted> {
        let mut ids: Vec<String> = [self.dir.join("events"), self.dir.join("send-backs")]
            .iter()
            .filter_map(|d| fs::read_dir(d).ok())
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().trim_end_matches(".json").to_string())
            .filter(|n| !n.starts_with('.') && !n.ends_with(".tmp"))
            .collect();
        ids.sort();
        ids.dedup();
        ids.iter().filter_map(|id| self.load(id)).collect()
    }

    /// Appends `event`, or refuses naming the state. `Ok(false)` is a repeat of the last event.
    pub fn record(&self, id: &str, event: SiftEvent) -> Result<bool, String> {
        let mut s = self.load(id).unwrap_or_else(|| Sifted::new(id));
        let moved = s.record(event, spira_config::vtime::now_epoch())?;
        if moved {
            write_atomic(&self.events_path(id), &format!("{}\n", s.to_json()))?;
        }
        Ok(moved)
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
        })
    }

    fn put(&self, v: &Verdict) -> Result<(), String> {
        let path = self.verdict_path(&v.id, &v.tip);
        let body = json!({"base": v.base, "conflict": v.conflict, "patch_id": v.patch_id, "stacked": v.stacked});
        write_atomic(&path, &body.to_string())
    }

    fn send_backs(&self, id: &str) -> u32 {
        self.load(id).map_or(0, |s| s.send_backs())
    }

    fn record_screened(&self, id: &str, tip: &str) -> Result<(), String> {
        self.record(id, SiftEvent::Screened { tip: tip.to_string() }).map(|_| ())
    }

    fn record_send_back(&self, id: &str, tip: &str, reason: &str) -> Result<u32, String> {
        let n = self.send_backs(id) + 1;
        self.record(id, SiftEvent::SentBack { n, tip: tip.to_string(), reason: reason.to_string() })?;
        Ok(n)
    }

    fn first_cap_report(&self, id: &str, tip: &str) -> bool {
        self.record(id, SiftEvent::Capped { tip: tip.to_string() }).unwrap_or(false)
    }
}

fn write_atomic(path: &PathBuf, body: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("no parent dir")?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}
