//! Acceptance T1–T6 for `cockpit-tier-600s`, from its contract
//! (`wiki/projects/spira/designs/tasks/cockpit-tier-600s.md`, Acceptance) on `TaskRig`. Red until
//! the port lands: `decide` and `step` are `unimplemented!`, and the row waits in
//! `tasks::g::PENDING`. T6 runs two virtual days with the dirty file on day 2 (the contract asks
//! seven). T3's "second slow probe waits for the class permit" is a pool property (S3) once both
//! probes ride one observe; it is not tested here.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, DAY_MS};
use runtime::StreamKey;
use spira_runtime::tasks::g::cockpit::{Carry, Fragment, FragmentState, Snapshot};
use spira_runtime::tasks::g::cockpit_tier_600s::*;
use spira_runtime::tasks::{self, g};

const CADENCE_MS: u64 = 600_000;
const BOGUS: &str = "cockpit-tier-600s.bogus";
const MODULE: &str = include_str!("../src/tasks/g/cockpit_tier_600s.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn handler() -> CockpitTier600s {
    CockpitTier600s { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(drift: &str) -> Snapshot {
    let frag = |kv: &[(&str, &str)]| Fragment { status: FragmentState::Ok, at: 0, killed: false, kv: kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
    Snapshot {
        supervised: true,
        fragments: [
            ("cockpit.livelock".to_string(), frag(&[("SP_LIVELOCK", "0"), ("SP_INVALID_CLOSED", "0"), ("SP_UNFILED_FOLLOW", "0")])),
            ("cockpit.drift".to_string(), frag(&[("SP_CHECKOUT_DRIFT", drift), ("SP_UNIT_DRIFT", "clean")])),
        ]
        .into(),
    }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(PUBLISH, |i| Ok(i.into()));
    let (_, stream, kind) = CockpitTier600s::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence_with_its_two_probes() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::g::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    assert_eq!(PROBES.iter().map(|p| p.0).collect::<Vec<_>>(), ["cockpit.livelock", "cockpit.drift"]);
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn t2_a_clean_checkout_shipped_units_and_no_livelocks_publish_nothing_and_journal_nothing() {
    let mut r = rig(1, |_| snap("clean"));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test]
fn t3_fragment_states_move_as_the_5s_tier_and_stale_before_ok_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(FragmentState::Never, "stale").expect_err("a fragment never read cannot go stale");
    assert!(refused.contains("Never"), "{refused}");
    let (next, _) = decide(&Carry::default(), &snap("clean")).unwrap();
    let back: Carry = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(back, next, "the carry read is the carry recorded");
}

#[test]
fn t4_every_effect_has_a_real_runner_and_a_sim_double() {
    let row = g::PENDING.iter().find(|t| t.name == NAME).expect("the row is pending");
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
fn t5_parity_the_drift_and_livelock_keys_reach_the_merged_kv() {
    let (_, acts) = decide(&Carry::default(), &snap("dirty:2")).unwrap();
    assert_eq!(acts.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), [PUBLISH]);
    for kv in ["SP_CHECKOUT_DRIFT=dirty:2", "SP_UNIT_DRIFT=clean", "SP_LIVELOCK=0", "SP_INVALID_CLOSED=0", "SP_UNFILED_FOLLOW=0"] {
        assert!(acts[0].input.contains(kv), "{kv}: {}", acts[0].input);
    }
}

#[test]
fn t5_parity_a_detector_that_answers_while_the_store_does_not_reads_unknown_not_zero() {
    let mut s = snap("clean");
    s.fragments.get_mut("cockpit.livelock").unwrap().status = FragmentState::Error;
    let (_, acts) = decide(&Carry::default(), &s).unwrap();
    let publish = acts.iter().find(|a| a.name == PUBLISH).expect("a first snapshot is published");
    assert!(publish.input.contains("SP_LIVELOCK=?"), "{}", publish.input);
}

#[test]
fn t6_a_dirty_file_on_day_two_publishes_once_then_and_two_days_are_deterministic() {
    let per_day = DAY_MS / CADENCE_MS;
    let world = move |call: u64| snap(if call >= per_day + 10 { "dirty:1" } else { "clean" });
    let a = assert_deterministic(|seed| rig(seed, world), 1, 2);
    running(&a);
    let publishes = a.requested().iter().filter(|q| q.name == PUBLISH).count();
    assert_eq!(publishes, 2, "the first snapshot, then the dirty file");
    assert_journal_budget(&a, 1);
}
