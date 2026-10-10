use serde_json::{json, Value};
use spira_sim::trace::{bead_row, invariant_views, violations, Event, Snapshot, Trace};

fn ev(seq: u64, vtime: u64, kind: &'static str, actor: &str, started: Option<u64>) -> Event {
    Event { seq, vtime, kind, actor: Some(actor.into()), started, completed: started.map(|_| vtime), exit: Some(0), coalesced: false, seed: 5 }
}

fn bead(fields: Value) -> Value {
    let mut base = json!({"bead": "sp-x", "bd_status": "open", "lc_state": "WORKING", "lc_version": 1, "holds": "", "landstate": null,
        "lc_tip": "aaa", "branch_tip": "aaa", "on_local_main": false, "on_origin_main": false});
    for (k, v) in fields.as_object().unwrap() {
        base[k] = v.clone();
    }
    bead_row(&base).unwrap()
}

fn hits(events: &[Event], beads: Vec<Value>) -> Vec<(String, u64)> {
    let w = testkit::TempDir::new("sim-trace");
    let t = Trace::create(&w).unwrap();
    for e in events {
        t.event(e).unwrap();
    }
    t.snapshot(1, &Snapshot { beads, ..Default::default() }).unwrap();
    let db = t.load(&w).unwrap();
    violations(&db, &[]).unwrap().into_iter().map(|v| (v.view, v.seq)).collect()
}

fn only(view: &str, seq: u64) -> Vec<(String, u64)> {
    vec![(view.to_string(), seq)]
}

#[test]
fn tip_matches_branch() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let bad = bead(json!({"lc_state": "CERTIFIED", "lc_tip": "aaa", "branch_tip": "bbb"}));
    assert_eq!(hits(&[], vec![bad]), only("inv_tip_matches_branch", 1));
    let ok = bead(json!({"lc_state": "SUBMITTED"}));
    assert!(hits(&[], vec![ok]).is_empty());
}

#[test]
fn landstate_certified_needs_lifecycle_certified_at_the_same_tip() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let bad = bead(json!({"landstate": "CERTIFIED", "lc_state": "SUBMITTED"}));
    assert_eq!(hits(&[], vec![bad]), only("inv_landstate_certified_needs_lifecycle", 1));
    let ok = bead(json!({"landstate": "CERTIFIED", "lc_state": "CERTIFIED"}));
    assert!(hits(&[], vec![ok]).is_empty());
}

#[test]
fn landed_tip_is_on_local_main() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let bad = bead(json!({"lc_state": "LANDED", "on_local_main": false}));
    assert_eq!(hits(&[], vec![bad]), only("inv_landed_is_on_local_main", 1));
    let ok = bead(json!({"lc_state": "LANDED", "on_local_main": true}));
    assert!(hits(&[], vec![ok]).is_empty());
}

#[test]
fn closed_bead_has_terminal_lifecycle() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let bad = bead(json!({"bd_status": "closed", "lc_state": "SUBMITTED"}));
    assert_eq!(hits(&[], vec![bad]), only("inv_closed_is_terminal", 1));
    let ok = bead(json!({"bd_status": "closed", "lc_state": "DONE"}));
    assert!(hits(&[], vec![ok]).is_empty());
}

#[test]
fn every_complete_has_a_start() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let bad = [ev(1, 10, "complete", "a", Some(0))];
    assert_eq!(hits(&bad, vec![]), only("inv_complete_has_start", 1));
    let ok = [ev(1, 0, "start", "a", None), ev(2, 10, "complete", "a", Some(0))];
    assert!(hits(&ok, vec![]).is_empty());
}

#[test]
fn an_actor_does_not_overlap_itself() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let bad = [ev(1, 0, "start", "a", None), ev(2, 5, "start", "a", None)];
    assert_eq!(hits(&bad, vec![]), only("inv_actor_does_not_overlap_itself", 2));
    let ok = [ev(1, 0, "start", "a", None), ev(2, 5, "start", "b", None), ev(3, 9, "complete", "a", Some(0)), ev(4, 10, "start", "a", None)];
    assert!(hits(&ok, vec![]).is_empty());
}

#[test]
fn a_new_invariant_view_needs_a_violating_and_a_clean_fixture() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    assert_eq!(invariant_views().len(), 6);
}

#[test]
fn unknown_probe_field_is_refused() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    assert!(bead_row(&json!({"bead": "x", "lc_stat": "READY"})).is_err());
    assert!(bead_row(&json!({"lc_state": "READY"})).is_err());
}

fn sql(name: &str, q: &str) -> spira_sim::drive::SqlDef {
    spira_sim::drive::SqlDef { name: name.into(), sql: q.into() }
}

fn traced(events: &[Event], beads: Vec<Value>) -> (testkit::TempDir, std::path::PathBuf) {
    let w = testkit::TempDir::new("sim-trace-own");
    let t = Trace::create(&w).unwrap();
    for e in events {
        t.event(e).unwrap();
    }
    t.snapshot(1, &Snapshot { beads, ..Default::default() }).unwrap();
    let db = t.load(&w).unwrap();
    (w, db)
}

#[test]
fn a_scenarios_own_invariant_reports_its_rows_under_its_own_name() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let (_w, db) = traced(&[ev(1, 10, "step", "step1", None)], vec![bead(json!({"lc_state": "SUBMITTED"}))]);
    let own = [sql("submitted_is_unwanted", "SELECT seq, bead FROM bead_state WHERE lc_state = 'SUBMITTED'")];
    let v = violations(&db, &own).unwrap();
    assert_eq!(v.iter().map(|v| (v.view.as_str(), v.seq)).collect::<Vec<_>>(), vec![("scn_submitted_is_unwanted", 1)]);
    let none = [sql("landed_is_unwanted", "SELECT seq FROM bead_state WHERE lc_state = 'LANDED'")];
    assert!(violations(&db, &none).unwrap().is_empty());
}

#[test]
fn an_expectation_is_unmet_exactly_when_its_query_returns_no_row() {
    if !spira_sim::trace::duckdb_available() {
        return;
    }
    let (_w, db) = traced(&[ev(1, 10, "step", "step1", None)], vec![bead(json!({"lc_state": "CERTIFIED"}))]);
    let met = sql("certified", "SELECT 1 FROM bead_state WHERE lc_state = 'CERTIFIED'");
    let unmet = sql("landed", "SELECT 1 FROM bead_state WHERE lc_state = 'LANDED'");
    assert_eq!(spira_sim::trace::unmet(&db, &[met, unmet]).unwrap(), vec!["landed".to_string()]);
}
