use super::*;

#[test]
fn eject_member_of_a_round_whose_pass_is_running_refuses_with_its_exit_and_changes_nothing() {
    let r = Rig::new();
    open_round(&r);
    let send = |expect: BatchState, kind: BatchEventKind| {
        let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
        let ev = BatchEvent { expect, version: rec.value.row.version, kind, actor: "test".into() };
        r.s.batch_event_with("b-1", &ev, AT + 10, &r.req(), None).unwrap();
    };
    send(BatchState::Open, BatchEventKind::PassStarted { n: 1, head: "h1".into() });
    let (_, before) = r.s.batch("b-1").unwrap().unwrap();
    let ev = BatchEvent {
        expect: BatchState::CiRunning,
        version: before.value.row.version,
        kind: BatchEventKind::Eject { bead_id: "sp-a".into(), reason: "red".into() },
        actor: "concierge".into(),
    };
    let (p, _) = r.s.plan_eject("b-1", "sp-a", &ev, AT + 20, &r.req()).unwrap();
    let Planned::Ran(Ran::Refused { refusal: lifecycle::Refusal::PassRunning { exit, .. }, .. }) = p else {
        panic!("a running pass refuses the eject");
    };
    assert!(exit.contains("round abandon") && exit.contains("hold"), "{exit}");
    let (_, after) = r.s.batch("b-1").unwrap().unwrap();
    assert_eq!(after.value.row, before.value.row);
    assert_eq!(r.state("sp-a"), BeadState::InDelivery);
}
