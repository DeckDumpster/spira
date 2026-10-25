use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    pub warned: BTreeSet<String>,
    pub seen: BTreeMap<String, BTreeSet<String>>,
    pub stop_done: bool,
}

fn file(dir: &Path, session: &str) -> PathBuf {
    let safe: String = session.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    dir.join(format!("{safe}.json"))
}

pub fn load(dir: &Path, session: &str) -> State {
    std::fs::read_to_string(file(dir, session)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(dir: &Path, session: &str, st: &State) {
    let _ = std::fs::create_dir_all(dir);
    if let Ok(t) = serde_json::to_string(st) {
        let _ = std::fs::write(file(dir, session), t);
    }
}
