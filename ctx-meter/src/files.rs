//! The small text files under `SPIRA_RUN` the meter reads and writes: `key=value` lines and
//! write-by-rename.

use std::collections::HashMap;
use std::path::Path;

/// `key=value` lines of the file at `path`; later lines win. `trim` strips each line's whole
/// surround (the archivist's state), otherwise only its newline (the pointer, the cursor).
pub fn read_kv(path: &Path, trim: bool) -> std::io::Result<HashMap<String, String>> {
    let text = String::from_utf8_lossy(&std::fs::read(path)?).into_owned();
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = if trim { line.trim() } else { line };
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.to_string(), v.to_string());
        }
    }
    Ok(out)
}

/// `body` to `path` by rename from a pid-named sibling, creating the directory; a reader never
/// sees a partial file. Best effort: every file written here is an optimisation or an instrument.
pub fn write_by_rename(path: &Path, body: &str) {
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = format!("{}.{}", path.display(), std::process::id());
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, path)
    };
    let _ = write();
}
