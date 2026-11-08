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

/// A dwarf call starts under `call_argv`'s properties: the rune's timeout as `TimeoutStartSec`,
/// the idle IO class, IO weight 10, and no `ExecStopPost` re-exec.
#[test]
fn t5_a_dwarf_call_carries_call_argvs_properties() {
    let p = dwarf_props("3600").unwrap();
    assert_eq!((p.start_timeout_ms, p.io_class.as_deref(), p.io_weight), (Some(3_600_000), Some("idle"), Some(10)));
    assert!(p.exec_stop_post.is_empty(), "the --call-only re-exec is retired");
    assert!(dwarf_props("soon").is_err(), "a timeout that is not seconds refuses");
}

// ---------------------------------------------------------------------------------------------
// sp-bpnmzg: the fleet ceiling counts every dwarf the scheduler starts.

use std::sync::Arc;

use runtime::ports::{Clock, VirtualClock};
use runtime::units::{self, Exit, ScriptedUnits, StartSpec, UnitFacts, UnitSpec, UnitState, Units};

/// A unit manager whose `sentinel-call` units run an hour, on a virtual clock at `now`.
fn manager(now: u64) -> (Arc<VirtualClock>, ScriptedUnits) {
    let clock = Arc::new(VirtualClock::new(now));
    let u = ScriptedUnits::new(clock.clone());
    u.script(NAME, 3_600_000, Exit::Code(0));
    (clock, u)
}

/// Each `unit_start` act started as `unit_start` starts it: a `unit.start` of the act's key
/// under this task, with the dwarf's properties.
fn start_all(u: &ScriptedUnits, acts: &[Act]) -> usize {
    let mut n = 0;
    for a in acts.iter().filter(|a| a.effect == UNIT_START) {
        let start: Start = serde_json::from_str(&a.input).unwrap();
        let unit = UnitSpec { task: NAME.into(), key: start.key, argv: vec!["/rel/bin/dwarf".into(), start.rune], env: vec![], cwd: None };
        let spec = StartSpec { unit, props: dwarf_props("3600").unwrap() };
        units::effect(u, units::UNIT_START, &serde_json::to_string(&spec).unwrap()).unwrap();
        n += 1;
    }
    n
}

/// Ten builder beads ready, a builder may run ten at once, and the fleet ceiling is 4.
fn crowded(f: &Fleet, now_ms: u64) -> Snapshot {
    Snapshot {
        ready_by_rune: [("builder".to_string(), Some(10))].into(),
        runes: [("builder".to_string(), RuneCfg { max_concurrent: 10, ..RuneCfg::default() })].into(),
        roster: vec!["builder".into()],
        max_live: Some(4),
        live_by_rune: f.live_by_rune.clone(),
        other_live: f.other_live,
        live_units: f.live_units.clone(),
        now_ms,
        ..idle()
    }
}

/// The bead's first acceptance: with 4 dwarves started by `unit_start` live, the next tick
/// launches none. Ticks a minute apart, each reading the fleet back from the unit manager.
#[test]
fn sp_bpnmzg_four_live_dwarves_started_by_unit_start_hold_the_next_tick_at_none() {
    let (clock, u) = manager(1_000_000);
    let roster = vec!["builder".to_string()];
    let (mut carry, mut started, mut last) = (Carry::default(), 0, usize::MAX);
    for _ in 0..4 {
        clock.advance(60_000);
        let f = fleet(&u, &roster, &|r: &str| r.to_string()).unwrap();
        let (next, acts) = decide(&carry, &crowded(&f, clock.now_ms()));
        last = start_all(&u, &acts);
        started += last;
        carry = next;
    }
    let f = fleet(&u, &roster, &|r: &str| r.to_string()).unwrap();
    assert_eq!(f.total(), 4, "the fleet read back counts the dwarves unit_start started: {f:?}; names {:?}", u.list("*").unwrap().iter().map(|d| d.name.clone()).collect::<Vec<_>>());
    assert_eq!((started, last), (4, 0), "4 started under a ceiling of 4, and the tick after launches none");
}

/// The bead's second acceptance: dwarves.sh's count and sentinel-call's count agree on the
/// same unit list — dwarves started by the scheduler and by name, live and ended, beside units
/// that are no dwarf at all.
#[test]
fn sp_bpnmzg_dwarves_sh_and_sentinel_call_count_the_same_units() {
    let (_clock, u) = manager(1_000_000);
    for key in ["builder-1000-0", "builder-1000-1", "ops-1000-0"] {
        let unit = UnitSpec { task: NAME.into(), key: key.into(), argv: vec!["/rel/bin/dwarf".into()], env: vec![], cwd: None };
        u.start(&unit).unwrap();
    }
    let facts = |state| UnitFacts { state, main_pid: Some(7), exit: None, since_ms: Some(1) };
    u.install("spira-dwarf-builder-1794006550.service", facts(UnitState::Active));
    u.install("spira-dwarf-czar-escape-1794006551.service", facts(UnitState::Activating));
    u.install("spira-dwarf-builder-1794000000.service", facts(UnitState::Failed));
    u.install(&units::unit_name(NAME, "builder-900-0").unwrap(), facts(UnitState::Inactive));
    u.install(&units::unit_name("czar-pass", "r7").unwrap(), facts(UnitState::Active));
    u.install("spira-runtime.service", facts(UnitState::Active));

    assert_eq!(NAME, units::DWARF_TASK, "the dwarf set names this task's units");
    let roster = vec!["builder".to_string(), "ops".to_string()];
    let call = fleet(&u, &roster, &|r: &str| r.to_string()).unwrap().total() as usize;

    // What `systemctl --user list-units <globs> --no-legend` prints for the same units.
    let word = |s: UnitState| match s {
        UnitState::Active => "active running",
        UnitState::Activating => "activating start",
        UnitState::Deactivating => "deactivating stop",
        UnitState::Failed => "failed failed",
        _ => "inactive dead",
    };
    let globs = spira_world::sysctl::dwarf_globs();
    let lines: Vec<String> = u.list("*").unwrap().into_iter().filter(|d| globs.iter().any(|g| units::glob_match(g, &d.name))).map(|d| format!("{} loaded {} dwarf", d.name, word(d.state))).collect();
    let sh = spira_world::sysctl::count_live_dwarves(&lines);

    assert_eq!((sh, call), (5, 5), "dwarves.sh {sh} and sentinel-call {call} must both count the 5 live dwarves (3 scheduler-started, 2 by name)");
}

/// A dwarf summoned onto a named bead runs outside the caps: the fleet the scheduler reads back
/// leaves it out, so a full fleet still reads full and an empty one still reads empty.
#[test]
fn sp_8mbnup_a_summoned_dwarf_takes_no_slot_in_the_fleet_ceiling() {
    let (_clock, u) = manager(1_000_000);
    let facts = |state| UnitFacts { state, main_pid: Some(7), exit: None, since_ms: Some(1) };
    let roster = vec!["builder".to_string()];
    u.install("spira-dwarf-builder-1794006550.service", facts(UnitState::Active));
    let before = fleet(&u, &roster, &|r: &str| r.to_string()).unwrap().total();
    u.install(&format!("{}.service", units::summoned_unit_stem("builder", 1794006551)), facts(UnitState::Active));
    let after = fleet(&u, &roster, &|r: &str| r.to_string()).unwrap();
    assert_eq!((before, after.total()), (1, 1), "one ordinary dwarf counts, the summoned one beside it does not: {after:?}");
}
