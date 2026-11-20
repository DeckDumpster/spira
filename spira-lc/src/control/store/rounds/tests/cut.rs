use super::*;

#[test]
fn a_cut_of_an_id_another_cut_took_after_the_plan_loses_and_leaves_that_round_whole() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let mine = vec![("sp-a".to_string(), T1.to_string())];
    let planned = r.s.plan_cut(&r.cut_spec(&mine)).unwrap();
    r.ok(&[
        "cut",
        "b-1",
        "--repo",
        "spira",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-b:{T2}"),
        "--actor",
        "other",
    ]);
    let Ran::Done { applied, .. } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 3]);
    let (_, round) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(round.value.members.len(), 1);
    assert_eq!(round.value.members[0].bead_id, "sp-b");
    assert_eq!(r.state("sp-a"), BeadState::Certified);
    assert!(r.s.delivery("sp-a").unwrap().is_none());
}

#[test]
fn a_delivery_written_after_the_plan_makes_the_cut_lose() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let mine = vec![("sp-a".to_string(), T1.to_string())];
    let planned = r.s.plan_cut(&r.cut_spec(&mine)).unwrap();
    // Another writer gives sp-a a delivery the plan never saw.
    let born = lifecycle::delivery::DeliveryRow::start_push("sp-a");
    let t = state_store::Txn::new().put(
        &keys::delivery("sp-a"),
        state_store::Object::new(serde_json::to_value(&born).unwrap()),
    );
    assert!(r.s.contract().txn(&t, &r.req()).unwrap().succeeded);
    let Ran::Done { applied, .. } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 3]);
    assert_eq!(
        r.s.delivery("sp-a").unwrap().unwrap().value.state,
        DeliveryState::Pushing
    );
}

#[test]
fn a_cut_is_one_txn_writing_the_round_with_its_members_and_each_members_delivery() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let Planned::Cascade(c) = r.s.plan_cut(&r.cut_spec(&members)).unwrap() else {
        panic!("the cut was turned away");
    };
    assert_eq!(c.steps(), 6, "MemberAdded, Deliver and Cut for each member");
    let head = r.s.contract().head().unwrap();
    let Ran::Done { rev, applied } =
        r.s.run_planned(Planned::Cascade(c), &r.req(), "cut")
            .unwrap()
    else {
        panic!("the cut did not run");
    };
    assert_eq!(
        (rev, applied),
        (head + 1, vec![true; 6]),
        "one revision for the whole cut"
    );
    let (_, round) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(round.value.row.state, BatchState::Open);
    assert_eq!(round.value.row.version, 2);
    assert_eq!(round.value.members.len(), 2);
    for id in ["sp-a", "sp-b"] {
        assert_eq!(r.state(id), BeadState::InDelivery);
        let d = r.s.delivery(id).unwrap().unwrap().value;
        assert_eq!(
            (d.state, d.batch_id.as_deref()),
            (DeliveryState::Batched, Some("b-1"))
        );
    }
    let (refused, audits) = r.audits_at(rev);
    assert!(!refused);
    let events: Vec<(&str, &str)> = audits
        .iter()
        .map(|a| (a.machine.as_str(), a.event.as_str()))
        .collect();
    assert_eq!(
        events,
        [
            ("batch", "MemberAdded"),
            ("bead", "Deliver"),
            ("delivery", "Enqueued"),
            ("delivery", "Cut"),
            ("batch", "MemberAdded"),
            ("bead", "Deliver"),
            ("delivery", "Enqueued"),
            ("delivery", "Cut"),
        ]
    );
    let history = r.s.history("sp-a", Some("delivery")).unwrap();
    assert_eq!(
        history.len(),
        2,
        "the delivery's birth and its cut, in its own history"
    );
}

#[test]
fn a_cut_whose_member_moved_after_the_plan_writes_nothing_and_audits_every_step_as_lost() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let planned = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    let a_before = r.s.contract().get("lifecycle/sp-a").unwrap().unwrap();
    // Another writer touches the second member between the plan and the commit.
    r.ok(&["set-repo", "sp-b", "other"]);
    let Ran::Done { rev, applied } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 6]);
    assert!(r.s.batch("b-1").unwrap().is_none(), "no round");
    for id in ["sp-a", "sp-b"] {
        assert!(r.s.delivery(id).unwrap().is_none(), "no delivery for {id}");
        assert_eq!(r.state(id), BeadState::Certified);
    }
    let a_after = r.s.contract().get("lifecycle/sp-a").unwrap().unwrap();
    assert_eq!(
        a_after.mod_rev, a_before.mod_rev,
        "the unmoved member is untouched too"
    );
    let (refused, lost) = r.audits_at(rev);
    assert!(refused);
    assert_eq!(lost.len(), 6);
    assert!(lost.iter().all(|a| !a.applied
        && a.refusal.as_deref() == Some("StaleVersion")
        && a.from_state == a.to_state));
    let (rc, out) = r.lc(&["show-batch", "b-1"]);
    assert_eq!((rc, out.as_str()), (1, "{}"));
}

#[test]
fn a_cut_refuses_a_member_not_at_its_tip_or_still_in_another_round_before_writing() {
    let r = Rig::new();
    green_round(&r);
    r.certified("sp-c", T1);
    let head = r.s.contract().head().unwrap();
    let wrong_tip = vec![("sp-c".to_string(), T2.to_string())];
    let p =
        r.s.plan_cut(&Cut {
            batch_id: "b-2",
            ..r.cut_spec(&wrong_tip)
        })
        .unwrap();
    assert!(
        matches!(p, Planned::Ran(Ran::Turned(super::Turned::Refused(ref m))) if m.contains("certified tip does not match"))
    );
    let (rc, out) = r.lc(&[
        "cut",
        "b-2",
        "--repo",
        "spira",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-c:{T1},sp-a:{T1}"),
        "--actor",
        "test",
    ]);
    assert_eq!(
        (rc, out.as_str()),
        (3, "refused: sp-a is IN_DELIVERY not SUBMITTED or CERTIFIED")
    );
    assert_eq!(r.s.contract().head().unwrap(), head, "nothing written");
}

#[test]
fn a_round_id_is_taken_in_every_repo_once_any_repo_cuts_it() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let mine = vec![("sp-a".to_string(), T1.to_string())];
    let planned = r.s.plan_cut(&r.cut_spec(&mine)).unwrap();
    let theirs = vec![("sp-b".to_string(), T2.to_string())];
    let other = Cut {
        repo: "other",
        ..r.cut_spec(&theirs)
    };
    let p = r.s.plan_cut(&other).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(planned, &r.req(), "cut").unwrap() else {
        panic!("the cut did not run");
    };
    assert_eq!(applied, vec![false; 3], "the index compared absent loses");
    let (key, _) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(key, keys::batch("other", "b-1"));
    assert!(r
        .s
        .contract()
        .get(&keys::batch("spira", "b-1"))
        .unwrap()
        .is_none());
}

#[test]
fn a_cut_ships_each_step_it_committed_under_its_machine() {
    use spira_otel::vlogs::fake::FakeLogs;
    use std::time::Duration;
    let logs = FakeLogs::start();
    let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock()), Config { min_lock_ttl_ms: 1_000 }).unwrap();
    let r = Rig { s: Store::new(c, 1_000).with_shipper(spira_otel::vlogs::Shipper::start(&logs.url())), n: Cell::new(0) };
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let Planned::Cascade(c) = r.s.plan_cut(&r.cut_spec(&members)).unwrap() else { panic!("the cut was turned away") };
    r.s.run_planned(Planned::Cascade(c), &r.req(), "cut").unwrap();
    let cut: Vec<(String, String)> = logs
        .wait_until(|got| got.iter().filter(|rec| rec["stream"] == "lifecycle.delivery").count() >= 4, Duration::from_secs(5))
        .into_iter()
        .filter(|rec| rec["to"] == "IN_DELIVERY" || rec["stream"] == "lifecycle.delivery")
        .map(|rec| (rec["stream"].as_str().unwrap().to_string(), rec["bead_id"].as_str().unwrap().to_string()))
        .collect();
    assert!(cut.iter().any(|(s, b)| s == "lifecycle.bead" && b == "sp-a"), "{cut:?}");
    assert!(cut.iter().any(|(s, b)| s == "lifecycle.delivery" && b == "sp-b"), "the cut's delivery steps ship under their own machine: {cut:?}");
}

#[test]
fn a_round_cut_with_one_member_twice_writes_no_discontinuous_delivery_event() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let members = vec![("sp-a".to_string(), T1.to_string()); 2];
    let head = r.s.contract().head().unwrap();
    let planned = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    let Planned::Ran(Ran::Turned(Turned::CannotTell(why))) = &planned else {
        panic!("a cut naming a member twice must be refused whole");
    };
    assert!(why.contains("sp-a is named twice"), "{why}");
    assert_eq!(r.s.contract().head().unwrap(), head, "a refused cut writes nothing");
    assert!(r.s.delivery("sp-a").unwrap().is_none());
}
