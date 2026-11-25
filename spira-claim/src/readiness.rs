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
/// claimable, and waits on every unfinished blocker except a parent that does not hold it back.
pub fn payload(live: &[&LifecycleRow], lc: &HashMap<String, LifecycleRow>, bd: &HashMap<String, ReadyRow>, entries: &[Entry], computed_at: &str, planning_watermark: Option<&str>) -> String {
    let verdicts: HashMap<&str, &Entry> = entries.iter().map(|e| (e.id.as_str(), e)).collect();
    let judged: Vec<Value> = live
        .iter()
        .map(|rec| {
            let blockers = bd.get(&rec.bead_id).map(rank::blockers).unwrap_or_default();
            let gates: BTreeMap<String, Option<edges::Gate>> = blockers.iter().map(|b| (b.clone(), lc.get(b).map(|r| edges::gate(r.state, r.tip.as_deref(), r.known_red)))).collect();
            let (claimable, blocked_by, verdict) = match verdicts.get(rec.bead_id.as_str()) {
                Some(e) => (claimable(e.verdict), e.blocker.iter().cloned().collect(), serde_json::to_value(e.verdict).unwrap_or(Value::Null)),
                None => {
                    let cand = bd.get(&rec.bead_id);
                    let waiting: Vec<String> = gates
                        .iter()
                        .filter(|(_, g)| g.as_ref().is_some_and(|g| g.class != GateClass::Finished))
                        .filter(|(b, _)| !cand.is_some_and(|c| c.parent.as_deref() == Some(b.as_str()) && rank::parent_exempt(c, b, lc)))
                        .map(|(b, _)| b.clone())
                        .collect();
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

    use crate::rank::{Dependency, LifecycleRow, ReadyRow};
    use crate::verdicts::{Entry, Kind};

    fn row(id: &str, state: BeadState) -> LifecycleRow {
        LifecycleRow { taken_over: false, bead_id: id.into(), state, holds: BTreeSet::new(), stack_depth: 0, tip: None, snoozed_until: None, stack_conflict: false, known_red: false, stack: Stack::new(), reason: None }
    }

    fn entries_of(live: &[&LifecycleRow], entries: &[Entry]) -> serde_json::Value {
        let lc: HashMap<String, LifecycleRow> = live.iter().map(|r| (r.bead_id.clone(), (*r).clone())).collect();
        serde_json::from_str(&super::payload(live, &lc, &HashMap::new(), entries, "t", None)).unwrap()
    }

    #[test_id(t75jntb)]
    #[test]
    fn an_occupied_entry_publishes_its_verdict_and_an_unjudged_record_its_state() {
        let (a, b) = (row("sp-a", BeadState::Ready), row("sp-b", BeadState::Working));
        let entry = Entry { id: "sp-a".into(), verdict: Kind::Occupied, blocker: None };
        let out = entries_of(&[&a, &b], &[entry]);
        assert_eq!(out["entries"][0]["verdict"], "occupied");
        assert_eq!(out["entries"][1]["verdict"], "state:WORKING");
    }

    fn child_entry(child_state: BeadState, labels: &[&str], parent: LifecycleRow, deps: Vec<Dependency>) -> serde_json::Value {
        let child = row("sp-c", child_state);
        let ready = ReadyRow { id: "sp-c".into(), parent: Some("sp-p".into()), labels: labels.iter().map(|l| l.to_string()).collect(), dependencies: deps, ..Default::default() };
        let lc: HashMap<String, LifecycleRow> = [child.clone(), parent].into_iter().map(|r| (r.bead_id.clone(), r)).collect();
        let bd = HashMap::from([("sp-c".to_string(), ready)]);
        let out: serde_json::Value = serde_json::from_str(&super::payload(&[&child], &lc, &bd, &[], "t", None)).unwrap();
        out["entries"][0].clone()
    }

    #[test_id(tm00bnn)]
    #[test]
    fn an_idle_parent_is_not_named_a_blocker_of_an_unjudged_child_but_stays_in_gates() {
        let e = child_entry(BeadState::Certified, &[], row("sp-p", BeadState::Ready), vec![]);
        assert_eq!(e["blocked_by"], serde_json::json!([]));
        assert_eq!(e["gates"]["sp-p"]["class"], "idle");
    }

    #[test_id(t6g97w1)]
    #[test]
    fn a_parent_that_blocks_is_still_named() {
        let split = child_entry(BeadState::Certified, &[spira_config::nonwork::SPLIT_LABEL], row("sp-p", BeadState::Ready), vec![]);
        assert_eq!(split["blocked_by"], serde_json::json!(["sp-p"]));
        let working = child_entry(BeadState::Certified, &[], row("sp-p", BeadState::Working), vec![]);
        assert_eq!(working["blocked_by"], serde_json::json!(["sp-p"]));
        let mut tipped = row("sp-p", BeadState::Ready);
        tipped.tip = Some("abc".into());
        let tipped = child_entry(BeadState::Certified, &[], tipped, vec![]);
        assert_eq!(tipped["blocked_by"], serde_json::json!(["sp-p"]));
    }

    #[test_id(tq4bzx0)]
    #[test]
    fn a_declared_blocks_dependency_on_a_ready_bead_is_still_named() {
        let dep = Dependency { issue_id: Some("sp-c".into()), depends_on_id: Some("sp-d".into()), dep_type: Some("blocks".into()) };
        let child = row("sp-c", BeadState::Certified);
        let ready = ReadyRow { id: "sp-c".into(), parent: Some("sp-p".into()), dependencies: vec![dep], ..Default::default() };
        let lc: HashMap<String, LifecycleRow> = [child.clone(), row("sp-p", BeadState::Ready), row("sp-d", BeadState::Ready)].into_iter().map(|r| (r.bead_id.clone(), r)).collect();
        let bd = HashMap::from([("sp-c".to_string(), ready)]);
        let out: serde_json::Value = serde_json::from_str(&super::super::payload(&[&child], &lc, &bd, &[], "t", None)).unwrap();
        assert_eq!(out["entries"][0]["blocked_by"], serde_json::json!(["sp-d"]));
    }

    #[test_id(tpdi2u8)]
    #[test]
    fn an_occupied_verdict_is_not_claimable() {
        assert!(!super::super::claimable(super::Kind::Occupied));
    }
}
