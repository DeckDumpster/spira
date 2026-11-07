//! With the lifecycle on the control store, no verb reaches the dolt connection except the
//! ones `served::DOLT_BY_DESIGN` names, and every refusal names its verb.

use std::collections::BTreeSet;

use crate::control::served::{self, testing};
use crate::db::{Conn, DIALS};

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Every verb `dispatch` names: the arms of its own match, and each served or caller list.
fn every_verb() -> BTreeSet<String> {
    let src = include_str!("main.rs");
    let table = &src[src.find("pub fn dispatch(").unwrap()..src.find("fn cmd_admin_apply_ddl").unwrap()];
    let mut verbs: BTreeSet<String> = table
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Some(\"")?.split('"').next().map(String::from))
        .collect();
    let lists: [&[&str]; 11] = [
        crate::control::state::VERBS,
        crate::control::state::rounds::VERBS,
        crate::control::state::cascades::VERBS,
        crate::control::state::asks::VERBS,
        crate::control::state::mending::VERBS,
        crate::control::state::conditions::VERBS,
        crate::control::verbs::VERBS,
        crate::control::readiness::VERBS,
        crate::callers::VERBS,
        crate::control::sections::VERBS,
        served::DOLT_BY_DESIGN,
    ];
    verbs.extend(lists.iter().flat_map(|l| l.iter().map(|v| v.to_string())));
    verbs.remove("work");
    verbs
}

#[test]
fn the_sweep_names_the_verbs_that_fell_through() {
    let verbs = every_verb();
    for v in ["reconcile-landed", "dep-add", "ops-view", "churn-facts", "fact", "show", "admin-import"] {
        assert!(verbs.contains(v), "{v} is missing from the sweep: it would pass by not being tried");
    }
}

#[test]
fn no_verb_opens_a_dolt_connection_while_the_lifecycle_is_on_the_store() {
    let dir = testkit::TempDir::new("lc-store-only");
    testing::with_store(dir.path(), |_| {
        let conn = Conn::unreachable();
        let mut dialed = Vec::new();
        for verb in every_verb() {
            if served::DOLT_BY_DESIGN.contains(&verb.as_str()) {
                continue;
            }
            let before = DIALS.with(|d| d.get());
            let (_, out) = crate::dispatch(&args(&[&verb, "sp-t1"]), &conn);
            if DIALS.with(|d| d.get()) != before {
                dialed.push(format!("{verb}: {out}"));
            }
        }
        assert!(dialed.is_empty(), "verbs that dialed dolt:\n{}", dialed.join("\n"));
    });
}

#[test]
fn an_unserved_verb_refuses_naming_itself() {
    let dir = testkit::TempDir::new("lc-store-only");
    testing::with_store(dir.path(), |_| {
        let (code, out) = crate::dispatch(&args(&["ops-view", "ops_live"]), &Conn::unreachable());
        assert_eq!(code, crate::REFUSED);
        assert!(out.contains("ops-view"), "{out}");
    });
}

#[test]
fn the_dolt_by_design_verbs_are_the_named_ones() {
    assert_eq!(served::DOLT_BY_DESIGN, ["fact", "facts", "facts-query", "journal"]);
}

#[test]
fn a_bead_event_lands_in_the_store_and_reads_back_from_it() {
    use lifecycle::bead::{BeadEventKind, HoldKind};
    use lifecycle::reason::HoldCause;
    let dir = testkit::TempDir::new("lc-store-only");
    testing::with_store(dir.path(), |_| {
        let conn = Conn::unreachable();
        let before = DIALS.with(|d| d.get());
        let (code, out) = crate::dispatch(&args(&["create-bead", "sp-t1", "--repo", "spira", "--ready"]), &conn);
        assert_eq!(code, 0, "{out}");
        let hold = BeadEventKind::Hold { kind: HoldKind::Manual, cause: HoldCause::SupersedeRequest, detail: None };
        let (code, out) = crate::apply_bead_event(&conn, "sp-t1", "dwarf", hold);
        assert_eq!(code, 0, "{out}");
        let row = crate::rows::fetch_bead(&conn, "sp-t1").unwrap().unwrap();
        assert!(row.holds.contains(&HoldKind::Manual), "{row:?}");
        assert_eq!(crate::rows::bead_repo(&conn, "sp-t1").unwrap().as_deref(), Some("spira"));
        assert!(crate::rows::fetch_delivery(&conn, "sp-t1").unwrap().is_none());
        assert_eq!(DIALS.with(|d| d.get()), before, "the event, its reads and its repo lookup dialed dolt");
        let (_, shown) = crate::dispatch(&args(&["show", "sp-t1"]), &conn);
        assert!(shown.contains("\"manual\"") || shown.contains("manual"), "{shown}");
    });
}

#[test]
fn reconcile_landed_records_the_landing_in_the_store() {
    use lifecycle::bead::BeadEventKind;
    let dir = testkit::TempDir::new("lc-store-only");
    testing::with_store(dir.path(), |_| {
        let conn = Conn::unreachable();
        crate::dispatch(&args(&["create-bead", "sp-t2", "--repo", "spira", "--ready"]), &conn);
        let row = crate::rows::fetch_bead(&conn, "sp-t2").unwrap().unwrap();
        let ev = lifecycle::bead::BeadEvent { expect: row.state, version: row.version, kind: BeadEventKind::ContentOnBase { proof: "reconcile-landed:abc".into() }, actor: "hel".into(), at: Some(1) };
        let before = DIALS.with(|d| d.get());
        let (code, out) = crate::cutover::reconcile_on_store("sp-t2", "abc", "hel", &ev, "reconcile-landed:abc", &conn);
        assert_eq!(code, 0, "{out}");
        let after = crate::rows::fetch_bead(&conn, "sp-t2").unwrap().unwrap();
        assert_eq!(after.state, lifecycle::bead::BeadState::Landed);
        assert_eq!(DIALS.with(|d| d.get()), before);
    });
}
