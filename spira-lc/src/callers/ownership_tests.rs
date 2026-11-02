//! The ownership table in force, through the verbs a caller runs: an epic has no lifecycle
//! record, so its child is never held by one; an ask's state is the control plane's; and bd's
//! status follows the control plane, written by reconcile-content.

use lifecycle::bead::BeadState;
use serde_json::Value;

use super::world::World;
use super::*;

fn v(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

/// The record's `(state, has_tip)`, as the claim rule reads a parent; `None` when it has none.
fn record_of(w: &World, id: &str) -> Option<(BeadState, bool)> {
    let (rc, out) = w.lc(&["show", id]);
    if rc != 0 {
        return None;
    }
    let row: Value = serde_json::from_str(&out).unwrap();
    let state = BeadState::from_str(row["bead"]["state"].as_str()?)?;
    Some((state, row["bead"]["tip"].as_str().is_some()))
}

#[test]
fn create_bead_refuses_an_epic_and_a_decision_naming_the_type() {
    let w = World::new();
    w.file("sp-e", "epic", None, &[]);
    w.file("sp-d", "decision", None, &["ask"]);
    w.file("sp-t", "task", None, &[]);
    for (id, kind) in [("sp-e", "epic"), ("sp-d", "decision")] {
        let (rc, out) = w.lc(&["create-bead", id, "--ready"]);
        assert_eq!(rc, REFUSED, "{id}: {out}");
        assert!(out.contains(kind), "the refusal names the type: {out}");
        assert_eq!(record_of(&w, id), None);
    }
    w.ok(&["create-bead", "sp-t", "--ready"]);
    assert_eq!(record_of(&w, "sp-t"), Some((BeadState::Ready, false)));
}

#[test]
fn an_ask_closed_in_the_control_plane_is_not_open_and_reconcile_content_closes_its_decision_bead() {
    let w = World::new();
    w.file("sp-q", "decision", None, &["ask"]);
    w.ok(&["create-ask", "sp-q"]);
    w.ok(&["close-ask", "sp-q", "--exit", "answered", "--quote", "ship it", "--actor", "operator", "--channel", "pane"]);
    let open: Vec<Value> = serde_json::from_str(&w.ok(&["list-asks"])).unwrap();
    assert!(open.iter().all(|a| a["ask_id"] != "sp-q"), "a closed ask is listed open: {open:?}");
    assert_eq!(w.status("sp-q"), "open", "the decision bead is still open in bd");

    let (mut m, mut b) = (&w, &w);
    let a = reconcile_content(&v(&["--apply"]), &mut m, &mut b);
    assert_eq!(a.code, APPLIED, "{a:?}");
    assert_eq!(w.status("sp-q"), "closed", "{}", a.stdout);
    assert_eq!(*w.writes.borrow(), ["close sp-q"]);
    let a = reconcile_content(&v(&["--apply"]), &mut m, &mut b);
    assert_eq!((a.code, w.writes.borrow().len()), (APPLIED, 1), "a second pass writes nothing: {}", a.stdout);
}
