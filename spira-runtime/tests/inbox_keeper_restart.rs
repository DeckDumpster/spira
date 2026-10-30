//! A restarted process finds the keeper's state in the journal: it must not announce a wake it
//! already made, and a changed `last_wake` or a stale `persisted_seen` is always journaled.

use std::sync::Arc;

use runtime::event::EFFECT_REQUESTED;
use runtime::journal::Journal;
use runtime::ports::{Clock, VirtualClock};
use runtime::{Engine, Executor, MemJournal, Outcome};
use spira_runtime::watchers::inbox_keeper::{Carry, InboxKeeper, OBSERVE, STATE};
use spira_runtime::watchers::CONCIERGE_WAKE;

const STALL: u64 = 600;
const START_MS: u64 = 1_800_000_000_000;

fn keeper() -> InboxKeeper {
    InboxKeeper { interval_ms: 60_000, stall_secs: STALL, backoff_secs: 3600 }
}

fn open(mem: &Arc<MemJournal>, clock: &VirtualClock) -> Engine {
    let mut e = Engine::open(mem.clone(), Arc::new(clock.clone()), vec![keeper().registration()], 0).unwrap();
    let (_, stream, kind) = InboxKeeper::boot();
    e.start(stream, kind);
    e
}

fn run(e: &mut Engine, clock: &VirtualClock, minutes: u64, snapshot: &str) {
    let until = clock.now_ms() + minutes * 60_000;
    while clock.now_ms() < until {
        e.run_until_idle().unwrap();
        for p in e.pending_effects().into_iter().filter(|p| p.spec.name == OBSERVE) {
            e.complete(&p.key, Outcome::Succeeded(snapshot.into())).unwrap();
        }
        e.run_until_idle().unwrap();
        let Some(next) = e.next_timer_at() else { break };
        clock.advance_to(next.min(until));
    }
}

fn wakes(mem: &Arc<MemJournal>) -> usize {
    mem.read_from(0, 100_000).unwrap().iter().filter(|(_, e)| e.kind.tag == EFFECT_REQUESTED && e.kind.body.contains(CONCIERGE_WAKE)).count()
}

fn states(mem: &Arc<MemJournal>) -> Vec<Carry> {
    mem.read_from(0, 100_000).unwrap().iter().map(|(_, e)| e).filter(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).collect()
}

fn waiting() -> String {
    format!(r#"{{"triage_alive":false,"inbox_mtime":{}}}"#, START_MS / 1000 + 5)
}

#[test]
fn a_restart_inside_the_backoff_does_not_announce_the_wake_again() {
    let mem = MemJournal::new();
    let clock = VirtualClock::new(START_MS);
    let mut e = open(&mem, &clock);
    run(&mut e, &clock, 15, &waiting());
    assert_eq!(wakes(&mem), 1, "the first process woke the pane");
    drop(e);

    let mut again = open(&mem, &clock);
    run(&mut again, &clock, 30, &waiting());
    assert_eq!(wakes(&mem), 1, "local ids restart at 1, the carried last_wake still holds the backoff");
}

#[test]
fn a_wake_is_journaled_with_its_last_wake_even_when_last_seen_is_fresh() {
    let mem = MemJournal::new();
    let clock = VirtualClock::new(START_MS);
    let mut e = open(&mem, &clock);
    run(&mut e, &clock, 15, &waiting());
    let saved = states(&mem);
    assert!(saved.iter().any(|c| c.last_wake > 0), "{saved:?}");
    let woke = saved.iter().find(|c| c.last_wake > 0).unwrap();
    assert_eq!(woke.persisted_seen, woke.last_seen.unwrap());
}

#[test]
fn a_stale_persisted_seen_is_refreshed_at_once_and_then_only_a_quarter_stall_later() {
    let alive = r#"{"triage_alive":true,"inbox_mtime":0}"#;
    let mem = MemJournal::new();
    let clock = VirtualClock::new(START_MS);
    let mut e = open(&mem, &clock);
    run(&mut e, &clock, 1, alive);
    let first = states(&mem);
    assert_eq!(first.len(), 1, "persisted_seen 0 is stale: the first observation saves");
    assert_eq!(first[0].persisted_seen, first[0].last_seen.unwrap());
    run(&mut e, &clock, 1, alive);
    assert_eq!(states(&mem).len(), 1, "inside a quarter-stall the journal is left alone");
    run(&mut e, &clock, 3, alive);
    let saved = states(&mem);
    assert_eq!(saved.len(), 2, "a quarter-stall on, persisted_seen is stale again");
    assert!(saved[1].persisted_seen - saved[0].persisted_seen >= STALL / 4);
}
