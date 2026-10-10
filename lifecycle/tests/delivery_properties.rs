use lifecycle::delivery::{apply, DeliveryEvent, DeliveryEventKind, DeliveryRow, DeliveryState, Exit, Mode, HARNESS_ACTOR};
use lifecycle::reason::ReturnedReason;
use lifecycle::Refusal;
use proptest::prelude::*;

fn kind() -> impl Strategy<Value = DeliveryEventKind> {
    prop_oneof![
        Just(DeliveryEventKind::Cut { batch_id: "b1".into() }),
        Just(DeliveryEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() }),
        Just(DeliveryEventKind::Returned { reason: ReturnedReason::PushRejected }),
        Just(DeliveryEventKind::Requeued { tip: "t".into() }),
        Just(DeliveryEventKind::PublishStarted { pr: 3 }),
        Just(DeliveryEventKind::PublishCi { ci: lifecycle::delivery::CiState::Green }),
        Just(DeliveryEventKind::Published { forge_sha: "f".into() }),
        Just(DeliveryEventKind::PublishRed { fix_forward: "sp-fix".into() }),
    ]
}

fn start(mode: u8) -> DeliveryRow {
    match mode % 4 {
        0 => DeliveryRow::start_queue("sp-prop"),
        1 => DeliveryRow::start_pr("sp-prop", 7),
        2 => DeliveryRow::start_push("sp-prop"),
        _ => DeliveryRow::start_local("sp-prop"),
    }
}

fn event_for(row: &DeliveryRow, kind: DeliveryEventKind, actor: &str) -> DeliveryEvent {
    DeliveryEvent { expect: row.state, version: row.version, kind, actor: actor.into() }
}

fn forward(row: &DeliveryRow) -> DeliveryEventKind {
    match row.state {
        DeliveryState::Queued => DeliveryEventKind::Cut { batch_id: "b1".into() },
        DeliveryState::Exited if row.mode == Mode::Local => DeliveryEventKind::Published { forge_sha: "f".into() },
        _ => DeliveryEventKind::Delivered { merge_sha: "s".into(), proof: "p".into() },
    }
}

fn steps() -> impl Strategy<Value = (u8, Vec<(bool, DeliveryEventKind, bool)>)> {
    (any::<u8>(), prop::collection::vec((prop::bool::weighted(0.6), kind(), any::<bool>()), 0..20))
}

fn run(mode: u8, steps: Vec<(bool, DeliveryEventKind, bool)>) -> Vec<(DeliveryRow, DeliveryRow, bool)> {
    let mut row = start(mode);
    let mut trace = Vec::new();
    for (go_forward, k, harness) in steps {
        let k = if go_forward { forward(&row) } else { k };
        let actor = if harness { HARNESS_ACTOR } else { "p" };
        let out = apply(&row, &event_for(&row, k, actor));
        trace.push((row.clone(), out.row.clone(), out.applied));
        row = out.row;
    }
    trace
}

proptest! {
    #[test]
    fn terminal_absorbs_every_event((m, ks) in steps()) {
        for (before, after, applied) in run(m, ks) {
            if before.state.is_terminal() && before.state != DeliveryState::Exited {
                prop_assert!(!applied);
                prop_assert_eq!(&before, &after);
            }
        }
    }

    #[test]
    fn version_advances_by_one_on_apply_and_never_on_refusal((m, ks) in steps()) {
        for (before, after, applied) in run(m, ks) {
            if applied {
                prop_assert_eq!(after.version, before.version + 1);
            } else {
                prop_assert_eq!(&after, &before);
            }
        }
    }

    #[test]
    fn stale_version_or_expect_never_applies((m, ks) in steps(), bump in 1u64..5) {
        for (before, _, _) in run(m, ks.clone()) {
            for (_, k, _) in &ks {
                let mut e = event_for(&before, k.clone(), HARNESS_ACTOR);
                e.version += bump;
                let out = apply(&before, &e);
                prop_assert!(!out.applied);
                let named = matches!(out.refusal, Some(Refusal::StaleVersion { .. }));
                prop_assert!(named);

                let mut e = event_for(&before, k.clone(), HARNESS_ACTOR);
                e.expect = if before.state == DeliveryState::Queued { DeliveryState::Exited } else { DeliveryState::Queued };
                let out = apply(&before, &e);
                prop_assert!(!out.applied);
                let named = matches!(out.refusal, Some(Refusal::ExpectMismatch { .. }));
                prop_assert!(named);
            }
        }
    }

    #[test]
    fn an_exit_row_names_exactly_one_exit_and_only_exited_rows_do((m, ks) in steps()) {
        for (_, after, _) in run(m, ks) {
            let exited = !matches!(after.state, DeliveryState::Queued | DeliveryState::Batched | DeliveryState::PrOpen | DeliveryState::Pushing);
            prop_assert_eq!(after.exit.is_some(), exited);
        }
    }

    #[test]
    fn a_delivered_exit_carries_the_merge_sha((m, ks) in steps()) {
        for (_, after, _) in run(m, ks) {
            if after.exit == Some(Exit::Delivered) {
                prop_assert!(after.merge_sha.is_some());
            }
        }
    }

    #[test]
    fn publish_facts_apply_only_to_an_exited_local_row((m, ks) in steps()) {
        for (before, _, _) in run(m, ks) {
            for k in [
                DeliveryEventKind::PublishStarted { pr: 3 },
                DeliveryEventKind::Published { forge_sha: "f".into() },
                DeliveryEventKind::PublishRed { fix_forward: "sp-fix".into() },
            ] {
                let out = apply(&before, &event_for(&before, k.clone(), "p"));
                let from_exited = before.state == DeliveryState::Exited && before.mode == Mode::Local;
                let from_green_publishing = before.state == DeliveryState::Publishing
                    && (matches!(k, DeliveryEventKind::PublishRed { .. }) || before.ci == Some(lifecycle::delivery::CiState::Green) && matches!(k, DeliveryEventKind::Published { .. }));
                prop_assert_eq!(out.applied, from_exited || from_green_publishing, "{:?} {:?}", before, k);
            }
        }
    }

    #[test]
    fn a_pr_row_requeues_only_for_the_harness(by in "[a-z]{1,8}") {
        prop_assume!(by != HARNESS_ACTOR);
        let row = DeliveryRow::start_pr("sp-prop", 7);
        let k = DeliveryEventKind::Requeued { tip: "t".into() };
        prop_assert!(!apply(&row, &event_for(&row, k.clone(), &by)).applied);
        prop_assert!(apply(&row, &event_for(&row, k, HARNESS_ACTOR)).applied);
    }

    #[test]
    fn only_queue_and_local_rows_are_cut(m in any::<u8>()) {
        let row = start(m);
        let out = apply(&row, &event_for(&row, DeliveryEventKind::Cut { batch_id: "b".into() }, "p"));
        prop_assert_eq!(out.applied, matches!(row.mode, Mode::Queue | Mode::Local));
    }

    #[test]
    fn a_cut_row_records_its_batch(id in "[a-z0-9]{1,8}") {
        let row = DeliveryRow::start_queue("sp-prop");
        let out = apply(&row, &event_for(&row, DeliveryEventKind::Cut { batch_id: id.clone() }, "p"));
        prop_assert!(out.applied);
        prop_assert_eq!(out.row.batch_id, Some(id));
    }
}
