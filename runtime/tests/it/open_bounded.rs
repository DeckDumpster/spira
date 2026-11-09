//! `Engine::open` reads what a restart needs, not the history: the rows it pulls from the store
//! are the same however long the engine ran before.

use std::sync::Arc;

use runtime::journal::{Journal, MemJournal};
use runtime::ports::VirtualClock;
use runtime::*;

struct Beat;

impl Handler for Beat {
    fn interests(&self) -> &[EventKindTag] {
        &["beat"]
    }
    fn handle(&self, _: &Ctx, _: &Projection, ev: &Event) -> Vec<Command> {
        vec![Command::Emit(EventKind::new("state", &ev.kind.body))]
    }
}

fn note(i: usize, stream: &str) -> Event {
    Event { id: EventId(format!("n{i}")), stream: StreamKey(stream.into()), kind: EventKind::new("note", ""), at: 1, cause: None, correlation: CorrelationId("c".into()) }
}

#[test]
fn what_a_restart_reads_does_not_grow_with_the_turns_before_it() {
    let j = MemJournal::new();
    let mut e = Engine::open(j.clone(), Arc::new(VirtualClock::new(0)), vec![Registration::new("beat", Beat)], 0).unwrap();
    for i in 0..2_000 {
        e.submit(&format!("b{i}"), "s", EventKind::new("beat", &i.to_string())).unwrap();
        e.run_until_idle().unwrap();
    }
    let boot = j.boot().unwrap();
    assert_eq!((boot.state.len(), boot.effects.len(), boot.schedules.len()), (1, 0, 0), "one carry row, however many turns wrote it");
    let carry = &boot.state[0];
    assert!(carry.value.contains("1999") && !carry.value.contains("\"1998\""), "and it holds the newest state only: {}", carry.value);
}

#[test]
fn trimming_a_busy_stream_never_lowers_what_the_heartbeat_counts() {
    use runtime::heartbeat::{Heartbeat, HEARTBEAT};
    let hb = Heartbeat { interval_ms: 1000, ping_deadline_ms: 500 };
    let boot = Event { id: EventId("b".into()), stream: StreamKey("hb".into()), kind: EventKind::new(HEARTBEAT, &(2 * WINDOW - 1).to_string()), at: 1, cause: None, correlation: CorrelationId("c".into()) };
    let mut p = Projection::default();
    let mut last = 0;
    for i in 0..5 * WINDOW {
        p.push(&note(i, "busy"));
        assert!(p.total() > last);
        last = p.total();
    }
    p.push(&boot);
    let cmds = hb.handle(&Ctx::new(1, 1), &p, &boot);
    assert!(cmds.iter().any(|c| matches!(c, Command::Probe(_))), "foreign events past the trim still advance the heartbeat");
}
