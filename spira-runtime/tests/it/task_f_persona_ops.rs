//! Acceptance T1–T6 for `persona.ops`, from its contract
//! (`wiki/projects/spira/designs/tasks/persona-ops.md`, Acceptance) on `TaskRig`. The row is ported
//! (in `tasks::f::TASKS`). T6 runs two virtual days (the contract asks seven). The ledger effect's input is taken to be a
//! JSON object carrying the line's `rc`; that shape is this test's, not the contract's.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::units::{Exit, UnitHandle, UnitRan, UNIT_START, UNIT_STOP};
use runtime::StreamKey;
use spira_runtime::tasks::f::persona_ops::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 5 * 60_000;
const BOGUS: &str = "persona.ops.bogus";
const MODULE: &str = include_str!("../../src/tasks/f/persona_ops.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn handler() -> PersonaOps {
    PersonaOps { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(head: &str, live: u32, draining: bool, pause: Pause) -> Snapshot {
    Snapshot { halted: false, rune_present: true, prompt_head: head.into(), prompt: format!("{head}\nbody"), live, max: 2, draining, pause }
}

fn nominal() -> Snapshot {
    snap(NOMINAL, 0, false, Pause::Open)
}

fn in_state(state: State) -> Carry {
    Carry { state, run_id: Some("r1".into()), name: Some("ops-1".into()), started_at: Some(0) }
}

fn ran(exit: Exit) -> UnitRan {
    UnitRan { handle: UnitHandle::of(NAME, "r1").unwrap(), exit, duration_ms: 60_000 }
}

fn names(acts: &[runtime::EffectSpec]) -> Vec<&str> {
    acts.iter().map(|a| a.name.as_str()).collect()
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap()));
    r = r.double(BRIEF, |_| Ok(r#"{"name":"ops-1","sys_file":"s.md","task_file":"t.md","argv":["claude"]}"#.into()));
    for e in [INTAKE_INSTALL, INCIDENT_DRAIN, LEDGER, CLEANUP, UNIT_START, UNIT_STOP] {
        r = r.double(e, |i| Ok(i.into()));
    }
    let (_, stream, kind) = PersonaOps::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::f::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    let mut r = TaskRig::new(1, handler().registration()).unwrap().double(OBSERVE, |_| Ok(serde_json::to_string(&nominal()).unwrap()));
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    running(&r);
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn t2_a_nominal_prompt_journals_nothing_and_asks_only_for_its_probe() {
    let mut r = rig(1, |_| nominal());
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_every_row_of_the_decide_table_is_a_legal_step() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
}

#[test]
fn t3_unit_ran_in_idle_and_a_tick_in_running_are_refused_naming_the_state() {
    let refused = step(State::Idle, "unit.ran").expect_err("no unit runs in Idle");
    assert!(refused.contains("Idle"), "{refused}");
    let refused = decide(&Carry::default(), &Input::UnitRan { ran: ran(Exit::Code(0)), outcome: None }).expect_err("no unit runs in Idle");
    assert!(refused.contains("Idle"), "{refused}");
    let refused = decide(&in_state(State::Running), &Input::Tick).expect_err("single-flight");
    assert!(refused.contains("Running"), "{refused}");
}

#[test]
fn t3_a_tick_in_idle_observes_and_the_carry_read_is_the_carry_recorded() {
    let (next, acts) = decide(&Carry::default(), &Input::Tick).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Observing, vec![OBSERVE]));
    let back: Carry = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(back, next);
}

#[test]
fn t4_every_effect_has_a_real_runner_and_a_sim_double() {
    let row = f::TASKS.iter().find(|t| t.name == NAME).expect("the row is ported");
    assert_eq!(row.effects, EFFECTS, "the row declares the module's effects");
    let real: Vec<&str> = tasks::routes().keys().copied().collect();
    let doubles = sim::doubles::service().names();
    let sim: Vec<&str> = doubles.iter().map(String::as_str).collect();
    assert!(effects_runnable(&[BOGUS], &real, &sim).is_err(), "the planted {BOGUS} must be refused");
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test]
fn t4_the_module_launches_no_process() {
    for l in LAUNCHERS {
        assert_eq!(launcher(&format!("{l}(\"x\")")), Some(*l), "the scan must see {l}");
    }
    assert_eq!(launcher(MODULE), None);
}

#[test]
fn t5_parity_capacity_draining_and_paused_write_their_awake_line_and_go_idle() {
    let observing = in_state(State::Observing);
    for (s, word) in [(snap("SWEEP: go", 2, false, Pause::Open), "capacity"), (snap("SWEEP: go", 0, true, Pause::Open), "draining"), (snap("SWEEP: go", 0, false, Pause::Paused { left_s: 60 }), "paused")] {
        let (next, acts) = decide(&observing, &Input::Observed(s)).unwrap();
        assert_eq!((next.state, names(&acts)), (State::Idle, vec![LEDGER]), "{word}");
        assert!(acts[0].input.contains(word), "{word}: {}", acts[0].input);
    }
}

#[test]
fn t5_parity_an_unlanded_segment_counts_as_rc_0() {
    let (next, acts) = decide(&in_state(State::Running), &Input::UnitRan { ran: ran(Exit::Code(1)), outcome: Some("unlanded".into()) }).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Recording, vec![LEDGER, CLEANUP]));
    let line: serde_json::Value = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(line["rc"], 0, "{line}");
}

#[test]
fn t9_the_prepare_acts_are_keyed_by_the_run_the_handler_assigned() {
    let r = rig(1, |_| snap("SWEEP: go", 0, false, Pause::Open));
    running(&r);
    let key = |name: &str| {
        r.journaled().iter().filter(|e| e.kind.tag == runtime::event::EFFECT_REQUESTED).find_map(|e| {
            let body: serde_json::Value = serde_json::from_str(&e.kind.body).ok()?;
            (body["spec"]["name"] == name).then(|| body["spec"]["key"].as_str().unwrap_or_default().to_string())
        })
    };
    let intake = key(INTAKE_INSTALL).expect("the prepare step asks for the intake install");
    let id = intake.strip_prefix("ops/").and_then(|k| k.strip_suffix("/intake")).expect("ops/<run>/intake");
    assert!(!id.is_empty(), "the run id is built into the key: {intake}");
    assert_eq!(key(INCIDENT_DRAIN), Some(format!("ops/{id}/drain")));
    assert_eq!(key(BRIEF), Some(format!("ops/{id}/brief")));
}
