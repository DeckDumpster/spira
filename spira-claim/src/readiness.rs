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
            let (claimable, blocked_by) = match verdicts.get(rec.bead_id.as_str()) {
                Some(e) => (claimable(e.verdict), e.blocker.iter().cloned().collect()),
                None => {
                    let waiting: Vec<String> = gates.iter().filter(|(_, g)| g.as_ref().is_some_and(|g| g.class != GateClass::Finished)).map(|(b, _)| b.clone()).collect();
                    (false, waiting)
                }
            };
            json!({ "id": rec.bead_id, "claimable": claimable, "blocked_by": blocked_by, "gates": gates })
        })
        .collect();
    json!({ "computed_at": computed_at, "planning_watermark": planning_watermark, "entries": judged }).to_string()
}
