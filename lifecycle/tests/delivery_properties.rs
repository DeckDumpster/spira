use lifecycle::delivery::{apply, DeliveryEvent, DeliveryEventKind, DeliveryRow, Mode};
use proptest::prelude::*;

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

proptest! {
    #[test]
    fn only_queue_and_local_rows_are_cut(m in any::<u8>()) {
        let row = start(m);
        let out = apply(&row, &event_for(&row, DeliveryEventKind::Cut { batch_id: "b".into() }, "p"));
        prop_assert_eq!(out.applied, matches!(row.mode, Mode::Queue | Mode::Local));
    }
}
