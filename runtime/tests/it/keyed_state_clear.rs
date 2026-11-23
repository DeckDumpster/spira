//! A keyed task's state key its carry no longer holds is deleted from the store, not kept as an
//! empty row; rows an earlier release left empty are deleted by the first turn after boot.

use std::collections::BTreeMap;
use std::sync::Arc;

use runtime::event::RowCodec;
use runtime::journal::{MemJournal, StateRow, Turn};
use runtime::ports::VirtualClock;
use runtime::*;

struct Task;
impl Handler for Task {
    fn interests(&self) -> &[EventKindTag] {
        &["look"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        vec![Command::Emit(EventKind::new("state", &ev.kind.body))]
    }
}

fn codec() -> RowCodec {
    RowCodec {
        stream: "s",
        also: &[],
        tag: "state",
        split: |b| serde_json::from_str::<BTreeMap<String, String>>(b).unwrap_or_default(),
        join: |r| serde_json::to_string(r).unwrap(),
    }
}

fn open(journal: &Arc<MemJournal>) -> Engine {
    Engine::open(journal.clone(), Arc::new(VirtualClock::new(0)), vec![Registration::new("task", Task).with_rows(codec())], 1).unwrap()
}

fn look(e: &mut Engine, id: &str, keys: impl Iterator<Item = usize>) {
    let body: BTreeMap<String, String> = keys.map(|i| (format!("k{i:03}"), format!("v{i}"))).collect();
    e.submit(id, "s", EventKind::new("look", &serde_json::to_string(&body).unwrap())).unwrap();
    e.run_until_idle().unwrap();
}

fn task_keys(j: &MemJournal) -> Vec<(String, String)> {
    j.state().into_iter().filter(|((t, _), _)| t == "task").map(|((_, k), v)| (k, v)).collect()
}

fn restored(e: &Engine) -> String {
    format!("{:?}", e.projection())
}

#[test_id(tmkcrx8)]
#[test]
fn a_key_the_carry_drops_is_deleted_from_the_store() {
    let j = MemJournal::new();
    let mut e = open(&j);
    look(&mut e, "l1", 0..100);
    assert_eq!(task_keys(&j).len(), 100);
    look(&mut e, "l2", 0..10);
    let left = task_keys(&j);
    assert_eq!(left.len(), 10);
    assert!(left.iter().all(|(_, v)| !v.is_empty()));
    let cleared = j.commits().last().unwrap().cleared.clone();
    assert_eq!(cleared.len(), 90);
    assert!(cleared.contains(&("task".to_string(), "k099".to_string())));
}

#[test_id(t643uct)]
#[test]
fn a_key_dropped_and_set_again_in_one_turn_is_not_deleted() {
    let j = MemJournal::new();
    let mut e = open(&j);
    look(&mut e, "l1", 0..3);
    look(&mut e, "l2", 0..2);
    look(&mut e, "l3", 0..3);
    assert_eq!(task_keys(&j).len(), 3);
}

#[test_id(ts9wlv1)]
#[test]
fn rows_left_empty_by_an_earlier_release_are_deleted_by_the_first_turn_after_boot() {
    let j = MemJournal::new();
    let row = |i: usize, v: &str| StateRow { task: "task".into(), skey: format!("k{i:03}"), value: v.into(), at: 1 };
    let rows: Vec<StateRow> = (0..50).map(|i| row(i, "")).chain((50..53).map(|i| row(i, &format!("v{i}")))).collect();
    use runtime::journal::Journal;
    j.commit(&Turn { state: rows, ..Turn::default() }).unwrap();
    let mut e = open(&j);
    look(&mut e, "l1", 50..53);
    let after = task_keys(&j);
    assert_eq!(after.len(), 3, "{after:?}");
    assert_eq!(j.commits()[1].cleared.len(), 50);
    drop(e);
    let plain = MemJournal::new();
    plain.commit(&Turn { state: (50..53).map(|i| row(i, &format!("v{i}"))).collect(), ..Turn::default() }).unwrap();
    assert_eq!(restored(&open(&j)), restored(&open(&plain)), "the restored carry is what a store without the empty rows gives");
    let before = j.commits().len();
    let mut e = open(&j);
    look(&mut e, "l2", 50..53);
    assert_eq!(j.commits().len(), before, "the restored carry is the same, so nothing more is written");
}

fn seeded(j: &Arc<MemJournal>) {
    use runtime::journal::Journal;
    let state = (0..2).map(|i| StateRow { task: "task".into(), skey: format!("k{i}"), value: format!("v{i}"), at: 1 }).collect();
    j.commit(&Turn { state, ..Turn::default() }).unwrap();
}

#[test_id(tn8ph2w)]
#[test]
fn rows_a_codec_keeps_none_of_are_deleted_by_the_first_turn_and_restore_nothing() {
    struct Silent;
    impl Handler for Silent {
        fn interests(&self) -> &[EventKindTag] {
            &["look"]
        }
        fn handle(&self, _: &Ctx, _: &Projection, _: &Event) -> Vec<Command> {
            vec![]
        }
    }
    let none = || RowCodec { stream: "s", also: &[], tag: "state", split: |b| runtime::rows::split(b, &[]), join: |r| runtime::rows::join(r, &[]) };
    let open_none = |j: &Arc<MemJournal>| Engine::open(j.clone(), Arc::new(VirtualClock::new(0)), vec![Registration::new("task", Silent).with_rows(none())], 1).unwrap();
    let j = MemJournal::new();
    seeded(&j);
    let mut e = open_none(&j);
    assert_eq!(restored(&e), restored(&open_none(&MemJournal::new())));
    e.run_until_idle().unwrap();
    assert!(task_keys(&j).is_empty(), "{:?}", task_keys(&j));
    let cleared: Vec<_> = j.commits().iter().flat_map(|c| c.cleared.clone()).collect();
    assert_eq!(cleared.len(), 2);
}

#[test_id(t1p5u21)]
#[test]
fn rows_a_codec_names_are_kept_when_nothing_happens() {
    let j = MemJournal::new();
    seeded(&j);
    let before = j.commits().len();
    let mut e = open(&j);
    e.run_until_idle().unwrap();
    assert_eq!(j.commits().len(), before);
    assert_eq!(task_keys(&j).len(), 2);
}
