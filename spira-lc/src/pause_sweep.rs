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
    let mut reached = |bead: &str, state: &str, since: u64| entered(&*store, bead, state, since);
    let (lifted, unread) = scoped_stop::sweep(&run, &mut reached);
    let mut lines: Vec<String> = lifted.iter().map(|r| format!("pause-sweep: lifted the stop on {r}: its bead condition was met")).collect();
    lines.extend(unread.iter().map(|u| format!("pause-sweep: kept {u}")));
    (if unread.is_empty() { 0 } else { CANNOT_TELL }, lines.join("\n"))
}

/// Whether the bead's applied events show it entering `state` at or after `since`.
fn entered<B: state_store::Backend>(store: &crate::control::store::Store<B>, bead: &str, state: &str, since: u64) -> Result<bool, String> {
    let history = store.history(bead, Some("bead")).map_err(|e| format!("cannot tell: {e}"))?;
    let rows: Vec<serde_json::Value> = history
        .iter()
        .filter(|c| c.audit.applied && c.audit.to_state == state && c.audit.at >= since as i64)
        .map(|c| serde_json::json!({ "to_state": c.audit.to_state, "applied": "1", "at": c.audit.at.to_string() }))
        .collect();
    Ok(scoped_stop::entered_state(&rows, state, since))
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use state_store::{Config, Contract, MemBackend, Ports, Request};

    fn store() -> crate::control::store::Store<MemBackend> {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(0)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        crate::control::store::Store::new(c, 1_000)
    }

    #[test]
    fn a_bead_that_entered_the_state_after_the_stop_was_set_releases_it_and_one_that_did_not_does_not() {
        let s = store();
        s.create("sp-a", None, Some("0123456789abcdef0123456789abcdef01234567"), 100, &Request::new("c1", "t", "t")).unwrap();
        assert_eq!(entered(&s, "sp-a", "SUBMITTED", 50), Ok(true));
        assert_eq!(entered(&s, "sp-a", "SUBMITTED", 100), Ok(true), "at the second the stop was set counts");
        assert_eq!(entered(&s, "sp-a", "SUBMITTED", 101), Ok(false), "an entry before the stop does not");
        assert_eq!(entered(&s, "sp-a", "LANDED", 0), Ok(false));
        assert_eq!(entered(&s, "sp-none", "SUBMITTED", 0), Ok(false));
    }
}
