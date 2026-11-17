//! The readiness pass: what the dispatcher publishes onto the control store for every live
//! record, whatever its state, so a claim compares each blocker's gate as this pass read it.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Value};
use spira_config::edges::{self, GateClass};

use crate::rank::{self, LifecycleRow, ReadyRow};
use crate::verdicts::{Entry, Kind};

/// The states a record is live in: every one a claim, a cut or a blocker still reads.
pub const LIVE_STATES: [&str; 7] = ["OPEN", "READY", "WORKING", "SUBMITTED", "CERTIFIED", "IN_DELIVERY", "REWORK"];

/// Whether a verdict leaves the record claimable by someone. Scope and carriage are the
/// caller's and the round's, not the record's, so they do not make it unclaimable.
fn claimable(kind: Kind) -> bool {
    matches!(kind, Kind::Claimable | Kind::OutOfScope)
}

/// The `publish-readiness` payload for `live` records. `lc` holds them and their blockers;
/// `bd` the content that names each one's blockers; `entries` the verdicts the claim rule
/// reached for the READY and REWORK ones. A record the claim rule did not judge is not
/// claimable, and waits on every blocker that has not finished.
pub fn payload(live: &[&LifecycleRow], lc: &HashMap<String, LifecycleRow>, bd: &HashMap<String, ReadyRow>, entries: &[Entry], computed_at: &str, planning_watermark: Option<&str>) -> String {
    let verdicts: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let judged: Vec<Value> = live
        .iter()
        .map(|rec| {
            let blockers = bd.get(&rec.bead_id).map(rank::blockers).unwrap_or_default();
            let gates: BTreeMap<String, Option<edges::Gate>> = blockers.iter().map(|b| (b.clone(), lc.get(b).map(|r| edges::gate(r.state, r.tip.as_deref())))).collect();
            let (claimable, blocked_by, verdict) = match verdicts.get(rec.bead_id.as_str()) {
                Some(e) => (claimable(e.verdict), e.blocker.iter().cloned().collect(), serde_json::to_value(e.verdict).unwrap_or(Value::Null)),
                None => {
                    let waiting: Vec<String> = gates.iter().filter(|(_, g)| g.as_ref().is_some_and(|g| g.class != GateClass::Finished)).map(|(b, _)| b.clone()).collect();
                    (false, waiting, json!(format!("state:{}", rec.state.as_str())))
                }
            };
            json!({ "id": rec.bead_id, "claimable": claimable, "blocked_by": blocked_by, "verdict": verdict, "gates": gates })
        })
        .collect();
    json!({ "computed_at": computed_at, "planning_watermark": planning_watermark, "entries": judged }).to_string()
}

#[cfg(test)]
mod occupied_tests {
    use std::collections::{BTreeSet, HashMap};

    use lifecycle::bead::{BeadState, Stack};

    use crate::rank::LifecycleRow;
    use crate::verdicts::{Entry, Kind};

    fn row(id: &str, state: BeadState) -> LifecycleRow {
        LifecycleRow { bead_id: id.into(), state, holds: BTreeSet::new(), stack_depth: 0, tip: None, snoozed_until: None, stack_conflict: false, stack: Stack::new(), reason: None }
    }

    fn entries_of(live: &[&LifecycleRow], entries: &[Entry]) -> serde_json::Value {
        let lc: HashMap<String, LifecycleRow> = live.iter().map(|r| (r.bead_id.clone(), (*r).clone())).collect();
        serde_json::from_str(&super::payload(live, &lc, &HashMap::new(), entries, "t", None)).unwrap()
    }

    #[test]
    fn an_occupied_entry_publishes_its_verdict_and_an_unjudged_record_its_state() {
        let (a, b) = (row("sp-a", BeadState::Ready), row("sp-b", BeadState::Working));
        let entry = Entry { id: "sp-a".into(), verdict: Kind::Occupied, blocker: None };
        let out = entries_of(&[&a, &b], &[entry]);
        assert_eq!(out["entries"][0]["verdict"], "occupied");
        assert_eq!(out["entries"][1]["verdict"], "state:WORKING");
    }

    #[test]
    fn an_occupied_verdict_is_not_claimable() {
        assert!(!super::claimable(super::Kind::Occupied));
    }
}
