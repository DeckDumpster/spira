//! Acceptance T1–T6 for `cockpit-tier-5s`, from its contract
//! (`wiki/projects/spira/designs/tasks/cockpit-tier-5s.md`, Acceptance) on `TaskRig`. Red until
//! the port lands: `decide` and `step` are `unimplemented!`, and the row waits in
//! `tasks::g::PENDING`. The three sub-probes ride one observe, so T2's one-probe rule holds. T6 runs
//! two virtual days (the contract asks one).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::g::cockpit::{Carry, Fragment, FragmentState, Snapshot};
use spira_runtime::tasks::g::cockpit_tier_5s::*;
use spira_runtime::tasks;

const CADENCE_MS: u64 = 5_000;
const BOGUS: &str = "cockpit-tier-5s.bogus";
const MODULE: &str = include_str!("../../src/tasks/g/cockpit_tier_5s.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn handler() -> CockpitTier5s {
    CockpitTier5s { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(supervised: bool, round: &str) -> Snapshot {
    let frag = |kv: &[(&str, &str)]| Fragment { status: FragmentState::Ok, at: 0, killed: false, kv: kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    Snapshot {
        supervised,
        fragments: [("cockpit.now".to_string(), frag(&[("SP_DWARVES", "0")])), ("cockpit.round".to_string(), frag(&[("SP_ROUND", round)])), ("cockpit.inflight".to_string(), frag(&[("SP_INFLIGHT", "")]))].into(),
    }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(PUBLISH, |i| Ok(i.into()));
    let (_, stream, kind) = CockpitTier5s::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence_with_its_three_probes() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::g::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    assert_eq!(PROBES.iter().map(|p| p.0).collect::<Vec<_>>(), ["cockpit.now", "cockpit.round", "cockpit.inflight"]);
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn t2_an_unchanged_snapshot_publishes_nothing_and_journals_nothing() {
    let mut r = rig(1, |_| snap(true, ""));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_never_ok_stale_error_timeout_ok_is_legal_and_stale_before_ok_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(FragmentState::Never, "stale").expect_err("a fragment never read cannot go stale");
    assert!(refused.contains("Never"), "{refused}");
}

#[test]
fn t3_a_publish_from_an_unsupervised_instance_is_refused_naming_may_write() {
    let refused = decide(&Carry::default(), &snap(false, "r1")).expect_err("only the supervised instance writes");
    assert!(refused.contains("may_write"), "{refused}");
    let (next, _) = decide(&Carry::default(), &snap(true, "r1")).unwrap();
    let back: Carry = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(back, next, "the carry read is the carry recorded");
}

#[test]
fn t4_every_effect_has_a_real_runner_and_a_sim_double() {
    let row = tasks::all().find(|t| t.name == NAME).expect("the row is ported");
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
fn t5_parity_the_merged_kv_carries_every_fragments_keys() {
    let (_, acts) = decide(&Carry::default(), &snap(true, "r1")).unwrap();
    assert_eq!(acts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), [PUBLISH]);
    for kv in ["SP_DWARVES=0", "SP_ROUND=r1", "SP_INFLIGHT="] {
        assert!(acts[0].input.contains(kv), "{kv}: {}", acts[0].input);
    }
}

#[test]
fn t5_parity_an_unchanged_digest_is_not_published_again() {
    let (published, _) = decide(&Carry::default(), &snap(true, "r1")).unwrap();
    let (_, acts) = decide(&published, &snap(true, "r1")).unwrap();
    assert!(acts.is_empty(), "{acts:?}");
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_publish_at_most_once_per_distinct_snapshot() {
    let world = |call: u64| snap(true, &format!("r{}", call / 720));
    let a = assert_deterministic(|seed| rig(seed, world), 1, 2);
    running(&a);
    let publishes = a.requested().iter().filter(|q| q.name == PUBLISH).count();
    assert!(publishes <= 1 + (2 * 86_400_000 / CADENCE_MS / 720) as usize, "{publishes} publishes");
    assert_journal_budget(&a, 10);
}
