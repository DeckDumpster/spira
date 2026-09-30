//! Which live sessions the sweep would archive, and in what order. Pure: no clock, no
//! file, no subprocess — `sweep` (in `run.rs`) measures every live transcript through
//! `ctx-meter.sh` and the covered cursor, then hands the numbers here.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub sid: String,
    pub tp: String,
    pub ctx: String,
    pub turns: u64,
    pub next: String,
    pub band: i32,
    pub drift: i64,
    pub would_archive: bool,
    pub prev_state: Option<String>,
}

/// THE TRIGGER IS TURNS SINCE THE LAST SWEEP, not the context band: cost is proportional
/// to turns since the cursor, not to how deep the context is. A session whose last run
/// FAILED is not re-fired — `covered` only advances on success, so without this gate a
/// failure would re-trigger on every pass. `timeout` is deliberately not excluded: a
/// timed-out run may succeed on a later pass with a scaled timeout (the retry budget in
/// `run.rs::archive` enforces the ceiling).
pub fn would_archive(drift: i64, every: u64, prev_state: Option<&str>) -> bool {
    drift >= every as i64 && prev_state != Some("failed")
}

/// Sort candidates by drift descending, so a limited per-pass budget is spent on the
/// most-drifted session first. Stable: ties keep discovery order, matching Python's
/// documented stable `sorted(..., reverse=True)`.
pub fn sort_by_drift_desc(candidates: &mut [Candidate]) {
    candidates.sort_by(|a, b| b.drift.cmp(&a.drift));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn would_archive_requires_drift_at_or_above_the_threshold() {
        assert!(!would_archive(4, 5, None));
        assert!(would_archive(5, 5, None));
        assert!(would_archive(6, 5, None));
    }

    #[test]
    fn a_previously_failed_session_is_never_refired_by_drift_alone() {
        assert!(!would_archive(100, 5, Some("failed")));
    }

    #[test]
    fn a_timed_out_session_is_still_a_candidate() {
        assert!(would_archive(100, 5, Some("timeout")));
    }

    fn c(sid: &str, drift: i64) -> Candidate {
        Candidate { sid: sid.into(), tp: String::new(), ctx: String::new(), turns: 0, next: String::new(), band: 0, drift, would_archive: drift >= 1, prev_state: None }
    }

    #[test]
    fn sort_orders_most_drifted_first_and_keeps_ties_in_discovery_order() {
        let mut v = vec![c("a", 3), c("b", 9), c("c", 9), c("d", 1)];
        sort_by_drift_desc(&mut v);
        let order: Vec<&str> = v.iter().map(|c| c.sid.as_str()).collect();
        assert_eq!(order, vec!["b", "c", "a", "d"]);
    }
}
