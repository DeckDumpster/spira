//! The fallback channel — `$SPIRA_RUN/auron.alerts.json`, written if and only if the beads
//! path failed, and REMOVED the moment it succeeds again, so the file's mere EXISTENCE is
//! the statement "beads could not be reached"; there is never a stale second source of
//! truth standing beside a working first one.

use std::path::Path;

use serde_json::{json, Value};

use crate::classify::Firing;

pub fn build(now: i64, db_reachable: bool, db_write_ok: Option<bool>, firing: &[Firing]) -> Value {
    json!({
        "at": now,
        "db_reachable": db_reachable,
        "db_write_ok": db_write_ok,
        "why": "beads could not be written; this file is Auron's secondary channel",
        "alerts": firing.iter().map(|f| json!({"key": f.key, "title": f.title, "evidence": f.evidence})).collect::<Vec<_>>(),
    })
}

/// Write-then-rename, so a reader never sees a half-written file.
pub fn write(path: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(value).unwrap_or_default() + "\n")?;
    std::fs::rename(&tmp, path)
}

pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_carries_db_write_ok_as_a_tri_state() {
        let v = build(100, true, None, &[]);
        assert!(v["db_write_ok"].is_null(), "unattempted must be null, never false");
        let v = build(100, true, Some(false), &[]);
        assert_eq!(v["db_write_ok"], false);
    }

    #[test]
    fn build_carries_the_firing_alerts() {
        let firing = vec![Firing { key: "k".into(), title: "T".into(), evidence: "e".into() }];
        let v = build(1, false, Some(false), &firing);
        assert_eq!(v["alerts"][0]["key"], "k");
        assert_eq!(v["db_reachable"], false);
    }

    #[test]
    fn write_then_remove_round_trips() {
        let dir = testkit::TempDir::new("auron-fallback");
        let p = dir.join("auron.alerts.json");
        write(&p, &build(1, false, Some(false), &[])).unwrap();
        assert!(p.is_file());
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"at\": 1"));
        remove(&p);
        assert!(!p.exists());
    }

    #[test]
    fn write_leaves_no_tmp_file_behind() {
        let dir = testkit::TempDir::new("auron-fallback");
        let p = dir.join("auron.alerts.json");
        write(&p, &build(1, true, Some(true), &[])).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains("tmp")).collect();
        assert!(leftovers.is_empty(), "no .tmp.<pid> file should survive a successful write: {leftovers:?}");
    }
}
