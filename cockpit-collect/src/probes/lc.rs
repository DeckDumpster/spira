//! Bulk reads of the lifecycle store (`spira-lc list`). `None` is CANNOT TELL — the program
//! is missing, timed out or answered unparseably — kept apart from `Some(empty)`, a real zero.

use crate::io;
use crate::quoting::parse_iso8601;
use serde_json::Value;
pub use spira_config::lc_state::{self, Row};
use std::collections::HashMap;
use std::process::{Command, Stdio};

pub struct LcRow {
    pub id: String,
    pub state: String,
    pub reason: String,
    pub updated_at: Option<i64>,
}

fn epoch(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok().or_else(|| parse_iso8601(&s.replace(' ', "T"))),
        _ => None,
    }
}

pub fn parse_rows(raw: &str) -> Option<Vec<LcRow>> {
    let Value::Array(rows) = serde_json::from_str::<Value>(raw).ok()? else { return None };
    let text = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Some(
        rows.iter()
            .map(|r| LcRow {
                id: text(r, "bead_id"),
                state: text(r, "state"),
                reason: text(r, "reason"),
                updated_at: r.get("updated_at").and_then(epoch),
            })
            .collect(),
    )
}

const FAIL_LIMIT: u32 = 3;
const FAIL_WINDOW_SECS: i64 = 30;

/// Keeps the last good answer of a lifecycle read across probe processes. A failed read serves
/// it, marked stale with its age, until `FAIL_LIMIT` consecutive failures or `FAIL_WINDOW_SECS`
/// since the first one — only then is it CANNOT TELL.
fn last_good(dir: &std::path::Path, key: &str, now: i64, fetch: impl FnOnce() -> Option<String>) -> Option<String> {
    let _ = std::fs::create_dir_all(dir);
    let (good, fails, stale) = (dir.join(format!("{key}.json")), dir.join(format!("{key}.fails")), dir.join(format!("{key}.stale")));
    if let Some(raw) = fetch() {
        let _ = std::fs::write(&good, &raw);
        let _ = std::fs::remove_file(&fails);
        let _ = std::fs::remove_file(&stale);
        return Some(raw);
    }
    let (n, since) = std::fs::read_to_string(&fails)
        .ok()
        .and_then(|t| {
            let (n, since) = t.trim().split_once(' ')?;
            Some((n.parse::<u32>().ok()?, since.parse::<i64>().ok()?))
        })
        .unwrap_or((0, now));
    let (n, since) = (n + 1, since);
    let _ = std::fs::write(&fails, format!("{n} {since}"));
    let raw = std::fs::read_to_string(&good).ok().filter(|_| n < FAIL_LIMIT && now - since < FAIL_WINDOW_SECS)?;
    let taken = std::fs::metadata(&good).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(now, |d| d.as_secs() as i64);
    let _ = std::fs::write(&stale, (now - taken).max(0).to_string());
    Some(raw)
}

fn cache_dir() -> std::path::PathBuf {
    io::run_dir().join("lc-snapshot")
}

/// Age in seconds of the stalest lifecycle snapshot a probe served in place of a failed read;
/// 0 when every read was live.
pub fn stale_age() -> i64 {
    std::fs::read_dir(cache_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "stale"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok()?.trim().parse::<i64>().ok())
        .max()
        .unwrap_or(0)
}

/// Rows in `state`, or every row when `state` is `None`.
pub fn list(state: Option<&str>) -> Option<Vec<LcRow>> {
    let key = format!("list-{}", state.unwrap_or("all"));
    parse_rows(&last_good(&cache_dir(), &key, io::now(), || list_live(state))?)
}

fn list_live(state: Option<&str>) -> Option<String> {
    let bin = std::env::var("SPIRA_LC_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira-lc".into());
    let timeout = std::env::var("SPIRA_LC_TIMEOUT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "5".into());
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout).arg(bin).arg("list");
    if let Some(s) = state {
        cmd.args(["--state", s]);
    }
    let o = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&o.stdout).into_owned();
    parse_rows(&raw).map(|_| raw)
}

/// Every lifecycle row as `spira_config::lc_state` reads it, keyed by bead id: a work bead's
/// state, holder and holds for the probes that used to read bd `status` (sp-mve9i, design
/// §3.4: "people and the cockpit read state from spira-lc"). `None` is CANNOT TELL.
pub fn state_index() -> Option<HashMap<String, Row>> {
    let raw = last_good(&cache_dir(), "state-index", io::now(), || lc_state::list_raw().ok().filter(|r| lc_state::parse_rows(r).is_ok()))?;
    lc_state::parse_rows(&raw).ok().map(lc_state::index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_with_null_reason_and_string_or_numeric_time() {
        let r = parse_rows(r#"[{"bead_id":"a","state":"REWORK","reason":null,"updated_at":"2026-09-30 00:00:00"},{"bead_id":"b","state":"X","reason":"gate","updated_at":"5"}]"#).unwrap();
        assert_eq!((r[0].reason.as_str(), r[0].updated_at), ("", Some(1790726400)));
        assert_eq!((r[1].reason.as_str(), r[1].updated_at), ("gate", Some(5)));
    }

    #[test]
    fn unparseable_or_non_array_is_cannot_tell() {
        assert!(parse_rows("cannot tell: x").is_none());
        assert!(parse_rows("{}").is_none());
        assert_eq!(parse_rows("[]").unwrap().len(), 0);
    }

    #[test]
    fn a_failed_read_serves_the_last_good_snapshot_then_gives_up() {
        let t = testkit::TempDir::new("lc-limit");
        let d = t.join("snap");
        assert_eq!(last_good(&d, "k", 100, || Some("[1]".into())).as_deref(), Some("[1]"));
        assert_eq!(last_good(&d, "k", 101, || None).as_deref(), Some("[1]"), "first failure is stale, not cannot-tell");
        assert_eq!(last_good(&d, "k", 102, || None).as_deref(), Some("[1]"));
        assert_eq!(last_good(&d, "k", 103, || None), None, "third consecutive failure shows");
        assert_eq!(last_good(&d, "k", 104, || Some("[2]".into())).as_deref(), Some("[2]"));
        assert_eq!(last_good(&d, "k", 105, || None).as_deref(), Some("[2]"), "a success resets the count");
    }

    #[test]
    fn failures_older_than_the_window_show_even_below_the_count() {
        let t = testkit::TempDir::new("lc-window");
        let d = t.join("snap");
        last_good(&d, "k", 100, || Some("[1]".into()));
        assert!(last_good(&d, "k", 101, || None).is_some());
        assert_eq!(last_good(&d, "k", 140, || None), None);
    }

    #[test]
    fn a_failure_with_nothing_remembered_is_cannot_tell_and_a_stale_serve_leaves_its_age() {
        let t = testkit::TempDir::new("lc-age");
        let d = t.join("snap");
        assert_eq!(last_good(&d, "k", 100, || None), None);
        last_good(&d, "j", 100, || Some("[]".into()));
        last_good(&d, "j", 105, || None);
        assert!(std::fs::read_to_string(d.join("j.stale")).unwrap().parse::<i64>().is_ok());
        last_good(&d, "j", 106, || Some("[]".into()));
        assert!(!d.join("j.stale").exists(), "a live read clears the marker");
    }
}
