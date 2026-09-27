use std::fs;
use std::path::Path;

pub fn read_landstate(dir: &Path, id: &str) -> Option<String> {
    fs::read_to_string(dir.join("landstate").join(id)).ok()
}
