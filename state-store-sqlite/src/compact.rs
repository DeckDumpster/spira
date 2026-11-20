//! Converts the control store's main file, once, to incremental auto-vacuum so that the
//! per-commit page reclaim can return free pages. Offline: the opener lock is held throughout.
//! Day files are not touched.

use std::path::Path;
use std::time::Instant;

use rusqlite::Connection;
use state_store::{Error, Result};

use crate::{db, take_opener, wal_of};

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub bytes_before: u64,
    pub free_bytes_before: u64,
    pub bytes_after: u64,
    pub free_bytes_after: u64,
    pub auto_vacuum: i64,
    pub elapsed_ms: u64,
}

impl Report {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "compact": "done",
            "bytes_before": self.bytes_before, "free_bytes_before": self.free_bytes_before,
            "bytes_after": self.bytes_after, "free_bytes_after": self.free_bytes_after,
            "auto_vacuum": self.auto_vacuum, "elapsed_ms": self.elapsed_ms,
        })
    }
}

fn usage(conn: &Connection, path: &Path) -> Result<(u64, u64)> {
    let page: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(db)?;
    let free: i64 = conn.pragma_query_value(None, "freelist_count", |r| r.get(0)).map_err(db)?;
    let bytes = std::fs::metadata(path).map_err(|e| Error::Backend(format!("{}: {e}", path.display())))?.len();
    Ok((bytes, u64::try_from(page * free).unwrap_or(0)))
}

/// Rewrites `path` with `auto_vacuum=INCREMENTAL`, or refuses and leaves it as it was.
pub fn compact(path: &Path) -> Result<Report> {
    let started = Instant::now();
    if !path.is_file() {
        return Err(Error::Backend(format!("{}: no such store", path.display())));
    }
    let _opener = take_opener(path)?;
    if std::fs::metadata(wal_of(path)).is_ok_and(|m| m.len() > 0) {
        return Err(Error::Backend(format!(
            "{} is not empty: the store did not close cleanly; start and stop the store once, then run compact again",
            wal_of(path).display()
        )));
    }
    let conn = Connection::open(path).map_err(db)?;
    let (bytes_before, free_bytes_before) = usage(&conn, path)?;
    conn.pragma_update(None, "auto_vacuum", 2).map_err(db)?;
    conn.prepare("VACUUM").and_then(|mut s| s.execute([])).map_err(db)?;
    let auto_vacuum: i64 = conn.pragma_query_value(None, "auto_vacuum", |r| r.get(0)).map_err(db)?;
    let (bytes_after, free_bytes_after) = usage(&conn, path)?;
    Ok(Report {
        bytes_before,
        free_bytes_before,
        bytes_after,
        free_bytes_after,
        auto_vacuum,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}
