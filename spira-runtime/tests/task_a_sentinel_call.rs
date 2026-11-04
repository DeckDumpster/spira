//! Acceptance T1–T6 for `sentinel-call`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/sentinel-call.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` and `step` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven, with scripted dwarf `unit.ran`).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: sentinel_call,
    registration: SentinelCall::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: 60_000,
    budget: 60,
}

/// The contract's idle fixture: world running, every rune `ready=0`.
fn idle() -> Snapshot {
    Snapshot { ready_by_rune: [("builder".to_string(), Some(0)), ("ops".to_string(), Some(0))].into(), max_live: Some(4), lanes_max_live: Some(2), express_ready: Some(0), ..Snapshot::default() }
}

/// Two builder beads ready, nothing live, room under the fleet ceiling.
fn busy() -> Snapshot {
    Snapshot { ready_by_rune: [("builder".to_string(), Some(2)), ("ops".to_string(), Some(0))].into(), ..idle() }
}

fn starts(acts: &[Act]) -> usize {
    acts.iter().filter(|a| a.effect == UNIT_START).count()
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
    for (from, on, to) in LEG_TABLE {
        assert_eq!(step_leg(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_a_foreign_unit_ran_in_idle_is_ignored_and_starting_x_tick_is_refused_naming_starting() {
    assert_eq!(step(State::Idle, Trigger::UnitRanForeign), Ok(State::Idle));
    let refused = step(State::Starting, Trigger::Tick).expect_err("starting × tick is illegal");
    assert!(refused.to_lowercase().contains("starting"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(60_000).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `call::tests` (`call.rs:964`): throttled with one express bead ready and 5 free grants 1, not 5.
#[test]
fn t5_throttled_with_one_express_bead_grants_one_slot_not_the_pool() {
    let snap = Snapshot { throttle: true, express_ready: Some(1), max_live: Some(5), ready_by_rune: [("builder".to_string(), Some(5))].into(), ..idle() };
    assert_eq!(starts(&decide(&Carry::default(), &snap).1), 1);
}

/// `call_rune`'s gates (`call.rs:577-675`): a halted world, a skew, or paused capacity call nobody.
#[test]
fn t5_a_closed_gate_calls_nobody() {
    for snap in [Snapshot { world: World::Halted, ..busy() }, Snapshot { skew: Some("release skew".into()), ..busy() }, Snapshot { capacity: Capacity::Paused, ..busy() }] {
        assert_eq!(starts(&decide(&Carry::default(), &snap).1), 0, "{snap:?}");
    }
}

/// `ck7_pool` (`call.rs:981`): live at the fleet ceiling floors the pool at 0; an open fleet
/// calls the ready rune and carries each started unit in flight.
#[test]
fn t5_a_full_fleet_calls_nobody_and_an_open_one_calls_the_ready_rune() {
    let full = Snapshot { live_by_rune: [("builder".to_string(), 4)].into(), ..busy() };
    assert_eq!(starts(&decide(&Carry::default(), &full).1), 0);
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert!(starts(&acts) >= 1 && acts.iter().any(|a| a.effect == UNIT_START && a.input.contains("builder")), "{acts:?}");
    assert_eq!(carry.in_flight.len(), starts(&acts), "every started unit is carried in flight");
}
