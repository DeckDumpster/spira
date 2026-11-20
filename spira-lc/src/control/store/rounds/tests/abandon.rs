use super::*;

#[test]
fn an_abandon_whose_member_moved_after_the_plan_writes_nothing() {
    let r = Rig::new();
    green_round(&r);
    let req = r.req();
    let ev = batch_ev(
        BatchState::Green,
        4,
        BatchEventKind::Abandon {
            reason: "stopped".into(),
        },
    );
    let p = r.s.plan_abandon("b-1", &ev, AT, &req).unwrap();
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "abandon").unwrap() else {
        panic!("the abandon did not run");
    };
    assert_eq!(applied, vec![false; 5], "Abandon, and two exits per member");
    assert_eq!(round_state(&r), BatchState::Green);
    let p = r.s.plan_abandon("b-1", &ev, AT, &req).unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(p, &r.req(), "abandon").unwrap() else {
        panic!("the abandon did not run");
    };
    assert_eq!(applied, vec![true; 5]);
    assert_eq!(round_state(&r), BatchState::Abandoned);
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::Certified);
    }
}

#[test]
fn an_abandon_notes_its_reason_on_each_released_member() {
    let r = Rig::new();
    green_round(&r);
    let ev = batch_ev(
        BatchState::Green,
        4,
        BatchEventKind::Abandon {
            reason: "attribution leg died".into(),
        },
    );
    let req = r.req();
    let p = r.s.plan_abandon("b-1", &ev, AT, &req).unwrap();
    r.s.run_planned(p, &req, "abandon").unwrap();
    for id in ["sp-a", "sp-b"] {
        let noted = r
            .s
            .history(id, Some("bead"))
            .unwrap()
            .into_iter()
            .any(|c| c.audit.evidence.get("note").and_then(|n| n.as_str()) == Some("attribution leg died"));
        assert!(noted, "{id} carries the abandon's reason");
    }
}
