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

fn eject_prerequisite(r: &Rig) -> (i32, String) {
    let (_, rec) = r.s.batch("b-1").unwrap().unwrap();
    r.lc(&["eject-member", "b-1", "--bead-id", "sp-a", "--expect", "OPEN", "--version", &rec.value.row.version.to_string(), "--actor", "concierge", "--reason", "red"])
}

fn put_range(r: &Rig, tip: &str, commits: &[&str]) {
    let commits: Vec<String> = commits.iter().map(|c| c.to_string()).collect();
    r.s.put_git_fact_range("spira", "B", tip, &commits, &r.req()).unwrap();
}

#[test]
fn ejecting_a_member_withdraws_the_member_whose_recorded_range_contains_its_own_and_runs_no_git() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let p = r.s.plan_cut(&r.cut_spec(&two())).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    put_range(&r, T1, &["c1"]);
    put_range(&r, T2, &["c1", "c2"]);
    let recorder = std::sync::Arc::new(state_store::phases::Recorder::default());
    state_store::phases::track(recorder.clone());
    let (code, out) = eject_prerequisite(&r);
    state_store::phases::untrack();
    assert_eq!(code, 0, "{out}");
    assert_eq!(recorder.snapshot().children, 0);
    assert_eq!(r.state("sp-b"), BeadState::Rework, "sp-b followed sp-a out of the round");
}

#[test]
fn ejecting_a_member_withdraws_nothing_by_containment_when_no_range_was_recorded() {
    let r = Rig::new();
    r.certified("sp-a", T1);
    r.certified("sp-b", T2);
    let p = r.s.plan_cut(&r.cut_spec(&two())).unwrap();
    r.s.run_planned(p, &r.req(), "cut").unwrap();
    let (code, out) = eject_prerequisite(&r);
    assert_eq!(code, 0, "{out}");
    assert_eq!(r.s.delivery("sp-b").unwrap().unwrap().value.state, DeliveryState::Batched);
    assert_eq!(r.state("sp-b"), BeadState::InDelivery);
}
