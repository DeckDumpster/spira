//! `rebase-waiting` (DESIGN.md §2.2): a landing leaves the queue at rebased tips, and a real
//! conflict goes back with its paths.

use super::*;

fn waiting_world() -> T {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/heads/local/main", "b1");
    for (id, tip) in [("sp-a", "ta2"), ("sp-b", "tb"), ("sp-c", "tc2")] {
        t.git.set(&format!("refs/heads/spira/{id}"), tip);
    }
    *t.git.branches.borrow_mut() = vec![("spira/sp-a".into(), "ta2".into()), ("spira/sp-b".into(), "tb".into()), ("spira/sp-c".into(), "tc2".into())];
    t.lc_row("sp-a", "SUBMITTED", "ta", 100);
    t.lc_row("sp-b", "CERTIFIED", "tb", 101);
    t.lc_row("sp-c", "REWORK", "tc", 102);
    t.lc_row("sp-d", "WORKING", "td", 103);
    t.lc.rows.borrow_mut().as_mut().unwrap()[2].reason = Some("no-rebase".into());
    {
        let mut rows = t.lc.bead_rows.borrow_mut();
        rows.insert("sp-a".into(), ("SUBMITTED".into(), "5".into()));
        rows.insert("sp-b".into(), ("CERTIFIED".into(), "6".into()));
        rows.insert("sp-c".into(), ("REWORK".into(), "7".into()));
    }
    t.scripts.rebase.borrow_mut().insert("sp-b".into(), RunOut { rc: 1, out: String::new(), err: "rebase-stale: spira/sp-b has a real conflict — returned to an aeon; files: src/x.rs docs/y.md\n".into() });
    t
}

#[test]
fn a_landing_rebases_the_waiting_queue_and_sends_a_conflict_back_with_its_paths() {
    let t = waiting_world();
    assert_eq!(t.run(&["rebase-waiting"]), 0, "{}", t.err());

    let calls = t.scripts.calls.borrow().clone();
    assert!(calls.contains(&"rebase-stale sp-a spira".to_string()) && calls.contains(&"rebase-stale sp-b spira".to_string()) && calls.contains(&"rebase-stale sp-c spira".to_string()), "{calls:?}");
    assert!(!calls.iter().any(|c| c.contains("sp-d")), "a bead being worked is not waiting: {calls:?}");

    assert!(t.lc.has("event bead sp-a SUBMITTED 5 {\"Submit\":{\"tip\":\"ta2\"}}"), "{:?}", t.lc.calls.borrow());
    assert!(t.lc.has("event bead sp-c REWORK 7 {\"Claim\":"), "{:?}", t.lc.calls.borrow());
    assert!(t.lc.has("event bead sp-c WORKING 8 {\"Submit\":{\"tip\":\"tc2\"}}"), "{:?}", t.lc.calls.borrow());

    assert!(t.lc.has("event bead sp-b CERTIFIED 6 {\"GateRed\":{\"tip\":\"tb\",\"reason\":\"no-rebase\"}}"), "{:?}", t.lc.calls.borrow());
    assert!(t.lib.has("comment sp-b rebase-conflict: does not rebase onto b1\npaths: src/x.rs docs/y.md"), "{:?}", t.lib.calls.borrow());
    assert!(!t.lc.has("event bead sp-b CERTIFIED 6 {\"Submit\""));
    assert!(t.landing_log().contains("id=sp-b outcome=conflict files=src/x.rs docs/y.md"), "{}", t.landing_log());
}

#[test]
fn a_red_gate_or_an_unattempted_rebase_moves_nothing() {
    let t = waiting_world();
    t.scripts.rebase.borrow_mut().insert("sp-a".into(), RunOut { rc: 2, ..RunOut::default() });
    t.scripts.rebase.borrow_mut().insert("sp-b".into(), RunOut { rc: 3, ..RunOut::default() });
    t.scripts.rebase.borrow_mut().insert("sp-c".into(), RunOut { rc: 3, ..RunOut::default() });
    assert_eq!(t.run(&["rebase-waiting"]), 0, "{}", t.err());
    assert!(!t.lc.has("event bead"), "{:?}", t.lc.calls.borrow());
    assert!(t.landing_log().contains("id=sp-a outcome=not-moved rc=2") && t.landing_log().contains("id=sp-b outcome=not-moved rc=3"));
}

#[test]
fn a_bead_rebase_stale_already_certified_at_the_new_tip_is_left_alone() {
    let t = waiting_world();
    t.lc.rows.borrow_mut().as_mut().unwrap()[0].tip = Some("ta2".into());
    assert_eq!(t.run(&["rebase-waiting"]), 0, "{}", t.err());
    assert!(!t.lc.has("event bead sp-a"), "{:?}", t.lc.calls.borrow());
    assert!(t.landing_log().contains("id=sp-a outcome=current"));
}
