//! One shared, short-lived copy of the claimable set for the counting readers.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::rank::ReadyRow;
use crate::store::Config;

fn path_for(cfg: &Config, key: &str) -> PathBuf {
    let h = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    PathBuf::from(&cfg.run).join("ready-cache").join(format!("{h:016x}.json"))
}

/// The cached rows while younger than `ready_ttl_s`, else `compute`'s, stored for the next
/// reader. A failed compute is returned and never stored; an unreadable or unwritable cache
/// is a miss, never an error.
pub fn read_or(cfg: &Config, key: &str, compute: impl FnOnce() -> Result<Vec<ReadyRow>, String>) -> Result<Vec<ReadyRow>, String> {
    if cfg.ready_ttl_s == 0 {
        return compute();
    }
    let path = path_for(cfg, key);
    let ttl = Duration::from_secs(cfg.ready_ttl_s as u64);
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ttl);
    if fresh {
        if let Some(rows) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            return Ok(rows);
        }
    }
    let rows = compute()?;
    if let (Some(dir), Ok(text)) = (path.parent(), serde_json::to_string(&rows)) {
        let tmp = dir.join(format!(".{}.{}", std::process::id(), path.file_name().and_then(|n| n.to_str()).unwrap_or("rows")));
        if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    Ok(rows)
}
