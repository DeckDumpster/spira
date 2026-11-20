//! The review ledger: one JSON row per unit sent, appended by `cr open`, read by `cr pick`.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerFile {
    pub path: String,
    pub blob: String,
    pub regions: Option<Vec<(u32, u32)>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub review: String,
    /// Milliseconds since the epoch.
    pub at: u64,
    pub commit: String,
    pub kind: String,
    pub unit_id: String,
    pub files: Vec<LedgerFile>,
}

#[derive(Debug, Clone, Default)]
pub struct Ledger {
    pub rows: Vec<Row>,
}

impl Ledger {
    pub fn from_rows(rows: Vec<Row>) -> Ledger {
        Ledger { rows }
    }

    /// A missing file is an empty ledger; a row that does not parse is an error naming its line.
    pub fn load(path: &Path) -> Result<Ledger, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let mut rows = Vec::new();
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            rows.push(serde_json::from_str(line).map_err(|e| format!("{} line {}: {e}", path.display(), n + 1))?);
        }
        Ok(Ledger { rows })
    }

    pub fn append(path: &Path, row: &Row) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        let mut line = serde_json::to_string(row).map_err(|e| e.to_string())?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
        f.write_all(line.as_bytes()).map_err(|e| format!("writing {}: {e}", path.display()))
    }

    pub fn last_sent_of_kind(&self, kind: &str) -> Option<u64> {
        self.rows.iter().filter(|r| r.kind == kind).map(|r| r.at).max()
    }

    pub fn last_sent_of_any(&self, paths: &[&str]) -> Option<u64> {
        self.rows.iter().filter(|r| r.files.iter().any(|f| paths.contains(&f.path.as_str()))).map(|r| r.at).max()
    }

    pub fn sent_at_blob(&self, path: &str, blob: &str) -> bool {
        self.rows.iter().any(|r| r.files.iter().any(|f| f.path == path && f.blob == blob))
    }
}
