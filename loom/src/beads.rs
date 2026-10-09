//! What the lifecycle store is asked, and what comes back from it.
//!
//! The page reads `spira-lc ops-view`, the read model the lifecycle store keeps for the
//! panes: the non-terminal rows plus the last day's landings, a few indexed lookups. It never
//! reads `bd`, whose full listing is a scan of the whole corpus under whatever load the box is
//! carrying. The store holds no dependency edges, so the page's graph has none.

use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Why a refresh produced nothing. Both are served rather than swallowed: a reader who gets
/// a stale page with no explanation cannot tell a slow box from a broken one.
#[derive(Debug)]
pub enum QueryError {
    /// The deadline fired. The child was killed rather than awaited — see `run`.
    OverBudget { name: &'static str, budget_ms: u64 },
    /// The store answered, and the answer was not usable.
    Failed { name: &'static str, detail: String },
}

/// Directories prepended to the child's PATH: this process may be started by a service
/// manager whose PATH has neither the lifecycle binary nor the tools behind it.
pub fn child_path(extra: &[String]) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();
    if extra.is_empty() {
        inherited
    } else {
        format!("{}:{}", extra.join(":"), inherited)
    }
}

/// One `spira-lc ops-view <view>` under a deadline. Returns its rows and what it cost.
///
/// THE DEADLINE KILLS, it does not merely stop waiting: `timeout` drops the future, and a
/// child is only ended by that drop because `kill_on_drop` is set. Otherwise an overrun leaves
/// a process still competing for the store that was already too slow.
pub async fn ops_view(
    bin: &str,
    extra_path: &[String],
    name: &'static str,
    view: &str,
    budget: Duration,
) -> Result<(Vec<Value>, u128), QueryError> {
    let started = Instant::now();
    let mut cmd = Command::new(bin);
    cmd.args(["ops-view", view])
        .env("PATH", child_path(extra_path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let out = match tokio::time::timeout(budget, cmd.output()).await {
        Err(_) => {
            return Err(QueryError::OverBudget {
                name,
                budget_ms: budget.as_millis() as u64,
            })
        }
        Ok(Err(e)) => {
            return Err(QueryError::Failed {
                name,
                detail: format!("could not run {bin}: {e}"),
            })
        }
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        return Err(QueryError::Failed {
            name,
            detail: format!(
                "{bin} ops-view {view} exited {}: {}",
                out.status.code().unwrap_or(-1),
                err.trim().chars().take(400).collect::<String>()
            ),
        });
    }
    let rows = parse_rows(&String::from_utf8_lossy(&out.stdout)).map_err(|detail| QueryError::Failed { name, detail })?;
    Ok((rows, started.elapsed().as_millis()))
}

pub fn parse_rows(text: &str) -> Result<Vec<Value>, String> {
    match serde_json::from_str::<Value>(text.trim()) {
        Ok(Value::Array(a)) => Ok(a),
        Ok(_) => Err("ops-view: not a JSON array".to_string()),
        Err(e) => Err(format!("unparseable payload: {e}")),
    }
}

fn num(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The hold kinds the page lists, in its order.
pub const HOLD_KINDS: [&str; 4] = ["wait", "manual", "ask", "poison"];

fn holds(v: Option<&Value>) -> Vec<String> {
    let arr = match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let held: Vec<String> = arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
    HOLD_KINDS.iter().filter(|k| held.iter().any(|h| h == *k)).map(|k| k.to_string()).collect()
}

/// Epoch seconds as the UTC timestamp the page parses.
pub fn iso(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// A view row in the shape the page reads: the lifecycle state stands for bd's status, and
/// the holder for its assignee. A row filed before titles were mirrored shows its id.
pub fn page_row(r: &Value, landed: bool) -> Option<Value> {
    let id = text(r.get("bead_id"))?;
    let state = text(r.get("state")).unwrap_or_default();
    let status = if landed {
        "closed"
    } else if state == "WORKING" || state == "IN_DELIVERY" {
        "in_progress"
    } else {
        "open"
    };
    let mut row = json!({
        "id": id,
        "title": text(r.get("title")).unwrap_or_else(|| id.clone()),
        "status": status,
        "issue_type": "task",
        "priority": num(r.get("priority")).unwrap_or(3),
        "state": state,
        "holds": holds(r.get("holds")),
    });
    let o = row.as_object_mut()?;
    if let Some(h) = text(r.get("holder")) {
        o.insert("assignee".into(), Value::from(h));
    }
    if let Some(t) = num(r.get("updated_at")) {
        o.insert("updated_at".into(), Value::from(iso(t)));
    }
    if let Some(t) = num(r.get("since")) {
        o.insert("closed_at".into(), Value::from(iso(t)));
    }
    Some(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_seconds_become_the_timestamp_the_page_parses() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_791_480_577), "2026-10-08T17:29:37Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn a_live_row_is_shaped_for_the_page() {
        let r: Value = serde_json::from_str(
            r#"{"bead_id":"sp-a","state":"WORKING","holds":"[\"manual\",\"wait\",\"other\"]","holder":"aeon-1","priority":"1","title":"t","updated_at":"100"}"#,
        )
        .unwrap();
        let p = page_row(&r, false).unwrap();
        assert_eq!(p["status"], "in_progress");
        assert_eq!(p["priority"], 1);
        assert_eq!(p["assignee"], "aeon-1");
        assert_eq!(p["holds"], json!(["wait", "manual"]));
        assert_eq!(p["updated_at"], "1970-01-01T00:01:40Z");
        let bare = page_row(&json!({"bead_id":"sp-b","state":"READY","holds":null}), false).unwrap();
        assert_eq!((bare["title"].as_str(), bare["status"].as_str(), bare["priority"].as_i64()), (Some("sp-b"), Some("open"), Some(3)));
        assert!(bare.get("assignee").is_none());
        assert_eq!(page_row(&json!({"bead_id":"sp-c","state":"LANDED"}), true).unwrap()["status"], "closed");
        assert!(page_row(&json!({"state":"READY"}), false).is_none());
    }

    #[test]
    fn only_a_json_array_is_rows() {
        assert_eq!(parse_rows("[{\"bead_id\":\"x\"}]\n").unwrap().len(), 1);
        assert!(parse_rows("{}").is_err());
        assert!(parse_rows("cannot tell").is_err());
    }
}
