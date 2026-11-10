//! T1–T6 for `watch.ci_spill` (contract `wiki/projects/spira/designs/tasks/watch-ci-spill.md`).
//! The behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_ci_spill::*;
use spira_runtime::watchers::INBOX_APPEND;

const WF: &str = ".github/workflows/ci.yml";
const MISSING: &str = "jobs:\n  test:\n    steps:\n      - uses: org/actions/provision@v1\n        with:\n          size: large\n";
const COMPLIANT: &str = "jobs:\n  test:\n    steps:\n      - uses: org/actions/provision@v1\n        with:\n          spill: ec2-only\n";

fn config() -> Config {
    Config { repo_map: "/fixture/repos".into(), inbox: "/q/inbox".into() }
}

fn repo(sha: &str, workflow: &str) -> Repo {
    Repo { name: "spira".into(), ref_: Some("local/main".into()), sha: Some(sha.into()), workflows: Some(vec![(WF.into(), workflow.into())]) }
}

fn world(repos: Vec<Repo>) -> Arc<Mutex<Snapshot>> {
    Arc::new(Mutex::new(Snapshot { repos }))
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchCiSpill::boot();
    let mut r = TaskRig::new(seed, WatchCiSpill::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn said(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_fifteen_minute_tick() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_repos_at_their_seen_sha_journal_nothing() {
    let mut r = rig(1, world(vec![repo("a1", COMPLIANT)]));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_a_repo_is_seen_at_its_sha_and_a_violation_is_said_once() {
    let w = world(vec![repo("a1", MISSING)]);
    let mut r = rig(1, w.clone());
    assert_eq!(carry(&r).seen.get("spira").map(String::as_str), Some("a1"), "unseen → seen(sha)");
    assert_eq!(said(&r).len(), 1, "violation absent → said");
    *w.lock().unwrap() = Snapshot { repos: vec![repo("a2", MISSING)] };
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(said(&r).len(), 1, "said → said records nothing");
    assert_eq!(carry(&r).seen.get("spira").map(String::as_str), Some("a2"));
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_ci_spill.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.ci_spill.bogus") && !sim.contains(&"watch.ci_spill.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// The awk's finding and the two lines `spira/ci-spill-guard.sh:79,88,96` wrote.
#[test]
fn t5_the_lines_are_the_ones_ci_spill_guard_wrote() {
    assert!(violations(WF, COMPLIANT).is_empty());
    assert_eq!(violations(WF, MISSING).len(), 1);
    let r = rig(1, world(vec![repo("a1", MISSING)]));
    assert_eq!(said(&r), ["1970-01-01T00:00:00Z [watch:ci-spill-guard] CI SPILL: law-buy-capacity-rather-than-wait: spira:.github/workflows/ci.yml:4: provision has no spill: input; override: spill: ec2-only (at local/main)"]);
    let r = rig(1, world(vec![Repo { name: "deck".into(), ref_: None, sha: None, workflows: None }]));
    assert_eq!(said(&r), ["1970-01-01T00:00:00Z [watch:ci-spill-guard] CI SPILL: deck has no resolvable landing ref; its workflows are unchecked"]);
}

fn registration() -> runtime::event::Registration {
    WatchCiSpill::new(config()).registration()
}

fn violation_said() -> TaskRig {
    rig(1, world(vec![repo("a1", MISSING)]))
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&violation_said(), NAME);
    assert_eq!(rows, ["said", "seen/spira"]);
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(violation_said, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}
