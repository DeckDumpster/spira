//! What a turn writes to the store: only the state keys that changed, the effects it
//! requested, the outcomes it handled and the schedule rows it made or fired; events are history
//! and never reach it. An effect key admits one request in flight.

use std::sync::Arc;

use runtime::journal::{MemJournal, CARRY_KEY};
use runtime::ports::VirtualClock;
use runtime::*;

/// On `look`, emits `state` with the body it was given, asks for `do` under the key in the body
/// when the body says to, and schedules nothing.
struct Task;
impl Handler for Task {
    fn interests(&self) -> &[EventKindTag] {
        &["look"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        let (state, ask) = ev.kind.body.split_once('|').unwrap_or((&ev.kind.body, ""));
        let mut cmds = vec![Command::Emit(EventKind::new("state", state))];
        if !ask.is_empty() {
            cmds.push(Command::Effect(EffectSpec { name: "do".into(), input: ask.into(), deadline_ms: 1000, key: format!("do/{ask}") }));
        }
        cmds
    }
}

fn open(journal: &Arc<MemJournal>) -> Engine {
    Engine::open(journal.clone(), Arc::new(VirtualClock::new(0)), vec![Registration::new("task", Task)], 1).unwrap()
}

fn look(e: &mut Engine, id: &str, body: &str) {
    e.submit(id, "s", EventKind::new("look", body)).unwrap();
    e.run_until_idle().unwrap();
}

#[test]
fn a_turn_writes_only_the_state_keys_that_changed() {
    let j = MemJournal::new();
    let mut e = open(&j);
    look(&mut e, "l1", "same");
    look(&mut e, "l2", "same");
    look(&mut e, "l3", "same");
    look(&mut e, "l4", "different");
    let commits = j.commits();
    let carries: Vec<usize> = commits.iter().map(|t| t.state.iter().filter(|s| s.skey == CARRY_KEY).count()).collect();
    assert_eq!(carries, [1, 1], "the carry is written when first set and when changed, not on the two turns that left it as it was: {commits:?}");
    assert_eq!(commits.len(), 2, "a turn that changed nothing commits nothing");
    assert!(commits[0].state.iter().all(|s| s.task == "task" && s.skey == CARRY_KEY), "{:?}", commits[0].state);
    assert!(commits[1].state[0].value.contains("different"));
    assert_eq!(j.state().len(), 1, "one row, overwritten in place");
}

#[test]
fn a_second_request_on_an_unresolved_effect_key_is_refused_and_a_resolved_one_is_not() {
    let j = MemJournal::new();
    let sink = MemSink::new();
    let mut e = open(&j).with_sink(sink.clone());
    look(&mut e, "l1", "a|k");
    look(&mut e, "l2", "b|k");
    let pending = e.pending_effects();
    assert_eq!(pending.iter().map(|p| p.spec.input.as_str()).collect::<Vec<_>>(), ["k"], "one request in flight for the key");
    assert_eq!(j.history().iter().filter(|ev| ev.kind.tag == EFFECT_REQUESTED).count(), 1, "the second was never requested");
    assert!(sink.records().iter().any(|r| r.get("unit") == Some(&Value::from("effect.refused")) && r.get("effect.key") == Some(&Value::from("do/k"))), "and the refusal is reported");
    look(&mut e, "l3", "c|other");
    assert_eq!(e.pending_effects().len(), 2, "another key is not held up");

    e.complete(&pending[0].key, Outcome::Succeeded("done".into())).unwrap();
    e.run_until_idle().unwrap();
    look(&mut e, "l4", "d|k");
    assert!(e.pending_effects().iter().any(|p| p.spec.key == "do/k"), "once resolved, the key is free again");
}

#[test]
fn an_effect_is_offered_to_an_executor_only_after_its_row_commits_and_resolved_with_its_outcome() {
    let j = MemJournal::new();
    let faulty = FaultyJournal::new(j.clone());
    let mut e = Engine::open(faulty.clone(), Arc::new(VirtualClock::new(0)), vec![Registration::new("task", Task)], 1).unwrap();
    e.submit("l1", "s", EventKind::new("look", "a|k")).unwrap();
    faulty.cannot_tell_next("commit");
    assert!(e.run_until_idle().is_err());
    assert!(e.pending_effects().is_empty(), "the row did not commit, so nothing may run");
    e.run_until_idle().unwrap();
    let pending = e.pending_effects();
    assert_eq!(pending.len(), 1, "committed on the retry, offered once");
    assert_eq!(j.boot().unwrap().effects.len(), 1);
    e.complete(&pending[0].key, Outcome::Succeeded("ok".into())).unwrap();
    e.run_until_idle().unwrap();
    assert!(j.boot().unwrap().effects.is_empty(), "the outcome's turn resolved the row by its request id");
    assert!(open(&j).pending_effects().is_empty());
}

#[test]
fn a_restart_finds_the_carry_and_the_halt() {
    let j = MemJournal::new();
    let mut e = open(&j);
    e.halt_at_boot("test").unwrap();
    look(&mut e, "l1", "kept");
    drop(e);
    let again = open(&j);
    assert!(again.is_halted(), "the halt is a key of the scheduler task");
    let seen = again.projection().events(&StreamKey("s".into())).iter().filter(|ev| ev.kind.tag == "state").map(|ev| ev.kind.body.clone()).collect::<Vec<_>>();
    assert_eq!(seen, ["kept"], "the task's carry is back in its view");
}
