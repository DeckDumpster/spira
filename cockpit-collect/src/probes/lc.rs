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
    parse_rows(&last_good(&cache_dir(), &key, io::now(), || shared_live(&key, || list_live(state)))?)
}

/// One lifecycle read per `SPIRA_PANE_READ_TTL` across every probe process.
pub fn shared_live(key: &str, fetch: impl FnOnce() -> Option<String>) -> Option<String> {
    let ttl = io::ttl_from_env("SPIRA_PANE_READ_TTL", io::PANE_TTL_SECS);
    io::shared_read(&cache_dir().join("live"), key, ttl, io::now(), fetch)
}

fn list_live(state: Option<&str>) -> Option<String> {
    let mut args = vec!["list"];
    if let Some(s) = state {
        args.extend(["--state", s]);
    }
    let raw = verb(&args)?;
    parse_rows(&raw).map(|_| raw)
}

/// `spira-lc <args>` under the configured timeout; `None` when it failed.
pub fn verb(args: &[&str]) -> Option<String> {
    let bin = std::env::var("SPIRA_LC_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira-lc".into());
    let timeout = spira_config::process::cfg("SPIRA_LC_TIMEOUT").ok().filter(|s| !s.is_empty())?;
    let o = Command::new("timeout").arg(timeout).arg(bin).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Ids per `list --ids` read: keeps each answer far under the 64 KiB a probe may draw.
pub const IDS_PER_READ: usize = 40;

pub fn id_chunks(ids: &[String]) -> Vec<String> {
    let mut sorted: Vec<&str> = ids.iter().map(String::as_str).filter(|i| !i.is_empty()).collect();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.chunks(IDS_PER_READ).map(|c| c.join(",")).collect()
}

fn digest(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn forget_old_reads(prefix: &str) {
    for dir in [cache_dir(), cache_dir().join("live")] {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|a| a.as_secs() > 600);
            if old && e.file_name().to_string_lossy().starts_with(prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn read_rows(key: &str, args: &[&str]) -> Option<HashMap<String, Row>> {
    let raw = last_good(&cache_dir(), key, io::now(), || shared_live(key, || verb(args).filter(|r| lc_state::parse_rows(r).is_ok())))?;
    lc_state::parse_rows(&raw).ok().map(lc_state::index)
}

/// The lifecycle rows of `ids` only, keyed by bead id: a work bead's state, holder and holds
/// for the probes that used to read bd `status` (design §3.4: "people and the cockpit read state
/// from spira-lc"). A bead with no row is absent. `None` is CANNOT TELL.
pub fn state_index_of(ids: &[String]) -> Option<HashMap<String, Row>> {
    let mut all = HashMap::new();
    for csv in id_chunks(ids) {
        all.extend(read_rows(&format!("ids-{}", digest(&csv)), &["list", "--ids", &csv])?);
    }
    forget_old_reads("ids-");
    Some(all)
}

/// Rows carrying hold `kind` that are not long terminal (`list --hold <kind> --live`).
pub fn held_rows(kind: &str) -> Option<HashMap<String, Row>> {
    read_rows(&format!("hold-{kind}"), &["list", "--hold", kind, "--live"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_read_in_bounded_sorted_deduplicated_chunks() {
        let ids: Vec<String> = (0..IDS_PER_READ * 2 + 1).rev().map(|n| format!("sp-{n:04}")).chain(["sp-0000".to_string(), String::new()]).collect();
        let chunks = id_chunks(&ids);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.split(',').count() <= IDS_PER_READ));
        assert!(chunks[0].starts_with("sp-0000,sp-0001"));
        assert_eq!(chunks.iter().map(|c| c.split(',').count()).sum::<usize>(), IDS_PER_READ * 2 + 1);
        assert!(id_chunks(&[]).is_empty());
    }

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
