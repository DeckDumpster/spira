//! `churn-facts --since <epoch>`: the rows `queue churn` measures, read-only. Members a round
//! ejected, when each landed and at which tip, and every mender failure with its stamps.

use serde_json::{json, Value};

use crate::db::Conn;
use crate::flag;
use crate::rows::escape;

const EJECTS: &str = "SELECT e.lc_key AS batch_id, e.evidence AS evidence, e.at AS at FROM event e \
     WHERE e.machine = 'batch' AND e.event = 'Eject' AND e.applied = 1 AND e.at >= ";

pub fn cmd_churn_facts(args: &[String], conn: &Conn) -> (i32, String) {
    let since = match since_of(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    match facts(conn, since) {
        Ok(v) => (0, v.to_string()),
        Err(e) => (2, format!("cannot tell: {e:?}")),
    }
}

pub(crate) fn since_of(args: &[String]) -> Result<i64, (i32, String)> {
    flag(args, "--since").and_then(|s| s.parse::<i64>().ok()).ok_or((2, "churn-facts: --since <epoch seconds> is required".into()))
}

fn epoch(row: &Value, col: &str) -> Option<i64> {
    match row.get(col)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// Stamps each failure with `outcome_at`: the latest applied event that left WAITING/MENDING,
/// or null while it runs. `events` rows carry `batch_id`, `pass`, `suite`, `at`.
pub(crate) fn fold_outcomes(mut mendings: Vec<Value>, events: &[Value]) -> Vec<Value> {
    for m in &mut mendings {
        let key = |r: &Value| ["batch_id", "pass", "suite"].map(|c| r.get(c).map(|v| v.to_string()));
        let at = events.iter().filter(|e| key(e) == key(m)).filter_map(|e| Some((epoch(e, "at")?, e.get("at")?))).max_by_key(|(n, _)| *n);
        m["outcome_at"] = at.map_or(Value::Null, |(_, v)| v.clone());
    }
    mendings
}

fn facts(conn: &Conn, since: i64) -> Result<Value, crate::db::DbError> {
    let ejects = conn.query(&format!("{EJECTS}{since}"))?;
    let batches: Vec<String> = ejects.iter().filter_map(|r| r.get("batch_id").and_then(Value::as_str)).map(escape).collect();
    let (members, landed_at, landed_tip) = if batches.is_empty() {
        (vec![], vec![], vec![])
    } else {
        let scope = format!("'{}'", batches.join("','"));
        let members = conn.query(&format!("SELECT batch_id, bead_id, tip FROM batch_member WHERE batch_id IN ({scope})"))?;
        let beads = format!("SELECT bead_id FROM batch_member WHERE batch_id IN ({scope})");
        let at = conn.query(&format!("SELECT lc_key AS bead, at FROM event WHERE machine = 'bead' AND to_state = 'LANDED' AND from_state <> 'LANDED' AND applied = 1 AND lc_key IN ({beads})"))?;
        let tip = conn.query(&format!("SELECT bead_id, tip FROM bead WHERE state = 'LANDED' AND bead_id IN ({beads})"))?;
        (members, at, tip)
    };
    let mendings = conn.query(&format!(
        "SELECT m.batch_id AS batch_id, m.pass AS pass, m.suite AS suite, m.state AS state, m.failed_at AS failed_at, m.picked_at AS picked_at, \
         m.triage_ended_at AS triage_ended_at FROM mending m WHERE m.failed_at >= {since}"
    ))?;
    let outcomes = conn.query(&format!(
        "SELECT m.batch_id AS batch_id, m.pass AS pass, m.suite AS suite, e.at AS at FROM mending m JOIN event e ON e.machine = 'mending' \
         AND e.lc_key = CONCAT(m.batch_id, '#', m.pass, '#', m.suite) AND e.applied = 1 AND e.to_state NOT IN ('WAITING', 'MENDING') WHERE m.failed_at >= {since}"
    ))?;
    let mendings = fold_outcomes(mendings, &outcomes);
    let has_mend = !conn.query("SELECT 1 FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'mend'")?.is_empty();
    let mends = if has_mend { conn.query(&format!("SELECT batch_id, pass, suite, bead_id, from_tip, tip, at FROM mend WHERE at >= {since}"))? } else { vec![] };
    let pass_ends = conn.query(&format!(
        "SELECT lc_key AS batch_id, evidence, at FROM event WHERE machine = 'batch' AND event IN ('PassRed', 'PassPreempted') AND applied = 1 AND at >= {since}"
    ))?;
    Ok(json!({ "ejects": ejects, "members": members, "landed_at": landed_at, "landed_tip": landed_tip, "mendings": mendings, "mends": mends, "pass_ends": pass_ends }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(suite: &str, state: &str) -> Value {
        json!({"batch_id": "batch-mend", "pass": "1", "suite": suite, "state": state})
    }

    fn outcome(suite: &str, at: &str) -> Value {
        json!({"batch_id": "batch-mend", "pass": "1", "suite": suite, "at": at})
    }

    #[test]
    fn an_expired_failure_gets_the_time_of_its_latest_outcome_event() {
        let out = fold_outcomes(vec![failure("test-z.sh", "RETURNED")], &[outcome("test-z.sh", "90"), outcome("test-z.sh", "100"), outcome("test-other.sh", "500")]);
        assert_eq!((out[0]["state"].as_str(), out[0]["outcome_at"].as_str()), (Some("RETURNED"), Some("100")));
    }

    #[test]
    fn a_running_failure_gets_no_outcome() {
        let out = fold_outcomes(vec![failure("test-f.sh", "MENDING")], &[outcome("test-z.sh", "100")]);
        assert_eq!(out[0]["outcome_at"], Value::Null);
    }

    #[test]
    fn a_missing_since_is_refused_with_code_2_and_a_malformed_one_too() {
        assert_eq!(since_of(&[]).unwrap_err().0, 2);
        assert_eq!(since_of(&["--since".into(), "yesterday".into()]).unwrap_err().0, 2);
        assert_eq!(since_of(&["--since".into(), "0".into()]), Ok(0));
    }
}
