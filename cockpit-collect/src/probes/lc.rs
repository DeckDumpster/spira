//! Bulk reads of the lifecycle store (`spira-lc list`). `None` is CANNOT TELL — the program
//! is missing, timed out or answered unparseably — kept apart from `Some(empty)`, a real zero.

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

/// Rows in `state`, or every row when `state` is `None`.
pub fn list(state: Option<&str>) -> Option<Vec<LcRow>> {
    let bin = std::env::var("SPIRA_LC_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira-lc".into());
    let timeout = std::env::var("SPIRA_LC_TIMEOUT").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "30".into());
    let mut cmd = Command::new("timeout");
    cmd.arg(timeout).arg(bin).arg("list");
    if let Some(s) = state {
        cmd.args(["--state", s]);
    }
    let o = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    parse_rows(&String::from_utf8_lossy(&o.stdout))
}

/// Every lifecycle row as `spira_config::lc_state` reads it, keyed by bead id: a work bead's
/// state, holder and holds for the probes that used to read bd `status` (sp-mve9i, design
/// §3.4: "people and the cockpit read state from spira-lc"). `None` is CANNOT TELL.
pub fn state_index() -> Option<HashMap<String, Row>> {
    lc_state::list().ok().map(lc_state::index)
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
}
