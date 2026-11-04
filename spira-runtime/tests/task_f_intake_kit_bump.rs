//! Acceptance T1–T6 for `intake.kit_bump`, from its contract
//! (`wiki/projects/spira/designs/tasks/intake-kit-bump.md`, Acceptance) on `TaskRig`. The row is ported
//! (in `tasks::f::TASKS`). T6 runs two virtual days (the contract asks fourteen).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::f::intake_kit_bump::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 10 * 60_000;
const BOGUS: &str = "intake.kit_bump.bogus";
const MODULE: &str = include_str!("../src/tasks/f/intake_kit_bump.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn config() -> BTreeMap<String, String> {
    [("SPIRA_KIT_REGISTRY", "http://registry"), ("SPIRA_KIT_CRATE", "kit"), ("SPIRA_KIT_SITES", "deck poke"), ("SPIRA_KIT_PERSONA", "builder")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn handler() -> IntakeKitBump {
    IntakeKitBump { interval_ms: CADENCE_MS, config: config() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(release: Option<&str>, locked: &str, requirement: &str) -> Snapshot {
    let site = || Ok(Site { locked: locked.into(), requirement: requirement.into() });
    Snapshot { release: Ok(release.map(str::to_string)), sites: [("deck".to_string(), site()), ("poke".to_string(), site())].into(), open: BTreeMap::new() }
}

fn names(acts: &[runtime::EffectSpec]) -> Vec<&str> {
    acts.iter().map(|a| a.name.as_str()).collect()
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(FILE, |_| Ok("sp-k1".into())).double(AMEND, |i| Ok(i.into()));
    let (_, stream, kind) = IntakeKitBump::boot();
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
fn t2_every_site_locked_at_the_release_journals_nothing_and_asks_only_for_its_probe() {
    let mut r = rig(1, |_| snap(Some("1.2.0"), "1.2.0", "^1.2"));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_current_open_retarget_current_is_legal_and_filing_over_an_open_bump_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(SiteState::BumpOpen, "file").expect_err("one open bump per site");
    assert!(refused.contains("BumpOpen"), "{refused}");
}

#[test]
fn t3_an_open_bump_at_an_older_target_is_amended_and_the_carry_reads_back_as_recorded() {
    let carry = Carry { bumps: [("deck".to_string(), ("sp-k1".to_string(), "1.3.0".to_string())), ("poke".to_string(), ("sp-k2".to_string(), "1.4.0".to_string()))].into() };
    let mut s = snap(Some("1.4.0"), "1.2.0", "^1");
    s.open = [("deck".to_string(), Ok(Some(Open { id: "sp-k1".into(), target: "1.3.0".into() }))), ("poke".to_string(), Ok(Some(Open { id: "sp-k2".into(), target: "1.4.0".into() })))].into();
    let (next, acts) = decide(&carry, &s, &config());
    assert_eq!(names(&acts), [AMEND], "deck moves to 1.4.0; poke is already there");
    assert_eq!(next.bumps["deck"].1, "1.4.0");
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
fn t5_parity_a_release_the_requirement_admits_is_a_lockfile_update_and_one_it_does_not_is_breaking() {
    let (_, acts) = decide(&Carry::default(), &snap(Some("1.3.0"), "1.2.0", "^1"), &config());
    assert_eq!(names(&acts), [FILE, FILE]);
    assert!(acts.iter().any(|a| a.input.contains("update kit lockfile to 1.3.0 in deck")), "{acts:?}");
    let (_, acts) = decide(&Carry::default(), &snap(Some("2.0.0"), "1.2.0", "^1"), &config());
    assert!(acts.iter().any(|a| a.input.contains("upgrade kit to 2.0.0 (breaking) in poke")), "{acts:?}");
}

#[test]
fn t5_parity_an_unpublished_crate_files_nothing() {
    let (next, acts) = decide(&Carry::default(), &snap(None, "1.2.0", "^1"), &config());
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(next, Carry::default());
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_budget() {
    let world = |call: u64| {
        let release = ["1.2.0", "1.3.0", "1.4.0"][((call / 96) % 3) as usize];
        snap(Some(release), "1.2.0", "^1")
    };
    let a = assert_deterministic(|seed| rig(seed, world), 1, 2);
    running(&a);
    assert_journal_budget(&a, 1);
}
