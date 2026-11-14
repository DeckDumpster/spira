//! The reconciler-engine detectors and the pass around them, through the entry points the
//! pass itself calls, against recording stand-ins for the forge, incident and sentinel seams.

use super::tests::{scratch_dir, test_config, KNOWN_EPOCH};
use super::*;

const NOW: u64 = 2_000_000_000;
const OPEN: &str = "batch_id=b1\nbranch=spira/queue/b1\n";

fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
    p.to_string_lossy().to_string()
}

fn forge_saying(dir: &Path, output: &str) -> String {
    script(dir, "forge", &format!("cat <<'EOF'\n{output}\nEOF"))
}

fn with_open_batch(dir: &Path, forge: String) -> Config {
    let mut cfg = test_config(dir, NOW);
    let map = dir.join("rows.txt");
    fs::write(&map, format!("r | {} | queue | origin/main | |\n", dir.display())).unwrap();
    cfg.repo_map = Some(map);
    cfg.seams.forge = forge;
    cfg.batch_show = std::sync::Arc::new(|_| Ok(OPEN.to_string()));
    cfg
}

fn landing_pass_at(cfg: &Config, epoch: u64) {
    fs::write(&cfg.qc_log, format!("{} spira: landing: pass complete\n", iso_of(epoch))).unwrap();
}

fn czar_log(cfg: &Config) -> String {
    fs::read_to_string(&cfg.czar_log).unwrap_or_default()
}

// covers: UC-ops-detection-remediation-23
#[test]
fn loop_stalled_is_silent_at_its_threshold_and_fires_one_second_past_it() {
    let dir = scratch_dir("loop-threshold");
    let cfg = test_config(&dir, KNOWN_EPOCH + 3000);
    landing_pass_at(&cfg, KNOWN_EPOCH);
    assert!(!detect_loop_stalled(&cfg, &mut StateMap::new()).0.is_gap);
    let cfg = test_config(&dir, KNOWN_EPOCH + 3001);
    let (v, remedy, _) = detect_loop_stalled(&cfg, &mut StateMap::new());
    assert!(v.is_gap);
    assert_eq!(remedy, "inference");
}

// covers: UC-ops-detection-remediation-23
#[test]
fn ci_stalled_and_ci_red_each_fire_only_past_their_own_threshold() {
    let dir = scratch_dir("ci-thresholds");
    let queued = |ago: u64| format!("run-id: 7\nqueued-since: {}", NOW - ago);
    let red = |ago: u64| format!("run-id: 7\nrun-conclusion: failure\nrun-completed-at: {}", NOW - ago);
    let verdicts = |output: String| {
        let mut cfg = with_open_batch(&dir, forge_saying(&dir, &output));
        cfg.ci_queued_max = 600;
        cfg.ci_red_max = 1200;
        let (stalled, _, _, red, _, _) = detect_ci(&cfg, &mut StateMap::new());
        (stalled.is_gap, red.is_gap)
    };
    assert_eq!(verdicts(queued(600)), (false, false));
    assert_eq!(verdicts(queued(601)), (true, false));
    assert_eq!(verdicts(red(1200)), (false, false));
    assert_eq!(verdicts(red(1201)), (false, true));
    assert_eq!(verdicts(red(700)), (false, false), "700s is past the queued limit but not the red one");
}

// covers: UC-ops-detection-remediation-23
#[test]
fn base_red_fires_on_a_red_base_run_at_once_and_a_green_one_is_silent() {
    let dir = scratch_dir("base-red");
    let run = |conclusion: &str| {
        let cfg = with_open_batch(&dir, forge_saying(&dir, &format!("run-id: 5\nrun-conclusion: {conclusion}")));
        detect_base_red(&cfg, &mut StateMap::new())
    };
    let (green, remedy, _) = run("success");
    assert!(!green.is_gap);
    assert_eq!(remedy, "none");
    let (red, remedy, _) = run("failure");
    assert!(red.is_gap);
    assert_eq!(remedy, "inference");
}

// covers: UC-ops-detection-remediation-23
#[test]
fn pool_idle_needs_a_full_pool_and_no_open_batch_and_waits_out_the_stall() {
    use czar_pass::pool_idle;
    assert!(matches!(pool_idle("r", false, 3, 4), RawStatus::Satisfied), "below the round threshold");
    assert!(matches!(pool_idle("r", true, 40, 4), RawStatus::Satisfied), "an open batch is deliberate back-pressure");
    let full = pool_idle("r", false, 4, 4);
    assert!(matches!(full, RawStatus::Gap { .. }));
    let (inside, _) = step(NOW, full.clone(), 900, Default::default());
    assert!(!inside.is_gap, "a full pool is not a stall until the stall window has run");
    let (past, _) = step(NOW + 900, full, 900, step(NOW, pool_idle("r", false, 4, 4), 900, Default::default()).1);
    assert!(past.is_gap);
}

// covers: UC-ops-detection-remediation-23
#[test]
fn every_detector_fires_in_one_pass_without_masking_another() {
    let dir = scratch_dir("coexist");
    let output = format!("run-id: 7\nqueued-since: {}\nrun-conclusion: failure\nrun-completed-at: {}", NOW - 700, NOW - 700);
    let cfg = with_open_batch(&dir, forge_saying(&dir, &output));
    landing_pass_at(&cfg, NOW - 5000);
    let lines = "no suites identified; leaving batch open\nejected 0, requeued 3\nqueue_sort_rows: ranking failed\n";
    let mut state = StateMap::new();
    let fired = [
        detect_deadlock(&cfg, &mut state, lines).0.is_gap,
        detect_attribution_failed(&cfg, &mut state, lines).0.is_gap,
        detect_sort_failed(&cfg, &mut state, lines).0.is_gap,
        detect_loop_stalled(&cfg, &mut state).0.is_gap,
    ];
    let (stalled, _, _, red, _, _) = detect_ci(&cfg, &mut state);
    let base = detect_base_red(&cfg, &mut state).0;
    assert_eq!(fired, [true; 4]);
    assert_eq!([stalled.is_gap, red.is_gap, base.is_gap], [true; 3]);
    let log = czar_log(&cfg);
    for class in ["deadlock", "attribution-failed", "sort-failed", "loop-stalled", "ci-stalled", "ci-red", "base-red"] {
        assert!(log.contains(&format!("CZAR-WOULD: {class} ")), "{class} missing from\n{log}");
    }
}

// covers: UC-ops-detection-remediation-23
#[test]
fn an_unreadable_input_is_unobservable_and_never_clear() {
    let dir = scratch_dir("unobservable");
    let unobservable = |v: &Verdict| matches!(v.status, RawStatus::Unobservable { .. });

    let cfg = test_config(&dir, NOW);
    fs::write(&cfg.qc_log, "garbage-stamp!!!!!!!!! spira: landing: pass complete\n").unwrap();
    assert!(unobservable(&detect_loop_stalled(&cfg, &mut StateMap::new()).0), "an unparseable landing stamp");

    let mut cfg = with_open_batch(&dir, script(&dir, "forge", "exit 1"));
    let (stalled, _, _, red, _, _) = detect_ci(&cfg, &mut StateMap::new());
    assert!(unobservable(&stalled) && unobservable(&red), "a forge call that failed");

    cfg.batch_show = std::sync::Arc::new(|_| Err("could not say: store stale".into()));
    let (stalled, _, _, red, _, _) = detect_ci(&cfg, &mut StateMap::new());
    assert!(unobservable(&stalled) && unobservable(&red), "a batch view the queue could not give");
    assert!(unobservable(&detect_base_red(&cfg, &mut StateMap::new()).0));
    assert!(unobservable(&detect_pool_idle(&cfg, &mut StateMap::new()).0));

    let mut cfg = with_open_batch(&dir, forge_saying(&dir, ""));
    let mut state = StateMap::new();
    let (inside, _, _) = detect_base_red(&cfg, &mut state);
    assert!(unobservable(&inside) && !inside.is_gap, "an unreadable base waits out its grace first");
    cfg.now_secs = NOW + cfg.base_unreadable_grace;
    let (past, remedy, _) = detect_base_red(&cfg, &mut state);
    assert!(unobservable(&past) && past.is_gap, "then it is filed as unreadable, never as green");
    assert_eq!(remedy, "inference");
}

fn recording_seams(dir: &Path, cfg: &mut Config) {
    cfg.seams.incident = script(dir, "incident", "printf 'cause=%s ref=%s\\n' \"$SPIRA_INCIDENT_CAUSE\" \"$SPIRA_INCIDENT_REF\" >> \"$0.calls\"");
    cfg.seams.sentinel = script(dir, "sentinel", "echo \"$*\" >> \"$0.calls\"");
}

fn calls(dir: &Path, seam: &str) -> String {
    fs::read_to_string(dir.join(format!("{seam}.calls"))).unwrap_or_default()
}

// covers: UC-ops-detection-remediation-24
#[test]
fn a_shadow_class_logs_would_and_files_nothing_while_act_files_and_calls_the_czar() {
    let dir = scratch_dir("stages");
    let mut cfg = test_config(&dir, NOW);
    recording_seams(&dir, &mut cfg);
    let lines = "no suites identified; leaving batch open\n";

    let (v, remedy, _) = detect_deadlock(&cfg, &mut StateMap::new(), lines);
    assert!(v.is_gap);
    assert_eq!(remedy, "inference");
    assert!(czar_log(&cfg).contains("CZAR-WOULD: deadlock inference"));
    assert_eq!((calls(&dir, "incident"), calls(&dir, "sentinel")), (String::new(), String::new()), "shadow filed or called");

    cfg.stages.insert("DEADLOCK".into(), Stage::Act);
    detect_deadlock(&cfg, &mut StateMap::new(), lines);
    assert!(calls(&dir, "incident").contains("cause=deadlock"), "{}", calls(&dir, "incident"));
    assert_eq!(calls(&dir, "sentinel").trim(), "--call czar");
    assert_eq!(czar_log(&cfg).matches("CZAR-WOULD").count(), 1, "act logged a would-line too");
}

/// The one world every `run_pass` test shares: the config door resolves once per process, so
/// a second world would be silently ignored. Each user holds the environment lock and leaves
/// the run directory empty.
pub(crate) struct World {
    pub run: PathBuf,
    _env: testkit::EnvGuard,
}

pub(crate) fn pass_world() -> World {
    type Edits = Vec<(&'static str, Option<String>)>;
    static DIR: std::sync::OnceLock<(testkit::TempDir, PathBuf, Edits)> = std::sync::OnceLock::new();
    let (_, run, edits) = DIR.get_or_init(|| {
        let d = testkit::TempDir::new("czar-pass-world");
        let run = d.path().join("run");
        fs::create_dir_all(&run).unwrap();
        let edits = spira_config::process::fixture_world_env(d.path(), &run, &["spira", "other"]);
        (d, run, edits)
    });
    let edits: Vec<(&str, Option<&str>)> = edits.iter().map(|(k, v)| (*k, v.as_deref())).collect();
    World { run: run.clone(), _env: testkit::env(&edits) }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.run);
        let _ = fs::create_dir_all(&self.run);
    }
}

// covers: UC-ops-detection-remediation-24
#[test]
fn a_pass_finishes_within_its_budget_and_reports_every_class() {
    let world = pass_world();
    let run = world.run.clone();
    let started = std::time::Instant::now();
    assert_eq!(run_pass(), Ok(()));
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
    let log = fs::read_to_string(run.join("czar.log")).unwrap();
    for class in czar_pass::CLASSES.iter().filter(|c| **c != "starved") {
        assert!(log.contains(&format!("CLASS={class} ")), "{class} not reported in\n{log}");
    }
}

// covers: UC-ops-detection-remediation-24
#[test]
fn a_concurrent_pass_is_skipped_under_flock_and_a_halted_world_exits_clean_without_acting() {
    let world = pass_world();
    let run = world.run.clone();
    let held = OpenOptions::new().create(true).truncate(false).write(true).open(run.join("czar-pass.lock")).unwrap();
    assert_eq!(unsafe { flock(held.as_raw_fd(), LOCK_EX | LOCK_NB) }, 0);
    assert_eq!(run_pass(), Ok(()));
    assert!(!run.join("czar.log").exists(), "a pass that lost the lock still ran its detectors");
    drop(held);

    fs::write(run.join("world.halted"), "").unwrap();
    assert_eq!(run_pass(), Ok(()));
    assert!(!run.join("czar.log").exists(), "a halted pass acted");

    fs::remove_file(run.join("world.halted")).unwrap();
    assert_eq!(run_pass(), Ok(()));
    assert!(run.join("czar.log").exists(), "positive control: the same world with the lock free and no halt does run");
}
