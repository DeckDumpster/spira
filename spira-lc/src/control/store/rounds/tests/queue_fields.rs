use super::*;

#[test_id(t9ewbj6)]
#[test]
fn the_queues_fields_of_a_round_survive_every_machine_step_and_the_landing() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    set_round_fields(&r, serde_json::json!({ "worktree": "wt/b-1", "red": "test-a.sh" }));
    batch_event(
        &r,
        BatchState::Open,
        2,
        BatchEventKind::CiStarted { run: "r1".into() },
    );
    batch_event(&r, BatchState::CiRunning, 3, BatchEventKind::Green);
    let landed = r.s.plan_land("b-1", &fast_forward(4), AT, &r.req()).unwrap();
    r.s.run_planned(landed, &r.req(), "land").unwrap();
    let (_, round) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(round.value.row.state, BatchState::Landed);
    assert_eq!(
        round.value.round,
        [("red", "test-a.sh"), ("worktree", "wt/b-1")]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .into()
    );
}

#[test_id(tc9jv2a)]
#[test]
fn the_queues_fields_given_to_a_cut_or_a_batch_event_commit_in_that_steps_own_txn() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let key = keys::batch("spira", "b-1");
    let at_head = |r: &Rig| {
        let head = r.s.contract().head().unwrap();
        let item = r.s.contract().get(&key).unwrap().unwrap();
        (item.mod_rev.main == head, item.value.data["round"].clone())
    };
    let before = r.s.contract().head().unwrap();
    r.ok(&[
        "cut", "b-1", "--repo", "spira", "--head", "H", "--base", "B", "--members",
        &format!("sp-a:{T1}"), "--actor", "test", "--round", r#"{"worktree":"wt/b-1","actor":"q"}"#,
    ]);
    assert_eq!(r.s.contract().head().unwrap(), before + 1, "the cut and its fields are one txn");
    assert_eq!(at_head(&r), (true, serde_json::json!({"actor": "q", "worktree": "wt/b-1"})));
    let before = r.s.contract().head().unwrap();
    r.ok(&[
        "event", "batch", "b-1", "--expect", "OPEN", "--version", "1", "--actor", "test",
        "--kind", r#"{"CiStarted":{"run":"r1"}}"#, "--round", r#"{"worktree":"wt/b-1","red":"x"}"#,
    ]);
    assert_eq!(r.s.contract().head().unwrap(), before + 1, "the event and its fields are one txn");
    assert_eq!(at_head(&r), (true, serde_json::json!({"red": "x", "worktree": "wt/b-1"})));
    let (rc, out) = r.lc(&[
        "event", "batch", "b-1", "--expect", "CI_RUNNING", "--version", "2", "--actor", "test",
        "--kind", r#""Green""#, "--round", "[1]",
    ]);
    assert!(rc != 0 && out.contains("--round must be one JSON object"), "{rc}: {out}");
    assert_eq!(r.s.batch("b-1").unwrap().unwrap().1.value.row.state, BatchState::CiRunning);
}

#[test_id(tqip1m0)]
#[test]
fn fields_with_no_write_of_their_round_to_ride_in_are_an_error_not_dropped() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    let members = vec![("sp-a".to_string(), T1.to_string())];
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    let fields = [("worktree".to_string(), "wt".to_string())].into();
    let e = p.with_round("b-9", Some(&fields)).err().expect("no write of b-9");
    assert!(e.to_string().contains("b-9"), "{e}");
}
