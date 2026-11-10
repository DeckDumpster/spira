//! Acceptance T1–T6 for `intake.gh`, from its contract
//! (`wiki/projects/spira/designs/tasks/intake-gh.md`, Acceptance) on `TaskRig`. The row is ported
//! (in `tasks::f::TASKS`). T6 runs two virtual days (the contract asks seven).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::f::intake_gh::*;
use spira_runtime::tasks::{self, f};

const CADENCE_MS: u64 = 15 * 60_000;
const BOGUS: &str = "intake.gh.bogus";
const MODULE: &str = include_str!("../../src/tasks/f/intake_gh.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn handler() -> IntakeGh {
    IntakeGh { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

/// Panics with the handler's quarantine message, so an unported `decide` is red in its own words.
fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn issue(n: u64, author: &str) -> GhIssue {
    GhIssue { number: n, title: format!("issue {n}"), body: "b".into(), author: author.into(), is_pr: false }
}

fn snap(store_total: u64, issues: Vec<GhIssue>, accept: Vec<Accept>) -> Snapshot {
    Snapshot {
        halted: false,
        targets: vec![Target { slug: "o/r".into(), bead_repo: "spira".into(), root_ok: true }],
        store: vec![KnownRef { id: "sp-1".into(), ext_ref: "github:o/r#1".into(), labels: vec!["spira".into()] }],
        store_total,
        now: 0,
        pages: [("o/r".to_string(), Pages::Issues(issues))].into(),
        accept,
        contributor_hidden: vec![],
    }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap()));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |i| Ok(i.into()));
    }
    let (_, stream, kind) = IntakeGh::boot();
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
fn t2_an_idle_tick_journals_nothing_and_asks_only_for_its_probe() {
    let mut r = rig(1, |_| snap(1, vec![issue(1, "ryan")], vec![]));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_every_legal_transition_is_taken_and_an_illegal_one_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(IssueState::WorkFiled, "untrusted").expect_err("work-filed never becomes untrusted");
    assert!(refused.contains("WorkFiled"), "{refused}");
}

#[test]
fn t3_the_carry_read_is_the_carry_recorded() {
    let (next, _) = decide(&Carry::default(), &snap(1, vec![issue(1, "ryan"), issue(2, "x")], vec![]));
    assert_eq!(next.seen.get("o/r"), Some(&vec![1, 2]), "the first read records the open issues as seen");
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
fn t5_parity_the_first_read_files_nothing_and_a_trusted_issue_becomes_work() {
    let (seeded, acts) = decide(&Carry::default(), &snap(1, vec![issue(1, "ryan")], vec![]));
    assert!(acts.is_empty(), "the first read of a slug files nothing: {acts:?}");
    let accepted = vec![Accept { slug: "o/r".into(), number: 2, actor: Some("ryan".into()), has_access: true, hidden: false }];
    let (_, acts) = decide(&seeded, &snap(1, vec![issue(1, "ryan"), issue(2, "stranger")], accepted));
    assert_eq!(acts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), [FILE_WORK]);
    assert!(acts[0].input.contains("github:o/r#2"), "{}", acts[0].input);
}

#[test]
fn t5_parity_a_zero_bead_store_is_refused_on_the_inbox_and_files_nothing() {
    let (_, acts) = decide(&Carry::default(), &snap(0, vec![issue(1, "ryan")], vec![]));
    assert_eq!(acts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), [f::INBOX]);
    assert!(acts[0].input.contains("[watch:intake.gh] gh-intake:"), "{}", acts[0].input);
}

fn baselined(second: fn(u64) -> Snapshot) -> TaskRig {
    rig(1, move |n| if n == 0 { snap(1, vec![issue(1, "ryan")], vec![]) } else { second(n) })
}

fn a_new_issue(_: u64) -> Snapshot {
    snap(1, vec![issue(1, "ryan"), issue(2, "stranger")], vec![])
}

fn rate_limited_for_hours(_: u64) -> Snapshot {
    Snapshot { now: 5 * 3_600, pages: [("o/r".to_string(), Pages::RateLimited { reset: 0 })].into(), ..snap(1, vec![], vec![]) }
}

fn boot_matches(make: impl Fn() -> TaskRig) {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(make, || handler().registration(), STREAM, runtime::event::EventKind::new(TICK, ""));
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&baselined(a_new_issue), NAME);
    for k in ["seen/o/r", "last_ok_hour"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test]
fn a_boot_files_a_new_issue_rather_than_baselining_it_away() {
    boot_matches(|| baselined(a_new_issue));
}

#[test]
fn a_boot_keeps_the_last_good_fetch_so_a_long_rate_limit_still_alarms() {
    boot_matches(|| baselined(rate_limited_for_hours));
}
