//! Acceptance T1–T6 for `asks.ideas`, from its contract
//! (`wiki/projects/spira/designs/tasks/asks-ideas.md`, Acceptance) on `TaskRig`. The row is ported (in
//! `tasks::f::TASKS`). T6 runs two virtual days (the contract asks seven).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, DAY_MS};
use runtime::StreamKey;
use spira_runtime::tasks::f::asks_ideas::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 5 * 60_000;
const BOGUS: &str = "asks.ideas.bogus";
const MODULE: &str = include_str!("../../src/tasks/f/asks_ideas.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];
const NOW: u64 = 1_800_000_000;

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn config() -> BTreeMap<String, String> {
    [("SPIRA_IDEAS_IDLE_SECS", "1800"), ("SPIRA_IDEAS_WAKE_FROM", "7"), ("SPIRA_IDEAS_WAKE_UNTIL", "22"), ("SPIRA_IDEAS_TZ", "America/Los_Angeles"), ("SPIRA_IDEAS_MIN_GAP_SECS", "86400")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn handler() -> AsksIdeas {
    AsksIdeas { interval_ms: CADENCE_MS, config: config() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(now: u64, hour: u32, busy: bool) -> Snapshot {
    Snapshot { now, local_hour: hour, busy: Ok(busy), ideas: Ok(vec![Idea { id: "sp-i1".into(), title: "an idea".into() }]) }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(ASK, |i| Ok(i.into()));
    let (_, stream, kind) = AsksIdeas::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::f::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn t2_busy_with_no_idle_spell_journals_nothing_and_asks_only_for_its_probe() {
    let mut r = rig(1, |_| snap(NOW, 10, true));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_busy_idle_prompted_busy_is_legal_and_a_second_prompt_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(Spell::Prompted, "prompt").expect_err("one prompt per idle spell");
    assert!(refused.contains("Prompted"), "{refused}");
}

#[test]
fn t3_the_carry_changes_only_on_an_edge_and_reads_back_as_recorded() {
    let (idle, _, _) = decide(&Carry::default(), &snap(NOW, 10, false), &config());
    assert_eq!(idle.idle_since, Some(NOW), "the first idle tick opens the spell");
    let (again, _, _) = decide(&idle, &snap(NOW + 300, 10, false), &config());
    assert_eq!(again, idle, "a tick inside the spell changes nothing");
    let back: Carry = serde_json::from_str(&serde_json::to_string(&idle).unwrap()).unwrap();
    assert_eq!(back, idle);
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
fn t5_parity_thirty_idle_minutes_in_waking_hours_prompt_once() {
    let spell = Carry { idle_since: Some(NOW - 1_800), last_prompt: None, prompted: false };
    let (next, outcome, acts) = decide(&spell, &snap(NOW, 10, false), &config());
    assert_eq!(outcome, Outcome::Prompted(1));
    assert_eq!(acts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), [ASK]);
    assert!(acts[0].input.contains("1 parked idea(s) — start any?"), "{}", acts[0].input);
    assert!(next.prompted);
    let (_, outcome, acts) = decide(&next, &snap(NOW + 300, 10, false), &config());
    assert_eq!((outcome, acts.len()), (Outcome::AlreadyPrompted, 0));
}

#[test]
fn t5_parity_outside_waking_hours_and_no_ideas_ask_nothing() {
    let spell = Carry { idle_since: Some(NOW - 1_800), last_prompt: None, prompted: false };
    let (_, outcome, acts) = decide(&spell, &snap(NOW, 23, false), &config());
    assert_eq!((outcome, acts.len()), (Outcome::OutsideWakingHours, 0));
    let none = Snapshot { ideas: Ok(vec![]), ..snap(NOW, 10, false) };
    let (_, outcome, acts) = decide(&spell, &none, &config());
    assert_eq!((outcome, acts.len()), (Outcome::NoIdeas, 0));
}

#[test]
fn t6_two_virtual_days_are_deterministic_within_budget_and_prompt_at_most_daily() {
    let world = |call: u64| snap(NOW + call * 300, ((call / 12) % 24) as u32, (call / 36).is_multiple_of(2));
    let a = assert_deterministic(|seed| rig(seed, world), 1, 2);
    running(&a);
    assert_journal_budget(&a, 2);
    let asks = a.requested().iter().filter(|q| q.name == ASK).count() as u64;
    assert!(asks <= a.now().div_ceil(DAY_MS), "{asks} prompts in {} ms", a.now());
}
