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

const PROBE_DISPATCH: &str = include_str!("../../../cockpit-collect/src/probes/mod.rs");

type Tier = (&'static str, &'static [(&'static str, u64)], u64);

fn dispatched_probes() -> Vec<&'static str> {
    let run = PROBE_DISPATCH.split("pub fn run(subcommand").nth(1).expect("probes::run is in probes/mod.rs");
    let body = run.split("\n}\n").next().unwrap();
    body.lines().filter_map(|l| l.trim().strip_prefix('"')).filter(|l| l.contains("\" =>")).filter_map(|l| l.split('"').next()).collect()
}

#[test_id(t118la9)]
#[test]
fn every_registry_probe_runs_in_exactly_one_tier() {
    use spira_runtime::tasks::g::cockpit::{self, registry_name};
    use spira_runtime::tasks::g::{cockpit_tier_5s as t5, cockpit_tier_600s as t600};
    let registry = dispatched_probes();
    assert_eq!(registry.len(), 24, "the dispatch lists {registry:?}");
    let tiers: [Tier; 3] = [(t5::NAME, t5::PROBES, t5::OBSERVE_DEADLINE_MS), (NAME, PROBES, OBSERVE_DEADLINE_MS), (t600::NAME, t600::PROBES, t600::OBSERVE_DEADLINE_MS)];
    for name in &registry {
        let homes: Vec<&str> = tiers.iter().filter(|(_, probes, _)| probes.iter().any(|p| registry_name(p.0) == *name)).map(|t| t.0).collect();
        assert_eq!(homes.len(), 1, "probe {name} runs in {homes:?}");
    }
    for (tier, probes, deadline) in tiers {
        for (probe, timeout) in probes {
            assert!(registry.contains(&registry_name(probe)) || cockpit::view::is_view_probe(registry_name(probe)), "{tier} runs {probe}, which probes::run does not dispatch");
            assert!(*timeout <= deadline, "{tier} gives {probe} {timeout} ms, past the tier deadline {deadline}");
        }
    }
}

#[test_id(t6nzhdo)]
#[test]
fn t1_the_task_is_registered_boots_and_arms_its_cadence_with_its_registry_probes_and_the_slow_view() {
    let row = tasks::all().find(|t| t.name == NAME).unwrap_or_else(|| panic!("{NAME} is not in tasks::all(): its row waits in tasks::g::PENDING"));
    assert_eq!(row.stream(), STREAM);
    assert!(row.keys.iter().flat_map(|k| k.iter()).any(|k| *k == CADENCE_KEY), "the cadence is pinned config");
    assert_eq!(PROBES.iter().map(|p| p.0).collect::<Vec<_>>(), ["cockpit.slots", "cockpit.reachable", "cockpit.tsd", "cockpit.admission", "cockpit.well", "cockpit.repo_labels", "cockpit.strands", "cockpit.ratelim", "cockpit.core", "cockpit.queue", "cockpit.mail", "cockpit.goodput", "cockpit.view_slow"]);
    let mut r = TaskRig::new(1, handler().registration()).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
}

#[test_id(t553l03)]
#[test]
fn t2_zero_dwarves_no_holds_and_unchanged_reachability_publish_nothing_and_journal_nothing() {
    let mut r = rig(1, |_| snap(0));
    running(&r);
    assert_silent_tick(&mut r, STREAM);
    running(&r);
}

#[test_id(tgvi8nf)]
#[test]
fn t3_fragment_states_move_as_the_5s_tier_and_stale_before_ok_is_refused_naming_the_state() {
    for t in TRANSITIONS {
        assert_eq!(step(t.from, t.on), Ok(t.to), "{:?} on {}", t.from, t.on);
    }
    let refused = step(FragmentState::Never, "stale").expect_err("a fragment never read cannot go stale");
    assert!(refused.contains("Never"), "{refused}");
}

#[test_id(t16rjkt)]
#[test]
fn t3_an_overrunning_reachable_probe_is_recorded_as_skipped_not_stacked() {
    let mut s = snap(0);
    s.fragments.get_mut("cockpit.reachable").unwrap().status = FragmentState::Timeout;
    let (next, _) = decide(&Carry::default(), &s).unwrap();
    assert_eq!(next.fragments["cockpit.reachable"].status, FragmentState::Timeout);
    let back: Carry = serde_json::from_str(&serde_json::to_string(&next).unwrap()).unwrap();
    assert_eq!(back, next, "the carry read is the carry recorded");
}

#[test_id(tciv5o6)]
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

#[test_id(tiscure)]
#[test]
fn t4_the_module_launches_no_process() {
    for l in LAUNCHERS {
        assert_eq!(launcher(&format!("{l}(\"x\")")), Some(*l), "the scan must see {l}");
    }
    assert_eq!(launcher(MODULE), None);
}

#[test_id(tcevy1j)]
#[test]
fn t5_parity_the_slots_reachable_and_tsd_keys_reach_the_merged_kv() {
    let (_, acts) = decide(&Carry::default(), &snap(2)).unwrap();
    let publish = acts.iter().find(|a| a.name == PUBLISH).expect("a first snapshot is published");
    for kv in ["SP_SLOTS_LIVE=2", "SP_SLOTS_CEILING=4", "SP_REACHABLE=0", "SP_TSD_WHERE=-"] {
        assert!(publish.input.contains(kv), "{kv}: {}", publish.input);
    }
}

#[test_id(txq02y8)]
#[test]
fn t5_parity_an_ok_slots_fragment_samples_live_and_ceiling() {
    let (_, acts) = decide(&Carry::default(), &snap(3)).unwrap();
    let sample = acts.iter().find(|a| a.name == SLOTS_SAMPLE).expect("an ok slots fragment is sampled");
    assert!(sample.input.contains("live=3") && sample.input.contains("ceiling=4"), "{}", sample.input);
    assert!(!sample.input.contains("ready="), "ready is the claim view's, read when the sample runs: {}", sample.input);
}

fn with_clock(mut s: Snapshot) -> Snapshot {
    let f = s.fragments.values_mut().next().unwrap();
    f.at = 10;
    f.kv.insert("SP_AT".into(), "10".into());
    s
}

fn boot_matches(make: impl Fn() -> TaskRig) {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(make, || handler().registration(), STREAM, runtime::event::EventKind::new(TICK, ""));
}

#[test_id(tajsnbo)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig(1, |_| with_clock(snap(0))), NAME);
    for k in ["digest", "fragments/cockpit.slots"] {
        assert!(rows.iter().any(|r| r == k), "{k} missing from {rows:?}");
    }
}

#[test_id(txcdn6j)]
#[test]
fn a_boot_does_not_republish_a_snapshot_whose_digest_it_published() {
    boot_matches(|| rig(1, |_| snap(0)));
}

#[test_id(twl50mv)]
#[test]
fn a_boot_does_not_refresh_a_snapshot_no_newer_than_the_last_read() {
    boot_matches(|| rig(1, |_| with_clock(snap(0))));
}

fn aged(rows: &[(&str, &str)], at: u64) -> Snapshot {
    let mut s = snap(0);
    let f = s.fragments.get_mut("cockpit.reachable").unwrap();
    f.at = at;
    f.kv.extend(rows.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    s
}

fn age_rows(minutes: &str, mail_age: &str) -> Vec<(&'static str, String)> {
    vec![("SP_INFLOW0", format!("{minutes}   task a")), ("SP_EVENT0", format!("{minutes}   ev a")), ("SP_MAIL0", format!("'{mail_age}|READ|hi")), ("SP_INFLOW0_REPO", "r".into())]
}

fn second_pass(first: Vec<(&'static str, String)>, second: Vec<(&'static str, String)>) -> (TaskRig, usize, usize) {
    let pick = |rows: &Vec<(&'static str, String)>, at| aged(&rows.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>(), at);
    let (a, b) = (pick(&first, 10), pick(&second, 20));
    let mut r = rig(1, move |n| if n == 0 { a.clone() } else { b.clone() });
    let (turns, asked) = (r.trace().len(), r.requested().len());
    let (_, stream, kind) = CockpitTier60s::boot();
    r.tick(stream, kind).unwrap();
    (r, turns, asked)
}

#[test_id(tmzn1ls)]
#[test]
fn a_pass_differing_only_in_row_ages_commits_no_turn_and_publishes_the_new_ages_off_the_journal() {
    let (r, turns, asked) = second_pass(age_rows("4m", "20218"), age_rows("5m", "20278"));
    assert_eq!(r.trace().len(), turns, "an age ticking over journaled a turn");
    let new = &r.requested()[asked..];
    let publish = new.iter().find(|q| q.name == PUBLISH).expect("the ages are published as a probe");
    assert!(!publish.journaled, "{publish:?}");
    assert!(publish.input.contains("SP_INFLOW0=5m") && publish.input.contains("SP_EVENT0=5m") && publish.input.contains("SP_MAIL0='20278|READ"), "{}", publish.input);
}

#[test_id(t9k2q4w)]
#[test]
fn a_changed_title_a_new_row_a_read_flip_or_a_repo_change_publishes_journalled() {
    let base = || age_rows("4m", "20218");
    let edit = |k: &str, v: &str| -> Vec<(&'static str, String)> { base().into_iter().map(|(rk, rv)| if rk == k { (rk, v.to_string()) } else { (rk, rv) }).collect() };
    let mut added = base();
    added.push(("SP_INFLOW1", "4m   task b".into()));
    let cases = [edit("SP_INFLOW0", "4m   task Z"), edit("SP_EVENT0", "4m   ev Z"), edit("SP_MAIL0", "'20218|UNREAD|hi"), edit("SP_INFLOW0_REPO", "other"), added];
    for second in cases {
        let (r, turns, asked) = second_pass(base(), second.clone());
        assert!(r.trace().len() > turns, "{second:?} committed no turn");
        let published: Vec<_> = r.requested()[asked..].iter().filter(|q| q.name == PUBLISH).collect();
        assert_eq!(published.len(), 1, "{second:?}");
        assert!(published[0].journaled, "{second:?}");
    }
}
