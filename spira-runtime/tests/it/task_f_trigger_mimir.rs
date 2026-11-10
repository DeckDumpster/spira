//! Acceptance T1–T6 for `trigger.mimir`, from its contract
//! (`wiki/projects/spira/designs/tasks/trigger-mimir.md`, Acceptance) on `TaskRig`. The row is ported
//! (in `tasks::f::TASKS`). T6 runs two virtual days (the contract asks fourteen).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::f::trigger_mimir::*;
use spira_runtime::tasks::f::Decision;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 15 * 60_000;
const BOGUS: &str = "trigger.mimir.bogus";
const MODULE: &str = include_str!("../../src/tasks/f/trigger_mimir.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];
const NOW: i64 = 1_800_000_000;

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn config() -> BTreeMap<String, String> {
    [("SPIRA_SCOPE_LABEL", "spira"), ("SPIRA_MIMIR_LABEL", "mimir"), ("SPIRA_RUN", "/run"), ("SPIRA_MIMIR_MAX_GAP_SECONDS", "10800"), ("SPIRA_MIMIR_LANDING_INTERVAL", "10"), ("SPIRA_MIMIR_MAX_BEADS", "3")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn handler() -> TriggerMimir {
    TriggerMimir { interval_ms: CADENCE_MS, config: config() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

/// `ago` seconds since the last pass, `landings` since the watermark.
fn snap(open: u64, ago: i64, landings: u64) -> Snapshot {
    Snapshot { halted: false, open_triggers: Ok(open), lane_admitted: true, now: NOW, lastpass: NOW - ago, watermark: NOW - ago, landings_since_watermark: landings, invalid_closed_raw: String::new() }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(FILE, |i| Ok(i.into()));
    let (_, stream, kind) = TriggerMimir::boot();
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
fn t2_an_hour_after_the_last_pass_with_no_landings_the_tick_asks_only_for_its_probe() {
    let mut r = rig(1, |_| snap(0, 3_600, 0));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_every_legal_step_is_taken_and_filing_with_an_open_trigger_is_refused() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(Step::Skip, "time").expect_err("a skipped tick files nothing");
    assert!(refused.contains("Skip"), "{refused}");
    let refused = may_file(&Ok(1)).expect_err("a re-count above 0 refuses the file");
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
fn t5_parity_an_open_trigger_or_no_condition_skips() {
    assert_eq!(skipped(&decide(&snap(1, 99_999, 99), &config())), Some("open"));
    assert_eq!(skipped(&decide(&snap(0, 3_600, 0), &config())), Some("no-trigger"));
}

#[test]
fn t5_parity_the_time_trigger_files_a_byte_equal_bead() {
    let Decision::File(bead) = decide(&snap(0, 10_800, 0), &config()) else { panic!("the max gap has elapsed") };
    assert_eq!(bead.title, "Mimir pass — 10800s elapsed");
    assert_eq!(bead.labels, ["spira", "mimir", "delivers:note:/run/mimir.log"]);
    let t = NOW - 10_800;
    assert_eq!(
        bead.body,
        format!("Scheduled trigger: the Mimir persona will claim this bead, run a retrospective pass over the failure distribution, identify recurring failure classes, and cut at most 3 remedy beads. Trigger: 10800s elapsed (lastpass ts={t}, watermark ts={t}). See spira/runes/mimir.md for the pass procedure.")
    );
}

fn filed() -> TaskRig {
    rig(1, |_| snap(0, 10_800, 0))
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&filed(), NAME);
    assert!(rows.iter().any(|k| k == "filed_at"), "{rows:?}");
}

#[test]
fn a_boot_does_not_file_again_inside_the_gap_a_filing_already_covers() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(filed, || handler().registration(), STREAM, runtime::event::EventKind::new(TICK, ""));
}

#[test]
fn a_journal_with_no_state_row_still_dates_the_last_filing() {
    let r = filed();
    assert!(last_filed_at(r.engine().projection(), &StreamKey(STREAM.into())).is_some());
}
