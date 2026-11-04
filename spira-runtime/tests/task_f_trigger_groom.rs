//! Acceptance T1–T6 for `trigger.groom`, from its contract
//! (`wiki/projects/spira/designs/tasks/trigger-groom.md`, Acceptance) on `TaskRig`. The row is ported
//! (in `tasks::f::TASKS`). T6 runs two virtual days (the contract asks thirty). The task carries no
//! state, so T3's read-equals-recorded part does not apply.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::f::trigger_groom::*;
use spira_runtime::tasks::f::Decision;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 6 * 3_600_000;
const BOGUS: &str = "trigger.groom.bogus";
const MODULE: &str = include_str!("../src/tasks/f/trigger_groom.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn config() -> BTreeMap<String, String> {
    [("SPIRA_SCOPE_LABEL", "spira"), ("SPIRA_GROOMER_LABEL", "groom"), (THRESHOLD_KEY, "5"), ("SPIRA_RUN", "/run"), ("SPIRA_HOME_REPO", "spira")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn handler() -> TriggerGroom {
    TriggerGroom { interval_ms: CADENCE_MS, config: config() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(open: u64, lane: bool, live: u64, landings: u64) -> Snapshot {
    Snapshot { halted: false, open_triggers: Ok(open), lane_admitted: lane, lastpass: 0, live_owing: Ok(live), landings_since_lastpass: landings, home_repo: "spira".into() }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(FILE, |i| Ok(i.into()));
    let (_, stream, kind) = TriggerGroom::boot();
    r.tick(stream, kind).unwrap();
    r
}

fn skipped(d: &Decision) -> Option<&str> {
    match d {
        Decision::Skip(why) => Some(why.as_str()),
        Decision::File(_) => None,
    }
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
fn t2_with_a_groom_trigger_open_the_tick_asks_only_for_its_probe() {
    let mut r = rig(1, |_| snap(1, true, 9, 9));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_every_legal_step_is_taken_and_filing_over_an_open_trigger_is_refused() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(Step::Filed, "file").expect_err("a filed tick files nothing more");
    assert!(refused.contains("Filed"), "{refused}");
    let refused = may_file(&Ok(1)).expect_err("a re-count above 0 refuses the file");
    assert!(refused.contains("open"), "{refused}");
    assert_eq!(skipped(&decide(&snap(1, true, 9, 9), &config())), Some("open"));
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
fn t5_parity_dedup_lane_and_threshold_skip_as_the_script_does() {
    assert_eq!(skipped(&decide(&snap(1, true, 9, 9), &config())), Some("open"), "dedup");
    assert_eq!(skipped(&decide(&snap(0, false, 9, 9), &config())), Some("lane"), "lane refused");
    assert_eq!(skipped(&decide(&snap(0, true, 2, 2), &config())), Some("score"), "below threshold");
}

#[test]
fn t5_parity_a_first_run_at_threshold_files_the_groomer_bead() {
    let Decision::File(bead) = decide(&snap(0, true, 3, 2), &config()) else { panic!("live 3 + landings 2 meets the threshold of 5") };
    assert_eq!(bead.title, TITLE);
    assert_eq!(bead.priority, 3);
    assert_eq!(bead.persona.as_deref(), Some("groomer"));
    assert_eq!(bead.repo.as_deref(), Some("spira"));
    assert!(bead.labels.contains(&"groom-trigger".to_string()) && bead.labels.contains(&"delivers:note:/run/groom.log".to_string()), "{:?}", bead.labels);
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_budget() {
    let world = |call: u64| snap(call % 2, true, 6, 0);
    let a = assert_deterministic(|seed| rig(seed, world), 1, 2);
    running(&a);
    assert_journal_budget(&a, 1);
}
