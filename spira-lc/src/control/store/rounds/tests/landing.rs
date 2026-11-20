use super::*;

#[test]
fn a_landing_moves_the_round_and_every_member_in_one_txn() {
    let r = Rig::new();
    green_round(&r);
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(4), AT, &req).unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "land").unwrap() else {
        panic!("the landing did not run");
    };
    assert_eq!(
        applied,
        vec![true; 5],
        "FastForward, and Delivered twice per member"
    );
    assert_eq!(
        r.s.batch("b-1").unwrap().unwrap().1.value.row.state,
        BatchState::Landed
    );
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::Landed);
        let d = r.s.delivery(id).unwrap().unwrap().value;
        assert_eq!(
            (d.state, d.merge_sha.as_deref()),
            (DeliveryState::Exited, Some("M1"))
        );
    }
}

#[test]
fn a_landing_whose_member_moved_after_the_plan_writes_nothing() {
    let r = Rig::new();
    green_round(&r);
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(4), AT, &req).unwrap();
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "land").unwrap() else {
        panic!("the landing did not run");
    };
    assert_eq!(applied, vec![false; 5]);
    assert_eq!(
        r.s.batch("b-1").unwrap().unwrap().1.value.row.state,
        BatchState::Green
    );
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::InDelivery);
        assert_eq!(
            r.s.delivery(id).unwrap().unwrap().value.state,
            DeliveryState::Batched
        );
    }
}

#[test]
fn a_batch_event_its_machine_refuses_writes_no_round_and_is_audited() {
    let r = Rig::new();
    green_round(&r);
    let before =
        r.s.contract()
            .get(&keys::batch("spira", "b-1"))
            .unwrap()
            .unwrap();
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(3), AT, &req).unwrap();
    let Planned::Ran(Ran::Refused { rev, refusal }) = p else {
        panic!("a stale landing was planned");
    };
    assert_eq!(
        refusal,
        lifecycle::Refusal::StaleVersion {
            given: 3,
            current: 4
        }
    );
    let after =
        r.s.contract()
            .get(&keys::batch("spira", "b-1"))
            .unwrap()
            .unwrap();
    assert_eq!(after.mod_rev, before.mod_rev);
    let (_, audits) = r.audits_at(rev);
    assert_eq!(audits[0].refusal.as_deref(), Some("StaleVersion"));
    assert_eq!(r.s.history("b-1", Some("batch")).unwrap().len(), 5);
}

#[test]
fn a_round_landing_enqueues_one_closeout_for_each_github_member_and_none_for_the_rest() {
    let r = Rig::new().with_refs(&[("sp-a", "github:o/r#7")]);
    green_round(&r);
    let req = r.req();
    let p = r.s.plan_land("b-1", &fast_forward(4), AT, &req).unwrap();
    r.s.run_planned(p, &req, "land").unwrap();
    let rows = r.s.gh_closeouts().unwrap();
    assert_eq!(rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["sp-a"], "{rows:?}");
    assert_eq!((rows[0].1["outcome_state"].as_str(), rows[0].1["external_ref"].as_str()), (Some("LANDED"), Some("github:o/r#7")));
    r.s.gh_closeout_done("sp-a", &r.req()).unwrap();
    let again = r.lc(&["event", "bead", "sp-a", "--expect", "LANDED", "--version", "99", "--actor", "test", "--kind", r#"{"ContentOnBase":{"proof":"p"}}"#]);
    assert_ne!(again.0, 0, "a landed bead refuses a second landing: {}", again.1);
    assert!(r.s.gh_closeouts().unwrap().is_empty(), "a repeated landing event enqueues nothing");
}

#[test]
fn a_content_on_base_landing_enqueues_one_closeout() {
    let r = Rig::new().with_refs(&[("sp-a", "github:o/r#7")]);
    r.ok(&["create-bead", "sp-a", "--repo", "spira", "--ready"]);
    r.bead_event("sp-a", "READY", "0", r#"{"ContentOnBase":{"proof":"p"}}"#);
    assert_eq!(r.state("sp-a"), BeadState::Landed);
    assert_eq!(r.s.gh_closeouts().unwrap().len(), 1);
}
