use super::*;

#[test_id(t27u9d7)]
#[test]
fn a_stack_whose_member_moved_after_the_plan_writes_nothing_and_none_past_the_cap_is_taken() {
    let r = Rig::new();
    open_round(&r);
    r.certified("sp-c", T3);
    let more = vec![("sp-c".to_string(), T3.to_string())];
    let p = r.s.plan_stack("b-1", &more, "test", AT).unwrap();
    r.ok(&["set-repo", "sp-c", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &r.req(), "stack").unwrap() else {
        panic!("the stack did not run");
    };
    assert_eq!(applied, vec![false; 3]);
    assert_eq!(r.s.batch("b-1").unwrap().unwrap().1.value.members.len(), 1);
    assert_eq!(r.state("sp-c"), BeadState::Certified);
    let full = ids(CAP);
    let p = r.s.plan_stack("b-1", &full, "test", AT).unwrap();
    assert!(
        matches!(p, Planned::Ran(Ran::Turned(super::Turned::Refused(ref m))) if m.contains(&format!("would hold {}", CAP + 1)))
    );
    let p = r.s.plan_stack("b-1", &more, "test", AT).unwrap();
    let Ran::Done { applied, .. } = r.s.run_planned(p, &r.req(), "stack").unwrap() else {
        panic!("the stack did not run");
    };
    assert_eq!(applied, vec![true; 3]);
    assert_eq!(r.s.batch("b-1").unwrap().unwrap().1.value.members.len(), 2);
}

#[test_id(tp50r18)]
#[test]
fn a_promotion_whose_member_moved_after_the_plan_leaves_the_round_staged() {
    let r = Rig::new();
    open_round(&r);
    r.certified("sp-c", T3);
    r.ok(&[
        "stage",
        "b-2",
        "--repo",
        "spira",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-c:{T3}"),
        "--actor",
        "test",
        "--parent",
        "b-1",
    ]);
    assert_eq!(
        r.state("sp-c"),
        BeadState::Certified,
        "staging delivers nothing"
    );
    let req = r.req();
    let p =
        r.s.plan_promote("b-2", "H2", "B2", "test", AT, &req)
            .unwrap();
    r.ok(&["set-repo", "sp-c", "other"]);
    let Ran::Done { applied, .. } = r.s.run_planned(p, &req, "promote").unwrap() else {
        panic!("the promotion did not run");
    };
    assert_eq!(applied, vec![false; 4]);
    let (_, staged) = r.s.batch("b-2").unwrap().unwrap();
    assert_eq!(staged.value.row.state, BatchState::Staged);
    assert_eq!(r.state("sp-c"), BeadState::Certified);
    let (rc, out) = r.lc(&[
        "promote", "b-2", "--head", "H2", "--base", "B2", "--actor", "test",
    ]);
    assert_eq!(rc, 0, "{out}");
    assert_eq!(r.state("sp-c"), BeadState::InDelivery);
    let (rc, out) = r.lc(&[
        "stage",
        "b-2",
        "--repo",
        "other",
        "--head",
        "H",
        "--base",
        "B",
        "--members",
        &format!("sp-a:{T1}"),
        "--actor",
        "test",
        "--parent",
        "b-1",
    ]);
    assert_eq!(
        (rc, out.as_str()),
        (2, "stage: a batch row for b-2 already exists")
    );
}
