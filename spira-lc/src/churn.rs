//! `churn-facts --since <epoch>`: the rows `queue churn` measures, read-only. Members a round
//! ejected, when each landed and at which tip, and every mender failure with its stamps.

use serde_json::{json, Value};
use state_store::Backend;

use crate::control::audits::Logged;
use crate::control::store::Store;
use crate::flag;

pub fn cmd_churn_facts(args: &[String]) -> (i32, String) {
    let since = match since_of(args) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let store = match crate::control::served::store() {
        Ok(s) => s,
        Err(e) => return (2, format!("cannot tell: {e}")),
    };
    match facts(&store, since) {
        Ok(v) => (0, v.to_string()),
        Err(e) => (2, format!("cannot tell: {e}")),
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

fn text(v: impl ToString) -> Value {
    Value::String(v.to_string())
}

fn opt(v: Option<impl ToString>) -> Value {
    v.map_or(Value::Null, text)
}

fn facts<B: Backend>(store: &Store<B>, since: i64) -> Result<Value, String> {
    let e = |e: state_store::Error| e.to_string();
    let scan = store.audits_since(since).map_err(e)?;
    if !scan.complete {
        return Err("the audit log is too long to read back to --since in one call".into());
    }
    let applied = |l: &&Logged, machine: &str| l.audit.applied && l.audit.machine == machine;
    let ejects: Vec<&Logged> = scan.audits.iter().filter(|l| applied(l, "batch") && l.audit.event == "Eject").collect();
    let mut members = Vec::new();
    let mut beads = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    for l in &ejects {
        if !seen.insert(l.audit.lc_key.as_str()) {
            continue;
        }
        if let Some((_, round)) = store.batch(&l.audit.lc_key).map_err(e)? {
            for m in &round.value.members {
                beads.insert(m.bead_id.clone());
                members.push(json!({ "batch_id": l.audit.lc_key, "bead_id": m.bead_id, "tip": m.tip }));
            }
        }
    }
    let landed_at: Vec<Value> = scan
        .audits
        .iter()
        .filter(|l| applied(l, "bead") && l.audit.to_state == "LANDED" && l.audit.from_state != "LANDED" && beads.contains(&l.audit.lc_key))
        .map(|l| json!({ "bead": l.audit.lc_key, "at": text(l.audit.at) }))
        .collect();
    let mut landed_tip = Vec::new();
    for bead in &beads {
        if let Some(r) = store.record(bead).map_err(e)?.filter(|r| r.value.row.state == lifecycle::bead::BeadState::Landed) {
            landed_tip.push(json!({ "bead_id": bead, "tip": opt(r.value.row.tip.as_ref()) }));
        }
    }
    let failures: Vec<_> = store.mendings().map_err(e)?.into_iter().filter(|m| m.failed_at >= since).collect();
    let mendings: Vec<Value> = failures
        .iter()
        .map(|m| {
            let r = &m.row;
            json!({ "batch_id": r.batch_id, "pass": text(r.pass), "suite": r.suite, "state": r.state.as_str(), "failed_at": text(m.failed_at), "picked_at": opt(r.picked_at), "triage_ended_at": opt(r.triage_ended_at) })
        })
        .collect();
    let outcomes: Vec<Value> = failures
        .iter()
        .flat_map(|m| {
            let key = m.row.key();
            scan.audits.iter().filter(move |l| applied(l, "mending") && l.audit.lc_key == key && !["WAITING", "MENDING"].contains(&l.audit.to_state.as_str())).map(|l| json!({ "batch_id": m.row.batch_id, "pass": text(m.row.pass), "suite": m.row.suite, "at": text(l.audit.at) }))
        })
        .collect();
    let mends: Vec<Value> = store
        .mends_since(since)
        .map_err(e)?
        .iter()
        .map(|m| json!({ "batch_id": m.batch_id, "pass": text(m.pass), "suite": m.suite, "bead_id": m.bead_id, "from_tip": m.from_tip, "tip": m.tip, "at": text(m.at) }))
        .collect();
    let rows = |event: &Logged| json!({ "batch_id": event.audit.lc_key, "evidence": text(&event.audit.evidence), "at": text(event.audit.at) });
    let pass_ends: Vec<Value> = scan.audits.iter().filter(|l| applied(l, "batch") && ["PassRed", "PassPreempted"].contains(&l.audit.event.as_str())).map(rows).collect();
    let ejects: Vec<Value> = ejects.into_iter().map(|l| rows(l)).collect();
    Ok(json!({ "ejects": ejects, "members": members, "landed_at": landed_at, "landed_tip": landed_tip, "mendings": fold_outcomes(mendings, &outcomes), "mends": mends, "pass_ends": pass_ends }))
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
