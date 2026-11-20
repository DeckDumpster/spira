use super::*;

#[test]
fn a_round_that_opened_and_never_began_is_abandoned_and_its_members_requeued() {
    let r = Rig::new();
    open_round(&r);
    let req = r.req();
    let early =
        r.s.requeue_orphans("test", true, 1800, AT + 60, &none(), &req)
            .unwrap();
    assert!(early.phantom_batches.is_empty() && early.orphans.is_empty());
    let late =
        r.s.requeue_orphans("test", false, 1800, AT + 3600, &none(), &req)
            .unwrap();
    assert_eq!(
        (late.phantom_batches.as_slice(), late.orphans.as_slice()),
        (
            ["b-1".to_string()].as_slice(),
            ["sp-a".to_string()].as_slice()
        )
    );
    assert_eq!(
        round_state(&r),
        BatchState::Open,
        "a dry run writes nothing"
    );
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(
        (applied.abandoned, applied.requeued, applied.failed),
        (vec!["b-1".to_string()], vec!["sp-a".to_string()], vec![])
    );
    assert_eq!(round_state(&r), BatchState::Abandoned);
    assert_eq!(r.state("sp-a"), BeadState::Certified);
    let noted = r.s.history("sp-a", Some("bead")).unwrap().into_iter().any(|c| {
        c.audit.evidence.get("note").and_then(|n| n.as_str()).is_some_and(|n| n.contains("never began"))
    });
    assert!(noted, "the sweep's reason rides on the released member");
}

#[test]
fn a_round_with_an_ejected_member_and_no_phase_write_is_not_abandoned_as_never_began() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let members = two();
    let p = r.s.plan_cut(&r.cut_spec(&members)).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
    let ev = BatchEvent {
        expect: BatchState::Open,
        version: rec.value.row.version,
        kind: BatchEventKind::Eject { bead_id: "sp-a".into(), reason: "red".into() },
        actor: "test".into(),
    };
    let (p, _) = r.s.plan_eject("b-1", "sp-a", &ev, AT, &r.req()).unwrap();
    r.s.run_planned(p, &r.req(), "eject").unwrap();
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert!(
        applied.phantom_batches.is_empty() && applied.abandoned.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-b"), BeadState::InDelivery);
}

#[test]
fn a_round_with_a_recorded_pass_and_no_live_leg_is_not_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    let send = |expect: BatchState, kind: BatchEventKind| {
        let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
        let ev = BatchEvent { expect, version: rec.value.row.version, kind, actor: "test".into() };
        r.s.batch_event_with("b-1", &ev, AT + 10, &r.req(), None).unwrap();
    };
    send(BatchState::Open, BatchEventKind::PassStarted { n: 1, head: "h1".into() });
    send(BatchState::CiRunning, BatchEventKind::PassIncomplete { n: 1, reason: "lost".into() });
    send(BatchState::Attributing, BatchEventKind::PassRebuilt { head: "h2".into() });
    assert_eq!(round_state(&r), BatchState::Open);
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(applied.running, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty() && applied.abandoned.is_empty() && applied.requeued.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}


#[test]
fn a_forge_round_with_a_recorded_pr_is_begun_and_not_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    set_round_fields(&r, serde_json::json!({ "forge.phase": "armed", "pr": "351" }));
    let applied =
        r.s.requeue_orphans("test", true, 120, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(applied.running, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty() && applied.abandoned.is_empty() && applied.requeued.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}

#[test]
fn a_forge_round_still_opening_with_no_pr_is_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    set_round_fields(&r, serde_json::json!({ "forge.phase": "opening" }));
    let applied =
        r.s.requeue_orphans("test", true, 120, AT + 3600, &none(), &r.req())
            .unwrap();
    assert!(applied.running.is_empty(), "{applied:?}");
    assert_eq!(round_state(&r), BatchState::Abandoned);
}

#[test]
fn at_121_seconds_a_round_with_a_started_pass_is_left_and_one_with_none_is_abandoned() {
    let begun = Rig::new();
    open_round(&begun);
    let (_, rec) = begun.s.batch("b-1").unwrap().unwrap();
    let ev = BatchEvent {
        expect: BatchState::Open,
        version: rec.value.row.version,
        kind: BatchEventKind::PassStarted { n: 1, head: "h1".into() },
        actor: "test".into(),
    };
    begun.s.batch_event_with("b-1", &ev, AT + 10, &begun.req(), None).unwrap();
    let left = begun.s.requeue_orphans("test", true, 120, AT + 121, &none(), &begun.req()).unwrap();
    assert!(left.phantom_batches.is_empty() && left.abandoned.is_empty() && left.requeued.is_empty(), "{left:?}");
    assert_ne!(round_state(&begun), BatchState::Abandoned);

    let never = Rig::new();
    open_round(&never);
    let gone = never.s.requeue_orphans("test", true, 120, AT + 121, &none(), &never.req()).unwrap();
    assert_eq!(gone.abandoned, vec!["b-1".to_string()]);
}

fn none() -> std::collections::BTreeSet<String> {
    Default::default()
}

#[test]
fn a_round_with_a_live_leg_unit_is_begun_and_not_abandoned_as_never_began() {
    let r = Rig::new();
    open_round(&r);
    let live: std::collections::BTreeSet<String> = ["b-1".to_string()].into();
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &live, &r.req())
            .unwrap();
    assert_eq!(applied.running, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty()
            && applied.abandoned.is_empty()
            && applied.requeued.is_empty()
            && applied.orphans.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
    let dry =
        r.s.requeue_orphans("test", false, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(dry.repos.get("b-1").map(String::as_str), Some("spira"));
}

#[test]
fn the_requeue_orphans_command_leaves_the_round_it_is_told_is_live() {
    let r = Rig::new();
    open_round(&r);
    let later = |argv: &[&str]| {
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let req = r.req();
        state::run(&r.s, &args, &Call { now: AT + 3600, request: &req.id, inputs: &crate::control::claimable::FIXED })
    };
    let (rc, dry) = later(&["requeue-orphans", "--actor", "test"]);
    assert_eq!(rc, 0, "{dry}");
    assert!(dry.contains(r#""repos":{"b-1":"spira"}"#), "{dry}");
    let (rc, out) = later(&["requeue-orphans", "--actor", "test", "--apply", "--live-round", "b-1"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(r#""running":["b-1"]"#) && out.contains(r#""abandoned":[]"#), "{out}");
    assert_eq!(round_state(&r), BatchState::Open);
}

#[test]
fn the_requeue_orphans_command_abandons_a_round_with_no_driver_after_two_minutes_by_default() {
    let r = Rig::new();
    open_round(&r);
    let at = |now: i64| {
        let args: Vec<String> = ["requeue-orphans", "--actor", "test"].iter().map(|s| s.to_string()).collect();
        let req = r.req();
        state::run(&r.s, &args, &Call { now, request: &req.id, inputs: &crate::control::claimable::FIXED })
    };
    let (rc, early) = at(AT + 60);
    assert_eq!(rc, 0, "{early}");
    assert!(!early.contains(r#""b-1""#), "a minute is not yet a stall: {early}");
    let (rc, late) = at(AT + 180);
    assert_eq!(rc, 0, "{late}");
    assert!(late.contains(r#""repos":{"b-1":"spira"}"#), "{late}");
    assert_eq!(round_state(&r), BatchState::Open, "a dry run writes nothing");
    let args: Vec<String> = ["requeue-orphans", "--actor", "test", "--apply"].iter().map(|s| s.to_string()).collect();
    let req = r.req();
    let (rc, out) = state::run(&r.s, &args, &Call { now: AT + 180, request: &req.id, inputs: &crate::control::claimable::FIXED });
    assert_eq!(rc, 0, "{out}");
    assert_eq!(round_state(&r), BatchState::Abandoned);
}

#[test]
fn requeue_orphans_leaves_a_leased_round_that_never_began_open_and_lists_it_leased() {
    let r = Rig::new();
    open_round(&r);
    r.s.contract()
        .lock("round/spira/b-1", 600_000, &r.req())
        .unwrap();
    let applied =
        r.s.requeue_orphans("test", true, 1800, AT + 3600, &none(), &r.req())
            .unwrap();
    assert_eq!(applied.leased, vec!["b-1".to_string()]);
    assert!(
        applied.phantom_batches.is_empty()
            && applied.abandoned.is_empty()
            && applied.requeued.is_empty()
            && applied.orphans.is_empty(),
        "{applied:?}"
    );
    assert_eq!(round_state(&r), BatchState::Open);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}



/// law-list-reads-paginate-in-the-database: the orphan sweep reads the open rounds through the
/// state index, never the `batches/` prefix.
#[test]
fn requeue_orphans_reads_the_open_rounds_by_state_not_the_batches_prefix() {
    let r = Rig::on(Counting::default(), &clock());
    many_rounds(&r, 0, 240, &[3, 7]);
    r.certified("sp-orph", T1);
    let members = vec![("sp-orph".to_string(), T1.to_string())];
    let cut = Cut { batch_id: "b-orph", at: AT - 100_000, ..r.cut_spec(&members) };
    let p = r.s.plan_cut(&cut).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    end_round(&r, "b-orph", BatchState::Abandoned);
    r.s.contract().backend().take();
    let found = r.s.requeue_orphans("test", false, 1800, AT + 3600, &Default::default(), &r.req()).unwrap();
    let reads = r.s.contract().backend().take();
    assert_eq!(ranges_of_rounds(&reads), 0, "{reads:?}");
    assert!(reads.iter().any(|(k, _, _)| *k == "range_in_state"), "{reads:?}");
    let mut expected: Vec<String> = (0..240).map(|i| format!("sp-{i:03}")).collect();
    expected.push("sp-orph".into());
    assert_eq!(found.orphans, expected, "the answer the full scan gave");
    assert_eq!(found.phantom_batches, ["b-003", "b-007"]);
}
