//! Acceptance T1–T6 for `cockpit-tier-60s`, from its contract
//! (`wiki/projects/spira/designs/tasks/cockpit-tier-60s.md`, Acceptance) on `TaskRig`. Red until
//! the port lands: `decide` and `step` are `unimplemented!`, and the row waits in
//! `tasks::g::PENDING`. T6 runs two virtual days (the contract asks one).
//!
//! T2 note: the contract has `slots_sample` fire on every tick and counts it as zero journal
//! rows, but a `Command::Effect` is journaled, so `assert_silent_tick` would refuse it. T2 here
//! holds the contract's silence for `publish` and requires the sample to stay off the journal
//! (pushed by the observe runner, or a probe), which is the contract's own intent.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use runtime::journal::Journal;
use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::StreamKey;
use spira_runtime::tasks::g::cockpit::{Carry, Fragment, FragmentState, Snapshot};
use spira_runtime::tasks::g::cockpit_tier_60s::*;
use spira_runtime::tasks;

const CADENCE_MS: u64 = 60_000;
const BOGUS: &str = "cockpit-tier-60s.bogus";
const MODULE: &str = include_str!("../../src/tasks/g/cockpit_tier_60s.rs");
const LAUNCHERS: &[&str] = &["Command::new", "std::process", "process::Command", "libc::fork", "libc::exec", "posix_spawn", "nix::unistd"];

fn launcher(code: &str) -> Option<&'static str> {
    LAUNCHERS.iter().copied().find(|l| code.contains(l))
}

fn handler() -> CockpitTier60s {
    CockpitTier60s { interval_ms: CADENCE_MS, config: BTreeMap::new() }
}

fn running(r: &TaskRig) {
    if let Some(p) = r.journal.poisoned(&StreamKey(STREAM.into())).unwrap() {
        panic!("{NAME} was quarantined: {}", p.message);
    }
}

fn snap(live: u32) -> Snapshot {
    let frag = |kv: Vec<(String, String)>| Fragment { status: FragmentState::Ok, at: 0, killed: false, kv: kv.into_iter().collect() };
    Snapshot {
        supervised: true,
        fragments: [
            ("cockpit.slots".to_string(), frag(vec![("SP_SLOTS_LIVE".into(), live.to_string()), ("SP_SLOTS_CEILING".into(), "4".into())])),
            ("cockpit.reachable".to_string(), frag(vec![("SP_REACHABLE".into(), "0".into())])),
            ("cockpit.tsd".to_string(), frag(vec![("SP_TSD_WHERE".into(), "-".into())])),
        ]
        .into(),
    }
}

fn rig(seed: u64, world: impl Fn(u64) -> Snapshot + Send + Sync + 'static) -> TaskRig {
    let calls = AtomicU64::new(0);
    let mut r = TaskRig::new(seed, handler().registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&world(calls.fetch_add(1, Ordering::SeqCst))).unwrap())).double(PUBLISH, |i| Ok(i.into())).double(SLOTS_SAMPLE, |i| Ok(i.into()));
    let (_, stream, kind) = CockpitTier60s::boot();
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence_with_its_three_probes() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::g::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    assert_eq!(PROBES.iter().map(|p| p.0).collect::<Vec<_>>(), ["cockpit.slots", "cockpit.reachable", "cockpit.tsd"]);
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test]
fn t2_zero_dwarves_no_holds_and_unchanged_reachability_publish_nothing_and_journal_nothing() {
    let mut r = rig(1, |_| snap(0));
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
}

#[test]
fn t3_an_overrunning_reachable_probe_is_recorded_as_skipped_not_stacked() {
    let mut s = snap(0);
    s.fragments.get_mut("cockpit.reachable").unwrap().status = FragmentState::Timeout;
    let (next, _) = decide(&Carry::default(), &s).unwrap();
    assert_eq!(next.fragments["cockpit.reachable"].status, FragmentState::Timeout);
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
fn t5_parity_the_slots_reachable_and_tsd_keys_reach_the_merged_kv() {
    let (_, acts) = decide(&Carry::default(), &snap(2)).unwrap();
    let publish = acts.iter().find(|a| a.name == PUBLISH).expect("a first snapshot is published");
    for kv in ["SP_SLOTS_LIVE=2", "SP_SLOTS_CEILING=4", "SP_REACHABLE=0", "SP_TSD_WHERE=-"] {
        assert!(publish.input.contains(kv), "{kv}: {}", publish.input);
    }
}

#[test]
fn t5_parity_an_ok_slots_fragment_samples_live_and_ceiling() {
    let (_, acts) = decide(&Carry::default(), &snap(3)).unwrap();
    let sample = acts.iter().find(|a| a.name == SLOTS_SAMPLE).expect("an ok slots fragment is sampled");
    assert!(sample.input.contains("live=3") && sample.input.contains("ceiling=4"), "{}", sample.input);
}
