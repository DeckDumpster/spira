//! The ops read model (`lifecycle/migrations/0007-ops-read-model.sql`, `0009-where-stuck.sql`):
//! `ops-view <view>` returns one view as a JSON array, `ops-graph` the machine's legal edges, and `backfill-titles` fills the mirrored
//! title and priority of rows filed before the mirror existed. Both ride `dispatch`, so a
//! refresh through `spira-lc serve` is one query on the held connection.

use serde_json::Value;

use crate::db::Conn;

/// The only views `ops-view` will name; the argument is matched, never interpolated.
pub const VIEWS: [&str; 6] = ["ops_live", "ops_round", "ops_recent", "ops_edges", "ops_dwell", "ops_dwell_p95"];

/// Every applied state change of the last day, read from the event time index.
pub const GANTT_SQL: &str = "SELECT seq, lc_key AS bead_id, from_state, to_state, at FROM event \
     WHERE machine = 'bead' AND applied = 1 AND at >= UNIX_TIMESTAMP() - 86400 AND from_state <> to_state";

const LIVE_STATES: &str = "'OPEN', 'READY', 'WORKING', 'SUBMITTED', 'CERTIFIED', 'IN_DELIVERY', 'REWORK'";

pub fn cmd_ops_view(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(view) = args.first().and_then(|a| VIEWS.iter().find(|v| **v == a.as_str())) else {
        return (2, format!("ops-view: takes one of {}", VIEWS.join(" | ")));
    };
    if let Some(rows) = conn.live().and_then(|l| l.ops_view(view, crate::db::now_epoch())) {
        return (0, Value::Array(rows).to_string());
    }
    match conn.query(&format!("SELECT * FROM {view}")) {
        Ok(rows) => (0, Value::Array(rows).to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

/// `live-check`: compare the live rows with Dolt now. A mismatch reloads memory and raises an
/// incident naming the divergent beads (exit 1); a match is exit 0.
pub fn cmd_live_check(conn: &Conn) -> (i32, String) {
    match run_live_check(conn, &raise_incident) {
        Ok(check) => (i32::from(!check.divergent.is_empty()), serde_json::json!({ "consistent": check.divergent.is_empty(), "rows": check.rows, "memory_hash": check.memory_hash, "dolt_hash": check.dolt_hash, "divergent": check.divergent }).to_string()),
        Err(e) => (2, format!("live-check: {e}")),
    }
}

pub fn run_live_check(conn: &Conn, raise: &dyn Fn(&[String])) -> Result<crate::live::Check, String> {
    let check = conn.live_check()?;
    if !check.divergent.is_empty() {
        raise(&check.divergent);
    }
    Ok(check)
}

pub fn raise_incident(divergent: &[String]) {
    let title = format!("lc-serve live rows diverged from Dolt: {}", divergent.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
    let body = format!("The consistency check found these beads in memory differing from the lifecycle store, and reloaded them from Dolt:\n{}\n\nA row changed without passing through lc-serve's write path, or a write's refresh was lost.\n", divergent.join("\n"));
    let env = [("SPIRA_INCIDENT_TYPE", "bug"), ("SPIRA_INCIDENT_PRIORITY", "1"), ("SPIRA_INCIDENT_ACTOR", "lc-serve"), ("SPIRA_INCIDENT_REPO", "spira"), ("SPIRA_INCIDENT_REF", "lc-live-divergence"), ("SPIRA_INCIDENT_CAUSE", "lc-live-divergence")];
    let (code, out) = crate::bd::tool_env("incident.sh", &["file".to_string(), title, "-".to_string()], Some(&body), "lc-serve", 60, &env);
    if code != 0 {
        eprintln!("spira-lc: live-check could not raise its incident ({code}): {out}");
    }
}

/// The stuck page's bars: the day's transitions plus the clock they are measured against.
pub fn cmd_ops_gantt(args: &[String], conn: &Conn) -> (i32, String) {
    if args.first().map(String::as_str) == Some("--print-sql") {
        return (0, GANTT_SQL.to_string());
    }
    match conn.query(GANTT_SQL) {
        Ok(rows) => (0, serde_json::json!({ "now": crate::db::now_epoch(), "events": rows }).to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

/// Refused bead events inside the last `window_secs`, one row per (actor, event, from_state,
/// refusal) with the count, the span and the newest five `bead@epoch` examples.
pub fn refusals_sql(window_secs: u64) -> String {
    format!(
        "SELECT actor, event, from_state, refusal, COUNT(*) AS n, MIN(at) AS first_at, MAX(at) AS last_at, \
         SUBSTRING_INDEX(GROUP_CONCAT(CONCAT(lc_key, '@', at) ORDER BY at DESC SEPARATOR ','), ',', 5) AS examples \
         FROM event WHERE machine = 'bead' AND applied = 0 AND at >= UNIX_TIMESTAMP() - {window_secs} \
         GROUP BY actor, event, from_state, refusal"
    )
}

/// `ops-refusals <window-secs>`: the refused bead events of the window, grouped by who asked.
pub fn cmd_ops_refusals(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(window) = args.first().and_then(|a| a.parse::<u64>().ok()).filter(|w| *w > 0) else {
        return (2, "ops-refusals: takes one positive <window-secs>".into());
    };
    match conn.query(&refusals_sql(window)) {
        Ok(rows) => (0, serde_json::json!({ "now": crate::db::now_epoch(), "classes": rows }).to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

/// One bead's whole timeline, refused events included, with the row that says who holds it.
pub fn cmd_ops_bead(args: &[String], conn: &Conn) -> (i32, String) {
    let Some(id) = args.first() else {
        return (2, "ops-bead: missing <bead-id>".into());
    };
    let key = crate::rows::escape(id);
    let bead = conn.query(&format!("SELECT bead_id, state, holder, persona, lease_until, holds, reason, gate_key, title, priority, updated_at FROM bead WHERE bead_id = '{key}'"));
    let events = conn.query(&format!("SELECT seq, event, from_state, to_state, applied, refusal, evidence, actor, at FROM event WHERE machine = 'bead' AND lc_key = '{key}' ORDER BY seq"));
    match (bead, events) {
        (Ok(b), Ok(e)) if b.is_empty() && e.is_empty() => (1, "{}".into()),
        (Ok(b), Ok(e)) => (0, serde_json::json!({ "now": crate::db::now_epoch(), "bead": b.first(), "events": e }).to_string()),
        (Err(e), _) | (_, Err(e)) => (2, format!("cannot tell: {e:?}")),
    }
}

/// The bead machine's legal moves, read from its own transition table.
pub fn cmd_ops_graph(_args: &[String], _conn: &Conn) -> (i32, String) {
    (0, graph_json())
}

/// States spelled as the `bead.state` column holds them, so a pane joins edges to view rows.
fn graph_json() -> String {
    let edges: Vec<Value> = lifecycle::bead::legal_edges().iter().map(|e| serde_json::json!({"from": e.from.as_str(), "event": e.event, "to": e.to.as_str()})).collect();
    Value::Array(edges).to_string()
}

pub fn title_priority(bd_show_json: &str) -> Option<(String, i64)> {
    let parsed: Value = serde_json::from_str(bd_show_json.trim()).ok()?;
    let doc = match &parsed {
        Value::Array(a) => a.first()?.clone(),
        v => v.clone(),
    };
    Some((doc.get("title")?.as_str()?.to_string(), doc.get("priority")?.as_i64()?))
}

/// Rows the views show that have no title yet: every live row, and the landings of the last day.
fn untitled_sql() -> String {
    format!(
        "SELECT bead_id FROM bead WHERE title IS NULL AND (state IN ({LIVE_STATES}) OR (state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400))"
    )
}

pub fn cmd_backfill_titles(_args: &[String], conn: &Conn) -> (i32, String) {
    let ids: Vec<String> = match conn.query(&untitled_sql()) {
        Ok(rows) => rows.iter().filter_map(|r| r.get("bead_id").and_then(|v| v.as_str()).map(str::to_string)).collect(),
        Err(e) => return (2, format!("cannot tell: {e:?}")),
    };
    let (mut filled, mut missed) = (0, Vec::new());
    for id in &ids {
        let known = crate::bd::run(&["show", id, "--json"]).ok().and_then(|o| title_priority(&o));
        let Some((title, priority)) = known else {
            missed.push(id.clone());
            continue;
        };
        let args = [id.clone(), "--title".into(), title, "--priority".into(), priority.to_string()];
        match crate::cutover::cmd_create_bead(&args, conn) {
            (0, _) => filled += 1,
            (_, why) => missed.push(format!("{id} ({why})")),
        }
    }
    let code = if missed.is_empty() { 0 } else { 1 };
    (code, format!("backfill-titles: {filled} of {} filled{}", ids.len(), if missed.is_empty() { String::new() } else { format!("; not filled: {}", missed.join(", ")) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bd_show_json_gives_title_and_priority_whether_array_or_object() {
        assert_eq!(title_priority(r#"[{"title":"t","priority":1}]"#), Some(("t".into(), 1)));
        assert_eq!(title_priority(r#"{"title":"t","priority":0}"#), Some(("t".into(), 0)));
        assert_eq!(title_priority(r#"{"title":"t"}"#), None);
        assert_eq!(title_priority("not json"), None);
    }

    #[test]
    fn the_refusal_query_reads_only_refused_bead_events_of_the_window() {
        let q = refusals_sql(86400);
        assert!(q.contains("machine = 'bead' AND applied = 0") && q.contains("UNIX_TIMESTAMP() - 86400"), "{q}");
        assert!(q.contains("GROUP BY actor, event, from_state, refusal"), "{q}");
    }

    #[test]
    fn the_graph_is_json_edges_with_state_names_and_events() {
        let out = graph_json();
        let edges: Vec<Value> = serde_json::from_str(&out).unwrap();
        assert!(edges.iter().any(|e| e["from"] == "READY" && e["event"] == "Claim" && e["to"] == "WORKING"), "{out}");
    }

    #[test]
    fn the_backfill_reaches_exactly_the_rows_the_views_show() {
        let sql = untitled_sql();
        let migration = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../lifecycle/migrations/0007-ops-read-model.sql")).unwrap();
        let one_line = migration.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(one_line.contains(&format!("state IN ({LIVE_STATES})")), "ops_live and the backfill name the same states");
        assert!(sql.contains("state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400") && migration.contains("state = 'LANDED' AND since >= UNIX_TIMESTAMP() - 86400"));
    }
}
