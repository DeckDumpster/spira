use std::fs;
use std::path::Path;

pub fn certified_waiting(landstate_dir: &Path) -> u64 {
    let entries = fs::read_dir(landstate_dir);
    entries.map(|e| e.count() as u64).unwrap_or(0)
}
