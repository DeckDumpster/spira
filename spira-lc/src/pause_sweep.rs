//! `pause-sweep`: lifts every scoped stop whose `bead:ID=STATE` release condition has been met,
//! judged on the bead's applied lifecycle events since the stop was set, so a bead that entered
//! the state and moved on between two sweeps still releases it.

use crate::CANNOT_TELL;
use spira_config::scoped_stop;

pub fn run() -> (i32, String) {
    let run = match crate::lifecycle_run_dir() {
        Ok(d) => d,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let store = match crate::control::served::store() {
        Ok(s) => s,
        Err(e) => return (CANNOT_TELL, format!("cannot tell: {e}")),
    };
    let mut reached = |bead: &str, state: &str, since: u64| {
        let history = store.history(bead, Some("bead")).map_err(|e| format!("cannot tell: {e}"))?;
        let rows: Vec<serde_json::Value> = history
            .iter()
            .filter(|c| c.audit.applied && c.audit.to_state == state && c.audit.at >= since as i64)
            .map(|c| serde_json::json!({ "to_state": c.audit.to_state, "applied": "1", "at": c.audit.at.to_string() }))
            .collect();
        Ok(scoped_stop::entered_state(&rows, state, since))
    };
    let (lifted, unread) = scoped_stop::sweep(&run, &mut reached);
    let mut lines: Vec<String> = lifted.iter().map(|r| format!("pause-sweep: lifted the stop on {r}: its bead condition was met")).collect();
    lines.extend(unread.iter().map(|u| format!("pause-sweep: kept {u}")));
    (if unread.is_empty() { 0 } else { CANNOT_TELL }, lines.join("\n"))
}
