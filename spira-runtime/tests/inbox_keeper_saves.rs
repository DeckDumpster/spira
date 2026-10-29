//! A live monitor advances `last_seen` every tick; the journal gets it a quarter-stall at a time.

use std::sync::Arc;

use runtime::Executor;
use runtime::journal::Journal;
use runtime::ports::{Clock, VirtualClock};
use runtime::{Engine, MemJournal, Outcome};
use spira_runtime::watchers::inbox_keeper::{Carry, InboxKeeper, OBSERVE, STATE};

const STALL: u64 = 1200;

fn journaled_states(mem: &Arc<MemJournal>) -> Vec<Carry> {
    mem.read_from(0, 100_000).unwrap().iter().map(|(_, e)| e).filter(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).collect()
}

#[test]
fn a_live_monitor_is_journaled_once_per_quarter_stall_not_once_per_tick() {
    let mem = MemJournal::new();
    let clock = VirtualClock::new(1_800_000_000_000);
    let start = clock.now_ms();
    let keeper = InboxKeeper { interval_ms: 60_000, stall_secs: STALL, backoff_secs: 3600 };
    let mut e = Engine::open(mem.clone(), Arc::new(clock.clone()), vec![keeper.registration()], 0).unwrap();
    let (_, stream, kind) = InboxKeeper::boot();
    e.start(stream, kind);
    let mut ticks = 0;
    while clock.now_ms() < start + 30 * 60_000 {
        e.run_until_idle().unwrap();
        for p in e.pending_effects() {
            assert_eq!(p.spec.name, OBSERVE);
            ticks += 1;
            e.complete(&p.key, Outcome::Succeeded(r#"{"triage_alive":true,"inbox_mtime":0}"#.into())).unwrap();
        }
        e.run_until_idle().unwrap();
        let Some(next) = e.next_timer_at() else { break };
        clock.advance_to(next);
    }
    assert!(ticks >= 29, "the simulation did not run: {ticks} ticks");
    let saved = journaled_states(&mem);
    assert!((5..=8).contains(&saved.len()), "{} saves over {ticks} ticks", saved.len());
    let gaps: Vec<u64> = saved.windows(2).map(|w| w[1].last_seen.unwrap() - w[0].last_seen.unwrap()).collect();
    assert!(gaps.iter().all(|g| *g >= STALL / 4), "{gaps:?}");
    assert!(saved.iter().all(|c| c.persisted_seen == c.last_seen.unwrap()), "a saved state records itself as persisted");
}
