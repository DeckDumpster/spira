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
    test_ids: [ #[test_id(thwpsj2)] #[test_id(t0ym2d8)] #[test_id(t90bmai)] #[test_id(taxxc64)] #[test_id(tafwsk4)] ],
}

/// The contract's idle fixture: world running, every rune `ready=0`.
fn idle() -> Snapshot {
    Snapshot { ready_by_rune: [("builder".to_string(), Some(0)), ("ops".to_string(), Some(0))].into(), max_live: Some(4), express_ready: Some(0), ..Snapshot::default() }
}

/// Two builder beads ready, nothing live, room under the fleet ceiling.
fn busy() -> Snapshot {
    Snapshot { ready_by_rune: [("builder".to_string(), Some(2)), ("ops".to_string(), Some(0))].into(), ..idle() }
}

fn starts(acts: &[Act]) -> usize {
    acts.iter().filter(|a| a.effect == UNIT_START).count()
}

#[test_id(tfhk46h)]
#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
    for (from, on, to) in LEG_TABLE {
        assert_eq!(step_leg(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test_id(tk3pr0s)]
#[test]
fn t3_a_foreign_unit_ran_in_idle_is_ignored_and_starting_x_tick_is_refused_naming_starting() {
    assert_eq!(step(State::Idle, Trigger::UnitRanForeign), Ok(State::Idle));
    let refused = step(State::Starting, Trigger::Tick).expect_err("starting × tick is illegal");
    assert!(refused.to_lowercase().contains("starting"), "{refused}");
}

#[test_id(tfw6apz)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(60_000).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `call::tests` (`call.rs:964`): throttled with one express bead ready and 5 free grants 1, not 5.
#[test_id(tdq968r)]
#[test]
fn t5_throttled_with_one_express_bead_grants_one_slot_not_the_pool() {
    let snap = Snapshot { throttle: true, express_ready: Some(1), max_live: Some(5), ready_by_rune: [("builder".to_string(), Some(5))].into(), ..idle() };
    assert_eq!(starts(&decide(&Carry::default(), &snap).1), 1);
}

/// `call_rune`'s gates (`call.rs:577-675`): a halted world, a skew, or paused capacity call nobody.
#[test_id(t1a1kca)]
#[test]
fn t5_a_closed_gate_calls_nobody() {
    for snap in [Snapshot { world: World::Halted, ..busy() }, Snapshot { skew: Some("release skew".into()), ..busy() }, Snapshot { capacity: Capacity::Paused, ..busy() }] {
        assert_eq!(starts(&decide(&Carry::default(), &snap).1), 0, "{snap:?}");
    }
}

/// `ck7_pool` (`call.rs:981`): live at the fleet ceiling floors the pool at 0; an open fleet
/// calls the ready rune and carries each started unit in flight.
#[test_id(tyr6eyx)]
#[test]
fn t5_a_full_fleet_calls_nobody_and_an_open_one_calls_the_ready_rune() {
    let full = Snapshot { live_by_rune: [("builder".to_string(), 4)].into(), ..busy() };
    assert_eq!(starts(&decide(&Carry::default(), &full).1), 0);
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert!(starts(&acts) >= 1 && acts.iter().any(|a| a.effect == UNIT_START && a.input.contains("builder")), "{acts:?}");
    assert_eq!(carry.in_flight.len(), starts(&acts), "every started unit is carried in flight");
}

/// A dwarf call starts under `call_argv`'s properties: the rune's timeout as `TimeoutStartSec`,
/// no IO class or weight, and no `ExecStopPost` re-exec.
#[test_id(tqbn2oy)]
#[test]
fn t5_a_dwarf_call_carries_call_argvs_properties() {
    let p = runtime::units::dwarf_props("3600").unwrap();
    assert_eq!(p, runtime::units::UnitProps { start_timeout_ms: Some(3_600_000), ..Default::default() });
    assert!(p.exec_stop_post.is_empty(), "the --call-only re-exec is retired");
    assert!(runtime::units::dwarf_props("soon").is_err(), "a timeout that is not seconds refuses");
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
        let unit = UnitSpec { task: NAME.into(), key: start.key, argv: vec!["rel/bin/dwarf".into(), start.rune], env: vec![], cwd: None };
        let spec = StartSpec { unit, props: runtime::units::dwarf_props("3600").unwrap() };
        units::effect(u, units::UNIT_START, &serde_json::to_string(&spec).unwrap()).unwrap();
        n += 1;
    }
    n
}

/// Ten builder beads ready, a builder is not single-flight, and the fleet ceiling is 4.
fn crowded(f: &Fleet, now_ms: u64) -> Snapshot {
    Snapshot {
        ready_by_rune: [("builder".to_string(), Some(10))].into(),
        runes: [("builder".to_string(), RuneCfg::default())].into(),
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
#[test_id(tu3ry36)]
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
#[test_id(trd0ueu)]
#[test]
fn sp_bpnmzg_dwarves_sh_and_sentinel_call_count_the_same_units() {
    let (_clock, u) = manager(1_000_000);
    for key in ["builder-1000-0", "builder-1000-1", "ops-1000-0"] {
        let unit = UnitSpec { task: NAME.into(), key: key.into(), argv: vec!["rel/bin/dwarf".into()], env: vec![], cwd: None };
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

/// A dwarf directed onto a named bead holds a slot while it lives: four pooled builders and one
/// directed one fill a ceiling of five, so the scheduler calls nobody, and the slot returns when it exits.
#[test_id(t3clhmx)]
#[test]
fn sp_ljgmia_a_directed_dwarf_takes_a_slot_in_the_fleet_ceiling() {
    let (_clock, u) = manager(1_000_000);
    let facts = |state| UnitFacts { state, main_pid: Some(7), exit: None, since_ms: Some(1) };
    let roster = vec!["builder".to_string()];
    for i in 0..4 {
        u.install(&format!("spira-dwarf-builder-{}.service", 1794006550 + i), facts(UnitState::Active));
    }
    let directed = format!("{}.service", units::directed_unit_stem("builder", 1794006560));
    u.install(&directed, facts(UnitState::Active));
    let call = |u: &ScriptedUnits| {
        let f = fleet(u, &roster, &|r: &str| r.to_string()).unwrap();
        let snap = Snapshot { live_by_rune: f.live_by_rune.clone(), max_live: Some(5), ..busy() };
        (f.total(), starts(&decide(&Carry::default(), &snap).1))
    };
    assert_eq!(call(&u), (5, 0), "four pooled and one directed fill a ceiling of five");
    assert_eq!(fleet(&u, &roster, &|r: &str| r.to_string()).unwrap().live_by_rune.get("builder"), Some(&5), "the directed unit is attributed to its rune");

    u.install(&directed, facts(UnitState::Inactive));
    assert_eq!(call(&u).0, 4, "the slot comes back when the directed dwarf exits");
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(60_000).unwrap();
    r
}

#[test_id(tmoblyk)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    kit::assert_kept_as_rows(&at_work(), NAME);
}

#[test_id(tfroq15)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(t6bjbn9)]
#[test]
fn a_started_builders_metric_rides_as_a_probe_and_is_not_journaled() {
    let r = at_work();
    let metrics: Vec<_> = r.requested().iter().filter(|q| q.name == METRIC).collect();
    assert!(!metrics.is_empty(), "no unit start was answered: {:?}", r.requested());
    assert!(metrics.iter().all(|q| !q.journaled), "{metrics:?}");
    assert!(r.requested().iter().any(|q| q.name == UNIT_START && q.journaled), "the start itself stays journaled");
}

// ---------------------------------------------------------------------------------------------
// sp-kyniae: the carry says since when a task slot has been open.

#[test_id(t9m8uqx)]
#[test]
fn the_carry_keeps_room_since_across_passes_with_room_and_a_full_pass_clears_it() {
    let open = |now_ms| Snapshot { now_ms, ..busy() };
    let (first, _) = decide(&Carry::default(), &open(1_000));
    assert_eq!((first.room_since_ms, first.room_at_ms), (Some(1_000), Some(1_000)));
    let (second, _) = decide(&first, &open(61_000));
    assert_eq!((second.room_since_ms, second.room_at_ms), (Some(1_000), Some(61_000)));
    let full = Snapshot { now_ms: 121_000, live_by_rune: [("builder".to_string(), 4)].into(), ..busy() };
    let (third, _) = decide(&second, &full);
    assert_eq!((third.room_since_ms, third.room_at_ms), (None, Some(121_000)));
}

// ---------------------------------------------------------------------------------------------
// sp-yui1x5: a session is held only by the bound classes its rune declares.

/// Each named rune's call settings parsed from the checkout's rune files. Parsed by path, never
/// through `runes_dir`: that resolves `SPIRA_TOML`, which a sibling test in this process may have
/// pointed at the complete fixture, whose runes directory does not exist.
fn shipped_runes(names: &[&str]) -> BTreeMap<String, RuneCfg> {
    let dir = testkit::manifest_dir().join("../spira/runes");
    names
        .iter()
        .map(|n| {
            let f = dir.join(format!("{n}.rune"));
            let text = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            let vars = spira_config::rune_file::parse(&f.to_string_lossy(), &text, &|_| None).unwrap();
            let get = |k: &str| vars.get(k).cloned().unwrap_or_default();
            let uses: Vec<String> = get("RUNE_USES").split_whitespace().map(str::to_string).collect();
            assert!(uses.iter().all(|u| spira_config::runes::RESOURCE_CLASSES.contains(&u.as_str())), "{n}: {uses:?}");
            let cfg = RuneCfg { single_flight: get("RUNE_SINGLE_FLIGHT") == "1", elastic: get("RUNE_ELASTIC") == "1", uses, reactive: get("RUNE_REACTIVE") == "1" };
            (n.to_string(), cfg)
        })
        .collect()
}

#[test_id(tovcn0n)]
#[test]
fn skuld_starts_while_the_build_disk_class_is_full() {
    let roster = ["builder", "mender", "ops", "skuld"];
    let snap = Snapshot {
        roster: roster.iter().map(|n| n.to_string()).collect(),
        runes: shipped_runes(&roster),
        live_by_rune: [("builder".to_string(), 6), ("mender".to_string(), 1), ("ops".to_string(), 1)].into(),
        ready_by_rune: [("builder".to_string(), Some(4)), ("mender".to_string(), Some(0)), ("ops".to_string(), Some(0)), ("skuld".to_string(), Some(1))].into(),
        max_dwarves: Some(7),
        max_live: Some(40),
        urgent: Some(vec![]),
        express_ready: Some(0),
        ..Snapshot::default()
    };
    let mut r = rig(1, snap);
    r.advance(60_000).unwrap();
    let started: std::collections::BTreeSet<String> = r.requested().iter().filter(|q| q.name == UNIT_START).map(|q| q.input.clone()).collect();
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(started.iter().all(|s| s.contains("\"rune\":\"skuld\"")), "{started:?}");
}

#[test_id(te8vh1i)]
#[test]
fn sp_lhumql_a_p0_behind_a_full_pool_does_not_hold_skuld() {
    let roster = ["builder", "mender", "ops", "skuld"];
    let snap = Snapshot {
        roster: roster.iter().map(|n| n.to_string()).collect(),
        runes: shipped_runes(&roster),
        live_by_rune: [("builder".to_string(), 5)].into(),
        ready_by_rune: [("builder".to_string(), Some(2)), ("mender".to_string(), Some(1)), ("ops".to_string(), Some(0)), ("skuld".to_string(), Some(2))].into(),
        max_dwarves: Some(5),
        max_live: Some(40),
        urgent: Some(vec![UrgentBead { id: "sp-7nj5be".into(), p0: true, express: false }]),
        express_ready: Some(0),
        ..Snapshot::default()
    };
    let mut r = rig(1, snap);
    r.advance(60_000).unwrap();
    let started: Vec<String> = r.requested().iter().filter(|q| q.name == UNIT_START).map(|q| q.input.clone()).collect();
    assert_eq!(started.len(), 2, "{started:?}");
    assert!(started.iter().all(|s| s.contains("\"rune\":\"skuld\"")), "{started:?}");
}

#[test_id(tzn0jyu)]
#[test]
fn call_world_binds_only_the_classes_that_have_a_limit() {
    let snap = Snapshot { max_dwarves: Some(7), class_limits: [("memory".to_string(), 3)].into(), ..idle() };
    let w = call_world(&snap);
    assert_eq!(w.limits, BTreeMap::from([("build-disk".to_string(), 7), ("memory".to_string(), 3)]));
    let unbound = call_world(&idle());
    assert!(unbound.limits.is_empty());
}

#[test_id(theiqxr)]
#[test]
fn a_decided_pass_carries_each_bound_class_as_it_left_it_and_the_carry_round_trips() {
    let snap = Snapshot { max_dwarves: Some(7), class_limits: [("memory".to_string(), 3)].into(), ..idle() };
    let (carry, _) = decide(&Carry::default(), &snap);
    let bound = carry.bound.clone().expect("a decided pass carries its bound classes");
    assert_eq!(bound.iter().map(|b| (b.class.as_str(), b.limit)).collect::<Vec<_>>(), [("build-disk", 7), ("memory", 3)]);
    let back: Carry = serde_json::from_str(&serde_json::to_string(&carry).unwrap()).unwrap();
    assert_eq!(back.bound, carry.bound);
    let limit = carry.task_limit.clone().expect("a decided pass carries the limit that bound task work");
    assert_eq!((limit.name.as_str(), limit.limit), ("ceiling", 4));
    assert_eq!(back.task_limit, carry.task_limit);
    assert_eq!(Carry::default().bound, None, "no pass yet is not an empty array");
}

#[test_id(thre9c0)]
#[test]
fn a_decided_pass_carries_why_each_ready_rune_got_no_call_and_the_carry_round_trips() {
    let full = Snapshot { live_by_rune: [("builder".to_string(), 4)].into(), ..busy() };
    let (carry, _) = decide(&Carry::default(), &full);
    let uncalled = carry.uncalled.clone().expect("a decided pass carries its uncalled runes");
    assert_eq!(uncalled.keys().collect::<Vec<_>>(), ["builder"], "ops has nothing ready: {uncalled:?}");
    assert!(uncalled["builder"].contains("4/4 dwarf(s) live across the whole fleet"), "{uncalled:?}");
    let back: Carry = serde_json::from_str(&serde_json::to_string(&carry).unwrap()).unwrap();
    assert_eq!(back.uncalled, carry.uncalled);
    assert_eq!(Carry::default().uncalled, None, "no pass yet is not an empty map");
}

fn unit_start_with_pause(until_offset: i64) -> Result<String, String> {
    let run = testkit::TempDir::new("sentinel-call-pause-run");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    std::fs::write(run.path().join("capacity-pause"), format!("{} iso why\n", now + until_offset)).unwrap();
    let e = spira_runtime::Effects { world: spira_runtime::world::World { run_dir: run.path().to_path_buf(), ..Default::default() }, ..Default::default() };
    let input = serde_json::to_string(&Start { rune: "builder".into(), key: "builder-1-0".into(), require_express: false }).unwrap();
    unit_start(&e, &runtime::event::EffectSpec::keyed(UNIT_START, "k", input, 10_000))
}

const CAPACITY_REFUSAL: &str = "the account's capacity is paused or unreadable — not calling";

#[test_id(thsa20x)]
#[test]
fn unit_start_does_not_refuse_on_a_capacity_pause_whose_time_has_passed() {
    let err = unit_start_with_pause(-60).unwrap_err();
    assert!(!err.contains(CAPACITY_REFUSAL), "{err}");
}

#[test_id(ttutwus)]
#[test]
fn unit_start_still_refuses_on_a_capacity_pause_in_the_future() {
    let err = unit_start_with_pause(3600).unwrap_err();
    assert!(err.contains(CAPACITY_REFUSAL), "{err}");
}

#[test_id(tb6jk6v)]
#[test]
fn a_unit_start_lost_at_a_restart_is_not_counted_as_a_failed_start() {
    let mut r = TaskRig::new(1, registration()).unwrap().hanging(UNIT_START).double(OBSERVE, |_| Ok(serde_json::to_string(&busy()).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert!(r.requested().iter().any(|q| q.name == UNIT_START), "{:?}", r.requested());
    r.reopen(registration()).unwrap();
    r.run_until_idle().unwrap();
    assert!(!r.requested().iter().any(|q| q.name == METRIC && q.input.contains("failed")), "{:?}", r.requested());
}
