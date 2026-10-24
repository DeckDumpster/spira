use std::fs;
use std::path::PathBuf;

use crate::machine::{Event, Pilgrimage};

pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    pub fn new(dir: PathBuf) -> FileStore {
        FileStore { dir }
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn load(&self, id: &str) -> Option<Pilgrimage> {
        Pilgrimage::from_json(&fs::read_to_string(self.path(id)).ok()?).ok()
    }

    pub fn all(&self) -> Vec<Pilgrimage> {
        let mut ids: Vec<String> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").map(str::to_string))
            .collect();
        ids.sort();
        ids.iter().filter_map(|id| self.load(id)).collect()
    }

    /// Appends `event` at `at`, or refuses naming the state.
    pub fn record(&self, id: &str, event: Event, at: u64) -> Result<Pilgrimage, String> {
        if id.is_empty() || id.contains(['/', '.']) {
            return Err(format!("'{id}' is not a bead id"));
        }
        let mut p = self.load(id).unwrap_or_else(|| Pilgrimage::new(id));
        p.record(event, at)?;
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let tmp = self.path(id).with_extension("tmp");
        fs::write(&tmp, format!("{}\n", p.to_json())).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, self.path(id)).map_err(|e| format!("{}: {e}", self.path(id).display()))?;
        Ok(p)
    }
}
