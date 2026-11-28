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
    Carry { state, run_id: Some("r1".into()), name: Some("ops-1".into()), started_at: Some(0), ..Default::default() }
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
    for e in [INTAKE_INSTALL, LEDGER, CLEANUP, UNIT_START, UNIT_STOP] {
        r = r.double(e, |i| Ok(i.into()));
    }
    let (_, stream, kind) = PersonaOps::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test_id(teyz12h)]
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

#[test_id(tgialav)]
#[test]
fn t2_a_nominal_prompt_journals_nothing_and_asks_only_for_its_probe() {
    let mut r = rig(1, |_| nominal());
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test_id(tveb22j)]
#[test]
fn t3_every_row_of_the_decide_table_is_a_legal_step() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
}

#[test_id(tey14ml)]
#[test]
fn t3_unit_ran_in_idle_and_a_tick_in_running_are_refused_naming_the_state() {
    let refused = step(State::Idle, "unit.ran").expect_err("no unit runs in Idle");
    assert!(refused.contains("Idle"), "{refused}");
    let refused = decide(&Carry::default(), &Input::UnitRan { ran: ran(Exit::Code(0)), outcome: None }).expect_err("no unit runs in Idle");
    assert!(refused.contains("Idle"), "{refused}");
    let refused = decide(&in_state(State::Running), &Input::Tick).expect_err("single-flight");
    assert!(refused.contains("Running"), "{refused}");
}

#[test_id(tffrx0i)]
#[test]
fn t3_a_tick_in_idle_observes_and_the_carry_read_is_the_carry_recorded() {
    let (next, acts) = decide(&Carry::default(), &Input::Tick).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Observing, vec![OBSERVE]));
    let back: Carry = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(back, next);
}

#[test_id(t62fq3d)]
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

#[test_id(tnntdpx)]
#[test]
fn t4_the_module_launches_no_process() {
    for l in LAUNCHERS {
        assert_eq!(launcher(&format!("{l}(\"x\")")), Some(*l), "the scan must see {l}");
    }
    assert_eq!(launcher(MODULE), None);
}

#[test_id(t2s67as)]
#[test]
fn t5_parity_capacity_draining_and_paused_write_their_awake_line_and_go_idle() {
    let observing = in_state(State::Observing);
    for (s, word) in [(snap("SWEEP: go", 2, false, Pause::Open), "capacity"), (snap("SWEEP: go", 0, true, Pause::Open), "draining"), (snap("SWEEP: go", 0, false, Pause::Paused { left_s: 60 }), "paused")] {
        let (next, acts) = decide(&observing, &Input::Observed(s)).unwrap();
        assert_eq!((next.state, names(&acts)), (State::Idle, vec![LEDGER]), "{word}");
        assert!(acts[0].input.contains(word), "{word}: {}", acts[0].input);
    }
}

#[test_id(ti60xkd)]
#[test]
fn t5_parity_an_unlanded_segment_counts_as_rc_0() {
    let (next, acts) = decide(&in_state(State::Running), &Input::UnitRan { ran: ran(Exit::Code(1)), outcome: Some("unlanded".into()) }).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Recording, vec![LEDGER, CLEANUP]));
    let line: serde_json::Value = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(line["rc"], 0, "{line}");
}

#[test_id(tnz7c1d)]
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
    assert_eq!(key(BRIEF), Some(format!("ops/{id}/brief")));
}

fn mid_run() -> TaskRig {
    let mut r = rig(1, |_| snap("a changed prompt head", 0, false, Pause::Open));
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test_id(tz2clb4)]
#[test]
fn the_run_in_flight_is_kept_as_rows_and_not_as_one_whole_row() {
    let r = mid_run();
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.iter().any(|k| k == "run_id"), "{rows:?}");
}

#[test_id(tgylb1t)]
#[test]
fn a_boot_records_the_run_that_ends_after_it_as_an_unstopped_process_does() {
    let run_id = spira_runtime::watchers::carried::<Carry>(mid_run().engine().projection(), &StreamKey(STREAM.into()), STATE).run_id.expect("a run in flight");
    let ended = UnitRan { handle: UnitHandle::of(NAME, &run_id).unwrap(), exit: Exit::Code(0), duration_ms: 60_000 }.event_kind_with(&runtime::units::RanOutput::captured(b"", b""));
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(mid_run, || handler().registration(), STREAM, ended);
}

fn failed(effect: &str, error: &str) -> Input {
    Input::Failed { effect: effect.into(), error: error.into() }
}

fn why_of(acts: &[runtime::EffectSpec]) -> Option<String> {
    let line: serde_json::Value = serde_json::from_str(&acts.iter().find(|a| a.name == LEDGER).unwrap().input).unwrap();
    line["why"].as_str().map(String::from)
}

#[test_id(tgmpgnu)]
#[test]
fn a_unit_start_that_fails_records_why_and_removes_the_brief_files() {
    let (next, acts) = decide(&in_state(State::Starting), &failed(UNIT_START, "boom\nmore")).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Recording, vec![LEDGER, CLEANUP]));
    assert_eq!(why_of(&acts).as_deref(), Some("boom"));
    let line: serde_json::Value = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(line["error"], "boom\nmore");
}

#[test_id(ttuxjgg)]
#[test]
fn a_brief_that_fails_records_why_and_removes_the_brief_files() {
    let (next, acts) = decide(&in_state(State::Briefing), &failed(BRIEF, "\nSPIRA_TOML is not set — it names the one source of config\nmore")).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Recording, vec![LEDGER, CLEANUP]));
    assert_eq!(why_of(&acts).as_deref(), Some("SPIRA_TOML is not set — it names the one source of config"));
}

#[test_id(tdkl2zt)]
#[test]
fn a_brief_with_no_argv_records_why_and_removes_the_brief_files() {
    let body = r#"{"name":"ops-1","sys_file":"s.md","task_file":"t.md","argv":[]}"#;
    let (next, acts) = decide(&in_state(State::Briefing), &Input::Succeeded { effect: BRIEF.into(), body: body.into() }).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Recording, vec![LEDGER, CLEANUP]));
    assert_eq!(why_of(&acts).as_deref(), Some("the brief returned no argv"));
}

#[test_id(t0de0xs)]
#[test]
fn a_missing_statute_still_goes_idle_without_cleanup() {
    let (next, acts) = decide(&in_state(State::Briefing), &failed(BRIEF, &format!("x {}", dwarf::ops_sweep::STATUTE_MISSING))).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Idle, vec![LEDGER]));
}

#[test_id(tz4e5b3)]
#[test]
fn the_ledger_line_ends_in_why_and_ops_log_holds_the_whole_error() {
    let dir = testkit::TempDir::new("ops-ledger");
    let cfg = BTreeMap::from([("SPIRA_RUN".to_string(), dir.path().display().to_string())]);
    let act = LedgerLine { what: "done".into(), detail: "harness-red".into(), rc: Some(1), run_id: Some("r9".into()), why: Some("boom".into()), error: Some("boom\nmore".into()) };
    let input = serde_json::json!({ "cfg": cfg, "act": act }).to_string();
    let spec = runtime::EffectSpec::keyed(LEDGER, "k".to_string(), input, 5_000);
    run_ledger(&spira_runtime::Effects::default(), "k", &spec).unwrap();
    let ledger = std::fs::read_to_string(dir.path().join("dwarf-ledger.log")).unwrap();
    assert!(ledger.trim_end().ends_with(r#"why="boom""#), "{ledger}");
    assert!(ledger.contains("done ops sweep rc=1 status=harness-red "), "{ledger}");
    let log = std::fs::read_to_string(dir.path().join("ops.log")).unwrap();
    assert!(log.contains("spira: ops sweep r9 harness-red: boom\nmore"), "{log}");

    for ext in ["name", "system.md", "task.md", "log"] {
        std::fs::write(dir.path().join(format!("sweep-ops-r9.{ext}")), "x").unwrap();
    }
    let old = dir.path().join("sweep-ops-rOLD.log");
    std::fs::write(&old, "x").unwrap();
    std::fs::File::open(&old).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86400)).unwrap();
    let mut pruning = cfg.clone();
    pruning.insert("SPIRA_OPS_SWEEP_KEEP_DAYS".into(), "1".into());
    pruning.insert("SPIRA_RESULTS_PRUNE_MAX".into(), "50".into());
    let clean = runtime::EffectSpec::keyed(CLEANUP, "c".to_string(), serde_json::json!({ "cfg": pruning, "act": "r9" }).to_string(), 5_000);
    let said = run_cleanup(&spira_runtime::Effects::default(), "c", &clean).unwrap();
    assert!(said.starts_with("cleaned; pruned 1 "), "{said}");
    let mut left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("sweep-ops-")).collect();
    left.sort();
    assert_eq!(left, ["sweep-ops-r9.log"]);
}

#[test_id(t09j8ct)]
#[test]
fn cleanup_with_no_run_id_still_prunes_over_age_sweep_files() {
    let dir = testkit::TempDir::new("ops-cleanup-noid");
    let old = dir.path().join("sweep-ops-rOLD.log");
    std::fs::write(&old, "x").unwrap();
    std::fs::File::open(&old).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86400)).unwrap();
    let cfg = BTreeMap::from([("SPIRA_RUN", dir.path().display().to_string()), ("SPIRA_OPS_SWEEP_KEEP_DAYS", "1".into()), ("SPIRA_RESULTS_PRUNE_MAX", "50".into())]);
    let clean = runtime::EffectSpec::keyed(CLEANUP, "c".to_string(), serde_json::json!({ "cfg": cfg, "act": null }).to_string(), 5_000);
    run_cleanup(&spira_runtime::Effects::default(), "c", &clean).unwrap();
    assert!(!old.exists());
}

#[test_id(tjb6xeg)]
#[test]
fn cleanup_without_the_keep_days_key_errs_naming_it_after_removing_the_brief_files() {
    let dir = testkit::TempDir::new("ops-cleanup-nokey");
    let cfg = BTreeMap::from([("SPIRA_RUN".to_string(), dir.path().display().to_string()), ("SPIRA_RESULTS_PRUNE_MAX".to_string(), "50".to_string())]);
    for ext in ["name", "system.md", "task.md"] {
        std::fs::write(dir.path().join(format!("sweep-ops-r9.{ext}")), "x").unwrap();
    }
    let clean = runtime::EffectSpec::keyed(CLEANUP, "c".to_string(), serde_json::json!({ "cfg": cfg, "act": "r9" }).to_string(), 5_000);
    let err = run_cleanup(&spira_runtime::Effects::default(), "c", &clean).unwrap_err();
    assert!(err.contains("SPIRA_OPS_SWEEP_KEEP_DAYS"), "{err}");
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test_id(tqsna7j)]
#[test]
fn the_same_failing_line_three_times_asks_the_concierge_once() {
    let mut carry = in_state(State::Starting);
    let mut asks = vec![];
    for _ in 0..4 {
        let (red, acts) = decide(&carry, &failed(UNIT_START, "boom")).unwrap();
        asks.push(acts.iter().filter(|a| a.name == INBOX_APPEND).count());
        let (idle, _) = decide(&red, &Input::Succeeded { effect: CLEANUP.into(), body: String::new() }).unwrap();
        assert_eq!(idle.red, red.red, "the count survives the attempt's end");
        carry = Carry { state: State::Starting, ..idle };
    }
    assert_eq!(asks, vec![0, 0, 1, 0]);
    let (_, acts) = decide(&Carry { red: 3, red_why: Some("boom".into()), ..in_state(State::Starting) }, &failed(UNIT_START, "boom")).unwrap();
    assert!(!names(&acts).contains(&INBOX_APPEND));
    let (c, _) = decide(&carry, &failed(UNIT_START, "other")).unwrap();
    assert_eq!((c.red, c.red_why.as_deref()), (1, Some("other")));
    let (c, _) = decide(&Carry { red: 2, red_why: Some("boom".into()), ..in_state(State::Starting) }, &Input::Succeeded { effect: UNIT_START.into(), body: String::new() }).unwrap();
    assert_eq!((c.red, c.red_why), (0, None));
}

#[test_id(tqly41t)]
#[test]
fn the_concierge_line_is_keyed_on_the_failing_line_and_names_it() {
    let carry = Carry { red: 2, red_why: Some("boom".into()), ..in_state(State::Starting) };
    let (_, acts) = decide(&carry, &failed(UNIT_START, "boom")).unwrap();
    let ask = acts.iter().find(|a| a.name == INBOX_APPEND).unwrap();
    assert_eq!(ask.key, format!("ops/harness-red/{:x}", runtime::event::fnv1a("boom")));
    assert!(ask.input.contains("[persona.ops] the ops sweep has failed to start 3 times running: boom"), "{}", ask.input);
}

#[test_id(tltgunn)]
#[test]
fn three_failed_starts_through_the_handler_ask_once_with_a_stamped_raw_line_and_keep_the_count_as_rows() {
    let mut r = TaskRig::new(1, handler().registration()).unwrap().double(OBSERVE, |_| Ok(serde_json::to_string(&snap("SWEEP: go", 0, false, Pause::Open)).unwrap()));
    r = r.double(BRIEF, |_| Ok(r#"{"name":"ops-1","sys_file":"s.md","task_file":"t.md","argv":["claude"]}"#.into()));
    r = r.double(UNIT_START, |_| Err("boom\nmore".into()));
    for e in [INTAKE_INSTALL, LEDGER, CLEANUP, UNIT_STOP, INBOX_APPEND] {
        r = r.double(e, |i| Ok(i.into()));
    }
    let (_, stream, kind) = PersonaOps::boot();
    r.tick(stream, kind).unwrap();
    for _ in 0..3 {
        r.advance(CADENCE_MS).unwrap();
    }
    running(&r);
    let asks: Vec<serde_json::Value> = r.journaled().iter().filter(|e| e.kind.tag == runtime::event::EFFECT_REQUESTED).filter_map(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).ok()).filter(|b| b["spec"]["name"] == INBOX_APPEND).collect();
    assert_eq!(asks.len(), 1, "{asks:?}");
    let input = asks[0]["spec"]["input"].as_str().unwrap();
    assert!(input.contains("Z [persona.ops] the ops sweep has failed to start 3 times running: boom"), "{input}");
    assert!(!input.starts_with('{'), "the shared runner takes the raw line: {input}");
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.iter().any(|k| k == "red") && rows.iter().any(|k| k == "red_why"), "{rows:?}");
}

#[test_id(tw8aqsj)]
#[test]
fn a_snapshot_already_swept_is_skipped_and_a_new_one_sweeps() {
    let carry = Carry { state: State::Observing, swept_head: Some("## Spira pipeline, T1".into()), ..Default::default() };
    let (next, acts) = decide(&carry, &Input::Observed(snap("## Spira pipeline, T1", 0, false, Pause::Open))).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Idle, vec![]));
    assert_eq!(next.swept_head.as_deref(), Some("## Spira pipeline, T1"));
    let (next, acts) = decide(&carry, &Input::Observed(snap("## Spira pipeline, T2", 0, false, Pause::Open))).unwrap();
    assert_eq!((next.state, names(&acts)), (State::Preparing, vec![INTAKE_INSTALL]));
}

#[test_id(tcdaz68)]
#[test]
fn the_swept_head_is_recorded_only_when_the_unit_starts() {
    let observing = Carry { swept_head: Some("older".into()), ..in_state(State::Observing) };
    let (running, _) = decide(&observing, &Input::Observed(snap("## Spira pipeline, T1", 0, false, Pause::Open))).unwrap();
    assert_eq!(running.swept_head.as_deref(), Some("older"));
    let starting = Carry { state: State::Starting, ..running };
    let (ok, _) = decide(&starting, &Input::Succeeded { effect: UNIT_START.into(), body: String::new() }).unwrap();
    assert_eq!(ok.swept_head.as_deref(), Some("## Spira pipeline, T1"));
    let (bad, _) = decide(&starting, &Input::Failed { effect: UNIT_START.into(), error: "boom".into() }).unwrap();
    assert_eq!(bad.swept_head.as_deref(), Some("older"));
}

#[test_id(tz7gozp)]
#[test]
fn the_swept_head_survives_the_stored_rows_once_the_unit_has_started() {
    let r = mid_run();
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.iter().any(|k| k == "swept_head") && rows.iter().any(|k| k == "head"), "{rows:?}");
    let carried = spira_runtime::watchers::carried::<Carry>(r.engine().projection(), &StreamKey(STREAM.into()), STATE);
    assert_eq!(carried.swept_head.as_deref(), Some("a changed prompt head"));
}
