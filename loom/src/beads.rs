//! What `bd` is asked, and what comes back from it.
//!
//! ONE QUERY ANSWERS THE WHOLE GRAPH, and it took a wrong turn to find that out. The listing
//! carries each bead's typed dependency edges inline, on the rows that have any — so reading
//! the key set of the FIRST row and concluding the edges are absent is wrong, and the second
//! query written to recover them cost as much again as the listing itself. There is no cheap
//! second source: the batch dependency query runs one lookup per id and takes seconds, and
//! the graph formats drop either the type or the edges to beads that are no longer live.

use serde_json::Value;
use spira_config::lc_state;
use spira_config::nonwork::{self, Kind};
use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Why a refresh produced nothing. Both are served rather than swallowed: a reader who gets
/// a stale page with no explanation cannot tell a slow box from a broken one.
#[derive(Debug)]
pub enum QueryError {
    /// The deadline fired. The child was killed rather than awaited — see `run`.
    OverBudget { name: &'static str, budget_ms: u64 },
    /// `bd` answered, and the answer was not usable.
    Failed { name: &'static str, detail: String },
}

/// Directories prepended to the child's PATH. `bd` lives in a different place on every box,
/// and this process may be started by a service manager whose PATH has neither it nor the
/// database engine behind it.
pub fn child_path(extra: &[String]) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();
    if extra.is_empty() {
        inherited
    } else {
        format!("{}:{}", extra.join(":"), inherited)
    }
}

/// One `bd` invocation under a deadline. Returns its stdout and what it cost.
///
/// THE DEADLINE KILLS, it does not merely stop waiting. `tokio::time::timeout` drops the
/// future it wrapped, and a child is only ended by that drop because `kill_on_drop` is set —
/// without it an overrun leaves a `bd` process still competing for the database that was
/// already too slow, and every subsequent refusal makes the next one likelier.
pub async fn run(
    bin: &str,
    db: &str,
    extra_path: &[String],
    name: &'static str,
    args: &[&str],
    budget: Duration,
) -> Result<(String, u128), QueryError> {
    let started = Instant::now();
    let mut cmd = Command::new(bin);
    cmd.arg("-C")
        .arg(db)
        .args(args)
        .env("PATH", child_path(extra_path))
        // A read endpoint must never be able to answer a prompt. Without this `bd` can
        // inherit a terminal and block on one, which reads from outside as a hung query.
        .env("BD_NON_INTERACTIVE", "1")
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
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(QueryError::Failed {
            name,
            detail: format!(
                "{bin} exited {}: {}",
                out.status.code().unwrap_or(-1),
                err.trim().chars().take(400).collect::<String>()
            ),
        });
    }
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        started.elapsed().as_millis(),
    ))
}

/// Everything from the first line that begins a JSON value.
///
/// `bd --json` can print advisories on STDOUT ahead of the payload — a `beads.role` warning
/// is emitted by an unconfigured checkout, which every fresh fixture is. Handing that
/// straight to a parser fails with a column-1 syntax error that reads like a broken database
/// rather than a preamble.
pub fn json_only(out: &str) -> &str {
    let mut at = 0usize;
    for line in out.split_inclusive('\n') {
        if line.starts_with('[') || line.starts_with('{') {
            return &out[at..];
        }
        at += line.len();
    }
    ""
}

/// Rows that are not over, and how many were dropped.
///
/// "Over" is the lifecycle machine's word for a work bead (design §3.4, sp-mve9i: bd holds
/// content, spira-lc holds state): a row whose lifecycle row is terminal is dropped,
/// whatever bd's status says. A row with no lifecycle row is a coordination or other
/// non-work bead, whose bd status is its only state (spira_config::nonwork). With no answer
/// from the machine (`lc` is `None`) nothing is dropped: a state that cannot be read is not
/// read as over.
///
/// The query is already bounded to work in flight — that bound is the entire reason a
/// per-request read over `bd` is affordable, since the closed corpus is several times the
/// live one. This is the belt to that brace, and the COUNT is the point of it: if the bound
/// ever stops holding, the number says so out loud in every response rather than the payload
/// quietly growing by a factor of six (a check that finds nothing must be able to find
/// something).
pub fn drop_closed(rows: Vec<Value>, lc: Option<&HashMap<String, lc_state::Row>>) -> (Vec<Value>, usize) {
    let before = rows.len();
    let Some(lc) = lc else { return (rows, 0) };
    let kept: Vec<Value> = rows
        .into_iter()
        .filter(|r| match lc.get(r.get("id").and_then(Value::as_str).unwrap_or("")) {
            Some(row) => !row.terminal(),
            None => !nonwork::row_closed(Kind::Epic, r),
        })
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

/// Every row's `dependencies`, lifted into one list and removed from the rows.
///
/// HOISTED RATHER THAN LEFT IN PLACE, so the payload has exactly one edge list. Left where it
/// was, the page would have a second one alongside the served list — unfiltered, still naming
/// beads that are not in the payload — and the two would disagree in precisely the case that
/// matters, an edge whose other end has been closed.
///
/// The edge objects themselves are passed through as `bd` writes them: `issue_id`,
/// `depends_on_id` and `type`, plus when and by whom. Renaming them would put the page's model
/// and the command line into different vocabularies for the same thing.
pub fn take_edges(rows: &mut [Value]) -> Vec<Value> {
    let mut edges = Vec::new();
    for row in rows.iter_mut() {
        let Some(obj) = row.as_object_mut() else { continue };
        let Some(Value::Array(deps)) = obj.remove("dependencies") else {
            continue;
        };
        edges.extend(deps);
    }
    edges
}

/// The edges both of whose ends are in the payload, and how many were dropped.
///
/// An edge to a bead that is not being served cannot be drawn, so it is dropped — and
/// counted, because a graph quietly missing a third of its edges looks like a graph. On a
/// live database this number is not small: the closed corpus is most of the database, and
/// every edge into it lands here.
pub fn drawable_edges(edges: Vec<Value>, live: &HashSet<&str>) -> (Vec<Value>, usize) {
    let before = edges.len();
    let kept: Vec<Value> = edges
        .into_iter()
        .filter(|e| {
            let has = |k: &str| {
                e.get(k)
                    .and_then(Value::as_str)
                    .map(|id| live.contains(id))
                    .unwrap_or(false)
            };
            has("issue_id") && has("depends_on_id")
        })
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_only_skips_an_advisory_printed_before_the_payload() {
        let out = "warning: beads.role not configured.\n  Fix: git config …\n[{\"id\":\"sp-a\"}]\n";
        assert_eq!(json_only(out).trim(), "[{\"id\":\"sp-a\"}]");
        // The positive control's other half: a payload with no preamble is returned whole.
        assert_eq!(json_only("[1,2]").trim(), "[1,2]");
        // And nothing that is not JSON at all yields nothing, rather than a truncated value.
        assert_eq!(json_only("bd: command not found\n"), "");
    }

    #[test]
    fn a_closed_row_is_dropped_and_counted() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"sp-a","status":"open"},
                {"id":"sp-b","status":"closed"},
                {"id":"sp-c","status":"in_progress"}]"#,
        )
        .unwrap();
        let lc = HashMap::new();
        let (kept, dropped) = drop_closed(rows, Some(&lc));
        // The filter is SEEN removing something before its silence is believed anywhere else.
        assert_eq!(dropped, 1);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|r| r["status"].as_str() != Some("closed")));
    }

    /// sp-mve9i: a work bead's row is dropped when its lifecycle row is over, not when bd
    /// says closed; with no machine answer nothing is dropped.
    #[test]
    fn a_work_row_is_dropped_by_its_lifecycle_state() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"sp-a","status":"open"},
                {"id":"sp-b","status":"closed"},
                {"id":"sp-c","status":"open"}]"#,
        )
        .unwrap();
        let lc: HashMap<String, lc_state::Row> = [("sp-a", "LANDED"), ("sp-b", "REWORK")]
            .iter()
            .map(|(id, st)| (id.to_string(), lc_state::Row { bead_id: id.to_string(), state: st.to_string(), ..Default::default() }))
            .collect();
        let (kept, dropped) = drop_closed(rows.clone(), Some(&lc));
        let ids: Vec<&str> = kept.iter().filter_map(|r| r["id"].as_str()).collect();
        assert_eq!((ids, dropped), (vec!["sp-b", "sp-c"], 1));
        assert_eq!(drop_closed(rows, None).1, 0);
    }

    #[test]
    fn edges_are_hoisted_off_the_rows_they_arrived_on() {
        let mut rows: Vec<Value> = serde_json::from_str(
            r#"[{"id":"sp-a","dependencies":[
                    {"issue_id":"sp-a","depends_on_id":"sp-b","type":"blocks"},
                    {"issue_id":"sp-a","depends_on_id":"sp-z","type":"parent-child"}]},
                {"id":"sp-b"}]"#,
        )
        .unwrap();
        let edges = take_edges(&mut rows);
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0]["type"], "blocks");
        // The whole point of hoisting: no second, unfiltered copy is left behind for the page
        // to reach for by accident.
        assert!(rows[0].get("dependencies").is_none());
        // A row that never had any is untouched rather than given an empty array.
        assert!(rows[1].get("dependencies").is_none());
    }

    #[test]
    fn an_edge_whose_other_end_is_not_served_is_dropped_and_counted() {
        let edges: Vec<Value> = serde_json::from_str(
            r#"[{"issue_id":"sp-a","depends_on_id":"sp-b","type":"blocks"},
                {"issue_id":"sp-a","depends_on_id":"sp-gone","type":"blocks"},
                {"issue_id":"sp-nowhere","depends_on_id":"sp-b","type":"blocks"}]"#,
        )
        .unwrap();
        let live: HashSet<&str> = ["sp-a", "sp-b"].into_iter().collect();
        let (kept, dropped) = drawable_edges(edges, &live);
        // SEEN dropping two before its silence on the third means anything.
        assert_eq!(dropped, 2);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["depends_on_id"], "sp-b");
    }
}
