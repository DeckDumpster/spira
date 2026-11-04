//! Acceptance T1–T6 for `persona.archivist`, from its contract
//! (`wiki/projects/spira/designs/tasks/persona-archivist.md`, Acceptance) on `TaskRig`. The row is
//! ported (in `tasks::f::TASKS`). T6 runs two virtual days (the contract asks three). The settle effect's
//! input is taken to be a JSON object carrying its `outcome`; that shape is this test's, not the
//! contract's.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::units::{Exit, UnitHandle, UnitRan, UNIT_START, UNIT_STOP};
use runtime::StreamKey;
use spira_runtime::tasks::f::persona_archivist::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 5 * 60_000;
const BOGUS: &str = "persona.archivist.bogus";
const MODULE: &str = include_str!("../src/tasks/f/persona_archivist.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn config(wiki: &str) -> BTreeMap<String, String> {
    [("SPIRA_WIKI", wiki), ("SPIRA_ARCHIVIST_EVERY", "40"), ("SPIRA_ARCHIVIST_PER_PASS", "2"), ("SPIRA_ARCHIVIST_TIMEOUT", "900"), ("SPIRA_TZ", "America/Los_Angeles")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn handler() -> PersonaArchivist {
    PersonaArchivist { interval_ms: CADENCE_MS, config: config("/brain") }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn candidate(sid: &str, drift: u64) -> Candidate {
    Candidate { sid: sid.into(), tp: format!("/t/{sid}.jsonl"), ctx: 100_000, turns: 50, drift, band: "mid".into(), prev_state: "safe".into(), retry_at: None }
}

fn snap(drift: u64, stale: &[&str]) -> Snapshot {
    Snapshot { paused: false, candidates: vec![candidate("s1", drift)], stale: stale.iter().map(|s| s.to_string()).collect() }
}

fn in_state(state: State) -> Carry {
    Carry { state, run_id: Some("r1".into()), queue: vec![], current: Some(Current { sid: "s1".into(), at: 50, tc: 0 }), archived: 0 }
}

fn ran(exit: Exit) -> UnitRan {
    UnitRan { handle: UnitHandle::of(NAME, "r1-s1").unwrap(), exit, duration_ms: 60_000 }
}

fn names(acts: &[runtime::EffectSpec]) -> Vec<&str> {
    acts.iter().map(|a| a.name.as_str()).collect()
}

fn settled(acts: &[runtime::EffectSpec]) -> String {
    let s = acts.iter().find(|a| a.name == SETTLE).expect("a settle");
    serde_json::from_str::<serde_json::Value>(&s.input).unwrap()["outcome"].as_str().unwrap_or_default().to_string()
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap()));
    r = r.double(BRIEF, |_| Ok(r#"{"sysfile":"s.md","taskfile":"t.md","wiki_before":"abc"}"#.into()));
    for e in [COMMIT, SETTLE, DIGEST_SEND, MARK, FORGET, UNIT_START, UNIT_STOP] {
        r = r.double(e, |i| Ok(i.into()));
    }
    let (_, stream, kind) = PersonaArchivist::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::f::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    let mut r = TaskRig::new(1, handler().registration()).unwrap().double(OBSERVE, |_| Ok(serde_json::to_string(&snap(10, &[])).unwrap()));
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    running(&r);
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn t2_one_live_session_below_every_and_nothing_stale_is_silent() {
    let mut r = rig(1, |_| snap(10, &[]));
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
fn t3_unit_ran_in_idle_and_a_commit_with_no_wiki_are_refused_naming_the_state() {
    let refused = step(State::Idle, "ran-ok").expect_err("no unit runs in Idle");
    assert!(refused.contains("Idle"), "{refused}");
    let refused = decide(&Carry::default(), &Input::UnitRan(ran(Exit::Code(0))), &config("/brain")).expect_err("no unit runs in Idle");
    assert!(refused.contains("Idle"), "{refused}");
    let refused = decide(&in_state(State::Running), &Input::Succeeded { effect: COMMIT.into(), body: String::new() }, &config("")).expect_err("no wiki, no commit");
    assert!(refused.contains("Running"), "{refused}");
}

#[test]
fn t3_a_tick_in_idle_observes_and_the_carry_read_is_the_carry_recorded() {
    let (next, acts) = decide(&Carry::default(), &Input::Tick, &config("/brain")).unwrap();
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
fn t5_parity_paused_marks_and_a_drift_above_every_is_briefed() {
    let observing = in_state(State::Observing);
    let paused = Snapshot { paused: true, ..snap(90, &[]) };
    let (next, acts) = decide(&observing, &Input::Observed(paused), &config("/brain")).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Idle, vec![MARK]));
    let (next, acts) = decide(&observing, &Input::Observed(snap(90, &[])), &config("/brain")).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Briefing, vec![BRIEF]));
    assert!(acts[0].input.contains("s1"), "{}", acts[0].input);
}

#[test]
fn t5_parity_rc_0_with_a_wiki_commits_and_rc_124_settles_as_timeout() {
    let r = in_state(State::Running);
    let (next, acts) = decide(&r, &Input::UnitRan(ran(Exit::Code(0))), &config("/brain")).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Committing, vec![COMMIT]));
    let (next, acts) = decide(&r, &Input::UnitRan(ran(Exit::Code(124))), &config("/brain")).unwrap();
    assert_eq!(next.state, State::Settling);
    assert_eq!(settled(&acts), "timeout");
    let (next, acts) = decide(&in_state(State::Committing), &Input::Failed { effect: COMMIT.into(), error: "dirty checkout".into() }, &config("/brain")).unwrap();
    assert_eq!(next.state, State::Settling);
    assert_eq!(settled(&acts), "retry", "a dirty checkout settles as retry");
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_budget() {
    let world = |call: u64| snap(if call.is_multiple_of(144) { 90 } else { 10 }, &[]);
    let a = assert_deterministic(|seed| rig(seed, world), 1, 2);
    running(&a);
    assert_journal_budget(&a, 30);
}
