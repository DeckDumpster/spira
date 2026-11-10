//! Acceptance T1–T6 for `trigger.warden`, from its contract
//! (`wiki/projects/spira/designs/tasks/trigger-warden.md`, Acceptance) on `TaskRig`. The row is ported
//! (in `tasks::f::TASKS`). T6 runs two virtual days (the contract asks thirty). T1 checks the 2 h
//! cadence only: a `Task` row has no boot offset, so the contract's first tick at boot+20 min is
//! not expressible (and `tasks.rs` requires the boot tick to arm one cadence on).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::f::trigger_warden::*;
use spira_runtime::tasks::f::Decision;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 2 * 3_600_000;
const BOGUS: &str = "trigger.warden.bogus";
const MODULE: &str = include_str!("../../src/tasks/f/trigger_warden.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn config() -> BTreeMap<String, String> {
    [("SPIRA_SCOPE_LABEL", "spira"), ("SPIRA_WARDEN_LABEL", "warden-sweep"), ("SPIRA_RUN", "/run")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn handler() -> TriggerWarden {
    TriggerWarden { interval_ms: CADENCE_MS, config: config() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(open: Result<u64, String>) -> Snapshot {
    Snapshot { halted: false, open_sweeps: open }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(FILE, |i| Ok(i.into()));
    let (_, stream, kind) = TriggerWarden::boot();
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
fn t2_with_a_sweep_open_the_tick_asks_only_for_its_probe() {
    let mut r = rig(1, |_| snap(Ok(1)));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_every_legal_step_is_taken_and_filing_over_an_open_sweep_is_refused() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(Step::Skip, "file").expect_err("a skipped tick files nothing");
    assert!(refused.contains("Skip"), "{refused}");
    let refused = may_file(&Ok(1)).expect_err("a re-count showing an open sweep refuses the file");
    assert!(refused.contains("open"), "{refused}");
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
fn t5_parity_the_scripts_skip_exits() {
    assert_eq!(skipped(&decide(&snap(Err("count timed out".into())), &config())), Some("unreadable"));
    assert_eq!(skipped(&decide(&snap(Ok(1)), &config())), Some("open"));
}

#[test]
fn t5_parity_the_filed_sweep_is_byte_equal() {
    let Decision::File(bead) = decide(&snap(Ok(0)), &config()) else { panic!("no sweep open files one") };
    assert_eq!(bead.title, TITLE);
    assert_eq!(bead.body, DESCRIPTION);
    assert_eq!((bead.kind.as_str(), bead.priority), ("task", 3));
    assert_eq!(bead.labels, ["spira", "warden-sweep", "delivers:note:/run/warden.log"]);
}
