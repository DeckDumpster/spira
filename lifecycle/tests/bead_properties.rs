use lifecycle::bead::{apply, BeadEvent, BeadEventKind, BeadRow, BeadState, HoldKind, Stack};
use lifecycle::reason::{DropReason, GateRedReason, HoldCause, ReturnedReason};
use lifecycle::Refusal;
use proptest::prelude::*;

fn tip() -> impl Strategy<Value = String> {
    prop_oneof![Just("t1".to_string()), Just("t2".to_string())]
}

fn kind() -> impl Strategy<Value = BeadEventKind> {
    prop_oneof![
        Just(BeadEventKind::Claim { holder: "h".into(), lease_until: 1, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4 }),
        Just(BeadEventKind::Release),
        Just(BeadEventKind::HolderDead),
        tip().prop_map(|tip| BeadEventKind::Submit { tip }),
        Just(BeadEventKind::Done { delivers: "d".into() }),
        tip().prop_map(|tip| BeadEventKind::GatePass { tip, gate_key: "k".into() }),
        tip().prop_map(|tip| BeadEventKind::GateRed { tip, reason: GateRedReason::SuitesFailed }),
        tip().prop_map(|tip| BeadEventKind::GateInfra { tip }),
        Just(BeadEventKind::Deliver),
        Just(BeadEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() }),
        Just(BeadEventKind::Returned { reason: ReturnedReason::PushRejected }),
        tip().prop_map(|tip| BeadEventKind::Requeued { tip }),
        Just(BeadEventKind::ContentOnBase { proof: "p".into() }),
        Just(BeadEventKind::Supersede { by: "sp-2".into() }),
        Just(BeadEventKind::Drop { reason: DropReason::Unwanted }),
        Just(BeadEventKind::Hold { kind: HoldKind::Poison, cause: HoldCause::AttemptsExhausted, detail: None }),
        Just(BeadEventKind::Unhold { kind: HoldKind::Poison }),
    ]
}

fn event_for(row: &BeadRow, kind: BeadEventKind) -> BeadEvent {
    BeadEvent { expect: row.state, version: row.version, kind, actor: "p".into(), at: None }
}

fn forward(row: &BeadRow) -> BeadEventKind {
    let tip = row.tip.clone().unwrap_or_else(|| "t1".into());
    match row.state {
        BeadState::Ready => BeadEventKind::Claim { holder: "h".into(), lease_until: 1, stack: Stack::new(), stack_depth: 0, stack_max_depth: 4 },
        BeadState::Working | BeadState::Rework => BeadEventKind::Submit { tip },
        BeadState::Submitted => BeadEventKind::GatePass { tip, gate_key: "k".into() },
        BeadState::Certified => BeadEventKind::Deliver,
        BeadState::InDelivery => BeadEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() },
        _ => BeadEventKind::Release,
    }
}

fn steps() -> impl Strategy<Value = Vec<(bool, BeadEventKind)>> {
    prop::collection::vec((prop::bool::weighted(0.6), kind()), 0..40)
}

fn run(steps: Vec<(bool, BeadEventKind)>) -> Vec<(BeadRow, BeadRow, bool)> {
    let mut row = BeadRow::filed("sp-prop");
    let mut trace = Vec::new();
    for (go_forward, k) in steps {
        let k = if go_forward { forward(&row) } else { k };
        let out = apply(&row, &event_for(&row, k));
        trace.push((row.clone(), out.row.clone(), out.applied));
        row = out.row;
    }
    trace
}

proptest! {
    #[test]
    fn terminal_absorbs_every_event(kinds in steps()) {
        for (before, after, applied) in run(kinds) {
            if before.state.is_terminal() {
                prop_assert!(!applied, "applied out of {:?}", before.state);
                prop_assert_eq!(&before, &after);
            }
        }
    }

    #[test]
    fn version_advances_by_one_on_apply_and_never_on_refusal(kinds in steps()) {
        let mut last = 0;
        for (before, after, applied) in run(kinds) {
            if applied {
                prop_assert_eq!(after.version, before.version + 1);
            } else {
                prop_assert_eq!(&after, &before);
            }
            prop_assert!(after.version >= last);
            last = after.version;
        }
    }

    #[test]
    fn stale_version_or_expect_never_applies(kinds in steps(), bump in 1u64..5) {
        for (before, _, _) in run(kinds.clone()) {
            for (_, k) in &kinds {
                let mut e = event_for(&before, k.clone());
                e.version += bump;
                let out = apply(&before, &e);
                prop_assert!(!out.applied);
                let named = matches!(out.refusal, Some(Refusal::StaleVersion { .. }));
                prop_assert!(named);
            }
        }
    }

    #[test]
    fn gate_verdict_on_a_foreign_tip_never_applies(kinds in steps(), foreign in "[a-z]{8}") {
        for (before, _, _) in run(kinds) {
            if before.state != BeadState::Submitted { continue; }
            for k in [
                BeadEventKind::GatePass { tip: foreign.clone(), gate_key: "k".into() },
                BeadEventKind::GateRed { tip: foreign.clone(), reason: GateRedReason::SuitesFailed },
                BeadEventKind::GateInfra { tip: foreign.clone() },
            ] {
                let out = apply(&before, &event_for(&before, k));
                prop_assert!(!out.applied);
                let named = matches!(out.refusal, Some(Refusal::TipMismatch { .. }));
                prop_assert!(named);
            }
        }
    }

    #[test]
    fn a_certified_row_always_names_the_tip_it_was_certified_on(kinds in steps()) {
        let mut row = BeadRow::filed("sp-prop");
        for (go_forward, k) in kinds {
            let k = if go_forward { forward(&row) } else { k };
            let out = apply(&row, &event_for(&row, k));
            row = out.row;
            if row.state == BeadState::Certified {
                prop_assert!(row.tip.is_some());
                prop_assert!(row.gate_key.is_some());
            }
        }
    }

    #[test]
    fn resubmitting_a_different_tip_voids_certification(foreign in "[a-z]{8}") {
        let mut row = BeadRow::filed("sp-prop");
        row.state = BeadState::Certified;
        row.tip = Some("t1".into());
        row.gate_key = Some("k".into());
        let out = apply(&row, &event_for(&row, BeadEventKind::Submit { tip: format!("x{foreign}") }));
        prop_assert!(out.applied);
        prop_assert_eq!(out.row.state, BeadState::Submitted);
        prop_assert!(out.row.gate_key.is_none());
    }
}
