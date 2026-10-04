use lifecycle::batch::{apply, BatchEvent, BatchEventKind, BatchRow, BatchState};
use lifecycle::Refusal;
use proptest::prelude::*;

fn kind() -> impl Strategy<Value = BatchEventKind> {
    prop_oneof![
        Just(BatchEventKind::MemberAdded { bead_id: "sp-1".into(), tip: "t1".into() }),
        Just(BatchEventKind::CiStarted { run: "r".into() }),
        Just(BatchEventKind::Green),
        Just(BatchEventKind::Red),
        Just(BatchEventKind::BaseMoved),
        Just(BatchEventKind::Rebuilt),
        Just(BatchEventKind::FastForward { sha: "s".into() }),
        Just(BatchEventKind::Attributed { bead_id: "sp-1".into(), outcome: "requeue".into() }),
        Just(BatchEventKind::Settle),
        Just(BatchEventKind::Abandon { reason: "r".into() }),
        Just(BatchEventKind::Eject { bead_id: "sp-1".into(), reason: "m".into() }),
    ]
}

fn event_for(row: &BatchRow, kind: BatchEventKind) -> BatchEvent {
    BatchEvent { expect: row.state, version: row.version, kind, actor: "p".into() }
}

fn forward(row: &BatchRow) -> BatchEventKind {
    match row.state {
        BatchState::Open => BatchEventKind::CiStarted { run: "r".into() },
        BatchState::CiRunning => BatchEventKind::Green,
        BatchState::Green => BatchEventKind::FastForward { sha: "s".into() },
        BatchState::Rebuilding => BatchEventKind::Rebuilt,
        BatchState::Attributing => BatchEventKind::Settle,
        _ => BatchEventKind::Settle,
    }
}

fn steps() -> impl Strategy<Value = Vec<(bool, BatchEventKind)>> {
    prop::collection::vec((prop::bool::weighted(0.6), kind()), 0..40)
}

fn run(steps: Vec<(bool, BatchEventKind)>) -> Vec<(BatchRow, BatchRow, bool)> {
    let mut row = BatchRow::cut("b-prop", "r", "h", "b");
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
    fn terminal_absorbs_every_event(ks in steps()) {
        for (before, after, applied) in run(ks) {
            if before.state.is_terminal() {
                prop_assert!(!applied);
                prop_assert_eq!(&before, &after);
            }
        }
    }

    #[test]
    fn version_advances_by_one_on_apply_and_never_on_refusal(ks in steps()) {
        for (before, after, applied) in run(ks) {
            if applied {
                prop_assert_eq!(after.version, before.version + 1);
            } else {
                prop_assert_eq!(&after, &before);
            }
        }
    }

    #[test]
    fn stale_version_never_applies(ks in steps(), bump in 1u64..5) {
        for (before, _, _) in run(ks.clone()) {
            for (_, k) in &ks {
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
    fn a_wrong_expect_never_applies(ks in steps()) {
        for (before, _, _) in run(ks.clone()) {
            for (_, k) in &ks {
                let mut e = event_for(&before, k.clone());
                e.expect = if before.state == BatchState::Open { BatchState::Green } else { BatchState::Open };
                let out = apply(&before, &e);
                prop_assert!(!out.applied);
                let named = matches!(out.refusal, Some(Refusal::ExpectMismatch { .. }));
                prop_assert!(named);
            }
        }
    }

    #[test]
    fn a_batch_reaches_landed_only_through_green(ks in steps()) {
        for (before, after, applied) in run(ks) {
            if applied && after.state == BatchState::Landed {
                prop_assert_eq!(before.state, BatchState::Green);
            }
        }
    }

    #[test]
    fn a_batch_reaches_settled_only_through_attributing(ks in steps()) {
        for (before, after, applied) in run(ks) {
            if applied && after.state == BatchState::Settled {
                prop_assert_eq!(before.state, BatchState::Attributing);
            }
        }
    }

    #[test]
    fn abandon_is_legal_from_every_live_state_and_names_its_reason(ks in steps(), why in "[a-z]{1,8}") {
        for (before, _, _) in run(ks) {
            let out = apply(&before, &event_for(&before, BatchEventKind::Abandon { reason: why.clone() }));
            if before.state.is_terminal() {
                prop_assert!(!out.applied);
            } else {
                prop_assert!(out.applied);
                prop_assert_eq!(out.row.state, BatchState::Abandoned);
                prop_assert_eq!(out.row.reason.as_deref(), Some(why.as_str()));
            }
        }
    }

    #[test]
    fn members_join_only_an_open_batch(ks in steps()) {
        for (before, _, _) in run(ks) {
            let out = apply(&before, &event_for(&before, BatchEventKind::MemberAdded { bead_id: "sp-9".into(), tip: "t".into() }));
            prop_assert_eq!(out.applied, before.state == BatchState::Open);
        }
    }

    #[test]
    fn a_landed_batch_records_the_fast_forward_sha(sha in "[a-f0-9]{8}") {
        let mut row = BatchRow::cut("b", "r", "h", "b");
        row.state = BatchState::Green;
        let out = apply(&row, &event_for(&row, BatchEventKind::FastForward { sha: sha.clone() }));
        prop_assert!(out.applied);
        prop_assert_eq!(out.row.reason, Some(sha));
    }
}
