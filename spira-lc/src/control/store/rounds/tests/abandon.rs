use super::*;

#[test_id(t9yrpay)]
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

#[test_id(tfpoalg)]
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

const CLAIM: &str = r#"{"Claim":{"holder":"dwarf-1","lease_until":1767229200}}"#;

fn pass_event(n: u32, red: bool) -> BatchEventKind {
    if red {
        BatchEventKind::PassRed { n, red_suites: vec!["x".into()], suites_s: 1, build_s: 1, runner: None }
    } else {
        BatchEventKind::PassIncomplete { n, reason: "vm lost".into() }
    }
}

/// `sp-a` SUBMITTED at T1 in a round whose pass 1 ended as `ended`, abandoned and then certified
/// by the gate at T1, with `sp-b` waiting on it.
fn abandoned_at_the_pass(ended: Option<bool>, restarted: bool) -> Rig {
    let r = Rig::new();
    r.submitted_with("sp-a", T1, CLAIM);
    r.ok(&["set-claim-inputs", "sp-a"]);
    r.ok(&["create-bead", "sp-b", "--repo", "spira", "--ready"]);
    r.ok(&["set-claim-inputs", "sp-b", "--block", "sp-a"]);
    let one = vec![("sp-a".to_string(), T1.to_string())];
    let p = r.s.plan_cut(&r.cut_spec(&one)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    let send = |expect: BatchState, kind: BatchEventKind| {
        let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
        let ev = BatchEvent { expect, version: rec.value.row.version, kind, actor: "test".into() };
        r.s.batch_event_with("b-1", &ev, AT + 10, &r.req(), None).unwrap();
    };
    let mut from = BatchState::Open;
    if let Some(red) = ended {
        send(BatchState::Open, BatchEventKind::PassStarted { n: 1, head: "h1".into() });
        send(BatchState::CiRunning, pass_event(1, red));
        from = BatchState::Attributing;
        if restarted {
            send(BatchState::Attributing, BatchEventKind::PassStarted { n: 2, head: "h2".into() });
            from = BatchState::CiRunning;
        }
    }
    let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
    let ev = batch_ev(from, rec.value.row.version, BatchEventKind::Abandon { reason: "names no member that can be ejected".into() });
    let req = r.req();
    let p = r.s.plan_abandon("b-1", &ev, AT + 20, &req).unwrap();
    r.s.run_planned(p, &req, "abandon").unwrap();
    assert_eq!(r.state("sp-a"), BeadState::Submitted);
    let v = r.s.record("sp-a").unwrap().unwrap().value.row.version.to_string();
    r.bead_event("sp-a", "SUBMITTED", &v, &format!(r#"{{"GatePass":{{"tip":"{T1}","gate_key":"k2"}}}}"#));
    assert_eq!(r.state("sp-a"), BeadState::Certified);
    r
}

fn claim_of_b_on(r: &Rig, tip: &str) -> crate::control::store::Outcome {
    let v = r.s.record("sp-b").unwrap().unwrap().value.row.version;
    let stack = std::collections::BTreeMap::from([("sp-a".to_string(), tip.to_string())]);
    let kind = lifecycle::bead::BeadEventKind::Claim { holder: "dwarf-2".into(), lease_until: AT + 3_600, stack, stack_depth: 1, stack_max_depth: 4, persona: None, directed: None };
    let ev = lifecycle::bead::BeadEvent { expect: BeadState::Ready, version: v, kind, actor: "dwarf-2".into(), at: Some(AT + 30) };
    r.s.claim("sp-b", &ev, &r.req()).unwrap().0
}

#[test_id(t7qccb0)]
#[test]
fn a_blocker_certified_at_the_tip_a_red_pass_abandoned_is_not_stacked_on_and_a_new_certified_tip_is() {
    let r = abandoned_at_the_pass(Some(true), false);
    let b = claim_view_of(&r, "sp-b");
    assert_eq!((b["verdict"].as_str(), b["blocker"].as_str()), (Some("blocked"), Some("sp-a")), "{b}");
    assert!(matches!(claim_of_b_on(&r, T1), crate::control::store::Outcome::BlockerMoved { .. }), "a claim stacked on the red tip conflicts");

    let v = r.s.record("sp-a").unwrap().unwrap().value.row.version.to_string();
    r.bead_event("sp-a", "CERTIFIED", &v, &format!(r#"{{"Submit":{{"tip":"{T2}"}}}}"#));
    let v = r.s.record("sp-a").unwrap().unwrap().value.row.version.to_string();
    r.bead_event("sp-a", "SUBMITTED", &v, &format!(r#"{{"GatePass":{{"tip":"{T2}","gate_key":"k3"}}}}"#));
    assert_eq!(claim_view_of(&r, "sp-b")["verdict"], "claimable");
    assert!(matches!(claim_of_b_on(&r, T2), crate::control::store::Outcome::Applied { .. }));
}

#[test_id(tbmffjk)]
#[test]
fn a_blocker_certified_after_a_pass_that_gave_no_verdict_is_still_stacked_on() {
    for (ended, restarted) in [(Some(false), false), (None, false), (Some(true), true)] {
        let r = abandoned_at_the_pass(ended, restarted);
        let ended = (ended, restarted);
        assert_eq!(claim_view_of(&r, "sp-b")["verdict"], "claimable", "{ended:?}");
        assert!(matches!(claim_of_b_on(&r, T1), crate::control::store::Outcome::Applied { .. }), "{ended:?}");
    }
}
