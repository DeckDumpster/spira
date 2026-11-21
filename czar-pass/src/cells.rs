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
#[test_id(ttmq14a)]
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
#[test_id(t6qoj0v)]
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
#[test_id(twneqpz)]
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
#[test_id(trno6ol)]
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
#[test_id(to5zjn8)]
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
#[test_id(t7dwa2w)]
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
    cfg.seams.incident = script(dir, "incident", "printf 'cause=%s ref=%s priority=%s express=%s repo=%s body=%s\\n' \"$SPIRA_INCIDENT_CAUSE\" \"$SPIRA_INCIDENT_REF\" \"$SPIRA_INCIDENT_PRIORITY\" \"$SPIRA_INCIDENT_EXPRESS\" \"$SPIRA_INCIDENT_REPO\" \"$(cat | tr '\\n' ' ')\" >> \"$0.calls\"");
}

fn calls(dir: &Path, seam: &str) -> String {
    fs::read_to_string(dir.join(format!("{seam}.calls"))).unwrap_or_default()
}

// covers: UC-ops-detection-remediation-24
#[test_id(tgu327s)]
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
    assert_eq!(calls(&dir, "incident"), "", "shadow filed");

    cfg.stages.insert("DEADLOCK".into(), Stage::Act);
    detect_deadlock(&cfg, &mut StateMap::new(), lines);
    assert!(calls(&dir, "incident").contains("cause=deadlock"), "{}", calls(&dir, "incident"));
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
#[test_id(ts2jr50)]
#[test]
fn a_pass_runs_every_detector_to_completion_and_reports_every_class() {
    let world = pass_world();
    let run = world.run.clone();
    assert_eq!(run_pass(), Ok(()));
    let log = fs::read_to_string(run.join("czar.log")).unwrap();
    for class in czar_pass::CLASSES.iter().filter(|c| **c != "starved") {
        assert!(log.contains(&format!("CLASS={class} ")), "{class} not reported in\n{log}");
    }
}

// covers: UC-ops-detection-remediation-24
#[test_id(te3t85r)]
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

// covers: UC-ops-detection-remediation-23
#[test_id(tq0esrg)]
#[test]
fn a_red_base_files_p0_express_naming_its_failing_suites_under_one_ref_however_often_it_is_seen() {
    let dir = scratch_dir("base-red-filing");
    let out = "run-id: 5\nrun-conclusion: failure\nred-suite: test-b.sh\nred-suite: test-a.sh";
    let mut cfg = with_open_batch(&dir, forge_saying(&dir, out));
    recording_seams(&dir, &mut cfg);
    cfg.stages.insert("BASE_RED".into(), Stage::Act);
    detect_base_red(&cfg, &mut StateMap::new());
    cfg.now_secs += 60;
    detect_base_red(&cfg, &mut StateMap::new());
    let filed = calls(&dir, "incident");
    let first = filed.lines().next().unwrap();
    assert!(first.contains("priority=0 express=1 repo=r"), "{first}");
    assert!(first.contains("test-a.sh, test-b.sh"), "{first}");
    assert_eq!(filed.lines().count(), 2);
    let refs: Vec<&str> = filed.lines().map(|l| l.split_whitespace().nth(1).unwrap()).collect();
    assert_eq!(refs[0], refs[1], "a second sighting is a recurrence of the same bead");
}

// covers: UC-ops-detection-remediation-23
#[test_id(tif8eow)]
#[test]
fn a_stalled_ci_job_is_rerun_once_then_escalated_under_a_ref_that_ignores_how_long_it_has_waited() {
    let dir = scratch_dir("ci-stalled-remedy");
    let forge = |dir: &Path, queued_ago: u64| {
        script(dir, "forge", &format!("case \"$1\" in workflow-rerun) echo rerun >> \"$0.calls\";; esac\nprintf 'run-id: 7\\nqueued-since: %s\\n' {}", NOW - queued_ago))
    };
    let mut cfg = with_open_batch(&dir, forge(&dir, 700));
    recording_seams(&dir, &mut cfg);
    cfg.stages.insert("CI_STALLED".into(), Stage::Act);
    let mut state = StateMap::new();
    assert_eq!(detect_ci(&cfg, &mut state).1, "det-rerun");
    assert_eq!(calls(&dir, "forge"), "rerun\n");
    cfg.now_secs += 30;
    cfg.seams.forge = forge(&dir, 730);
    assert_eq!(detect_ci(&cfg, &mut state).1, "inference", "the rerun did not close the gap");
    assert_eq!(calls(&dir, "forge"), "rerun\n", "no second blind rerun");
    let incident = calls(&dir, "incident");
    assert_eq!(incident.lines().count(), 1, "{incident}");
    assert!(incident.contains("ref=incident:queue-ci-stalled-r "), "the ref carries no measured duration: {incident}");
}
