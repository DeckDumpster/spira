//! Tests for `watch.transitions`: the period's wait-time table, the STALL page when a state holds
//! work and no exit transition happens inside its bound, and BLIND when the store cannot be read.
//! Each runs on the task rig's virtual clock against a fake store answer.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_transitions::*;
use spira_runtime::watchers::INBOX_APPEND;

const MIN: u64 = 60;
const HOUR: u64 = 3_600;
/// The rig's clock is moved here before the first tick; the fake store's times are written
/// relative to it and shifted with the clock each period.
const T0: u64 = 10 * 86_400;

fn config() -> Config {
    Config { period_s: 300, working_min: 2, submit_bound_s: 90 * MIN, cut_bound_s: 45 * MIN, deliver_bound_s: 60 * MIN, activate_bound_s: 15 * MIN, ask_bound_s: 60 * MIN, release_repo: "spira".into() }
}

fn bead(id: &str, repo: &str, state: &str, since: u64) -> Bead {
    Bead { id: id.into(), repo: repo.into(), state: state.into(), since, ask_held: false, ask_since: None }
}

fn last(repo: &str, kinds: &[(&str, u64)]) -> BTreeMap<String, BTreeMap<String, u64>> {
    BTreeMap::from([(repo.to_string(), kinds.iter().map(|(k, t)| (k.to_string(), *t)).collect())])
}

/// Work moving through every state, every exit seen minutes ago, the release current.
fn healthy(now: u64) -> Snapshot {
    Snapshot {
        readable: true,
        why: String::new(),
        horizon: now - 6 * HOUR,
        beads: vec![bead("sp-r1", "spira", "READY", now - 20 * MIN), bead("sp-w1", "spira", "WORKING", now - 40 * MIN), bead("sp-w2", "spira", "WORKING", now - 10 * MIN), bead("sp-w3", "spira", "WORKING", now - 70 * MIN), bead("sp-s1", "spira", "SUBMITTED", now - 5 * MIN), bead("sp-d1", "spira", "IN_DELIVERY", now - 15 * MIN)],
        last: last("spira", &[("Claim", now - 10 * MIN), ("Submit", now - 5 * MIN), ("Cut", now - 15 * MIN), ("Delivered", now - 20 * MIN)]),
        activated_at: Some(now - 10 * MIN),
        landed_after: BTreeMap::new(),
    }
}

/// 2026-11-07: six builders WORKING, and the last Submit three hours ago.
fn todays_case(now: u64) -> Snapshot {
    let mut s = healthy(now);
    s.beads = (1..=6).map(|i| bead(&format!("sp-w{i}"), "spira", "WORKING", now - (30 + 20 * i) * MIN)).collect();
    s.last = last("spira", &[("Claim", now - 20 * MIN), ("Submit", now - 3 * HOUR), ("Cut", now - 3 * HOUR), ("Delivered", now - 2 * HOUR)]);
    s
}

fn rig(world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchTransitions::boot();
    let mut r = TaskRig::new(1, WatchTransitions::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(INBOX_APPEND, |_| Ok("appended".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn lines(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn stalls(r: &TaskRig) -> Vec<String> {
    lines(r).into_iter().filter(|l| l.contains("P0 STALL")).collect()
}

fn rig_at(make: impl Fn(u64) -> Snapshot) -> (TaskRig, Arc<Mutex<Snapshot>>) {
    let w = Arc::new(Mutex::new(make(T0)));
    let (_, stream, kind) = WatchTransitions::boot();
    let w2 = w.clone();
    let mut r = TaskRig::new(1, WatchTransitions::new(config()).registration()).unwrap().double(OBSERVE, move |input| {
        let now = serde_json::from_str::<serde_json::Value>(input).unwrap()["now"].as_u64().unwrap();
        let mut s = w2.lock().unwrap().clone();
        let shift = |t: u64| t + now - T0;
        s.horizon = shift(s.horizon);
        s.beads.iter_mut().for_each(|b| b.since = shift(b.since));
        s.last.values_mut().flat_map(|k| k.values_mut()).for_each(|t| *t = shift(*t));
        s.activated_at = s.activated_at.map(shift);
        Ok(serde_json::to_string(&s).unwrap())
    })
    .double(INBOX_APPEND, |_| Ok("appended".into()));
    r.advance(T0 * 1000).unwrap();
    r.tick(stream, kind).unwrap();
    (r, w)
}

#[test]
fn six_working_and_no_submit_for_three_hours_raises_a_stall_naming_working_to_submitted_in_one_period() {
    let (r, _) = rig_at(todays_case);
    let pages = stalls(&r);
    let working: Vec<&String> = pages.iter().filter(|l| l.contains("STALL WORKING→SUBMITTED")).collect();
    assert_eq!(working.len(), 1, "spira's, and no echo of it for all repos: {pages:?}");
    assert!(working[0].contains("(spira)"), "{pages:?}");
    let page = working[0];
    assert!(page.contains("WORKING 6 oldest 2h30m"), "the evidence carries the count and ages: {page}");
    assert!(page.contains("no Submit for 3h00m (bound 1h30m)"), "{page}");
    assert!(page.contains("last: Claim "), "the evidence carries the last-event times: {page}");
    assert!(lines(&r).iter().any(|l| l.contains("WAIT ") && l.contains("WORKING 6")), "the period's table line is written too");
}

#[test]
fn a_stall_pages_once_while_it_stands_and_says_when_it_clears() {
    let (mut r, w) = rig_at(todays_case);
    let first = stalls(&r).len();
    r.advance(3 * 300_000).unwrap();
    assert_eq!(stalls(&r).len(), first, "a standing stall is not paged again");
    *w.lock().unwrap() = healthy(T0);
    r.advance(300_000).unwrap();
    assert!(lines(&r).iter().any(|l| l.contains("STALL CLEARED WORKING→SUBMITTED spira")), "{:?}", lines(&r));
}

#[test]
fn a_healthy_flow_raises_no_stall() {
    let (mut r, _) = rig_at(healthy);
    r.advance(12 * 300_000).unwrap();
    assert_eq!(stalls(&r), Vec::<String>::new());
    let tables = lines(&r).into_iter().filter(|l| l.contains("[watch:transitions] WAIT ")).count();
    assert_eq!(tables, 13, "one table line per period");
}

#[test]
fn an_unreadable_store_reports_blind_and_pages_never_clear() {
    let w = Arc::new(Mutex::new(blind("spira-lc dwell: cannot tell: the control store: locked")));
    let mut r = rig(w);
    r.advance(4 * 300_000).unwrap();
    let all = lines(&r);
    let pages: Vec<&String> = all.iter().filter(|l| l.contains("P0 BLIND")).collect();
    assert_eq!(pages.len(), 1, "BLIND pages once: {all:?}");
    assert!(pages[0].contains("this is not an all-clear"), "{pages:?}");
    assert!(all.iter().filter(|l| !l.contains("P0 BLIND")).all(|l| l.contains("WAIT BLIND")), "every period says BLIND, never a table: {all:?}");
    assert_eq!(stalls(&r), Vec::<String>::new());
}

#[test]
fn an_observe_that_fails_is_blind_too() {
    let (_, stream, kind) = WatchTransitions::boot();
    let mut r = TaskRig::new(1, WatchTransitions::new(config()).registration()).unwrap().double(OBSERVE, |_| Err("lc-serve: connection refused".into())).double(INBOX_APPEND, |_| Ok("appended".into()));
    r.tick(stream, kind).unwrap();
    let all = lines(&r);
    assert!(all.iter().any(|l| l.contains("P0 BLIND") && l.contains("connection refused")), "{all:?}");
}

#[test]
fn each_bound_is_judged_on_its_own_edge() {
    let now = T0;
    let mut s = healthy(now);
    s.beads.push(bead("sp-c1", "deckdumpster", "CERTIFIED", now - 2 * HOUR));
    s.last.insert("deckdumpster".into(), [("Cut".to_string(), now - 50 * MIN)].into());
    let mut asked = bead("sp-q1", "spira", "READY", now - 3 * HOUR);
    asked.ask_held = true;
    asked.ask_since = Some(now - 61 * MIN);
    s.beads.push(asked);
    s.landed_after.insert("spira".into(), now - 16 * MIN);
    s.beads.push(bead("sp-d9", "pokedumpster", "IN_DELIVERY", now - 2 * HOUR));
    let found: Vec<String> = stalls_of(&s, now).into_iter().map(|x| x.key()).collect();
    for want in ["SUBMITTED→IN_DELIVERY deckdumpster", "ASK→ANSWERED spira", "LANDED→ACTIVATED spira", "IN_DELIVERY→LANDED pokedumpster"] {
        assert!(found.iter().any(|k| k == want), "{want} missing from {found:?}");
    }
    assert!(!found.iter().any(|k| k.starts_with("WORKING→SUBMITTED")), "{found:?}");
    assert!(!found.iter().any(|k| k == "SUBMITTED→IN_DELIVERY spira"), "spira cut 15m ago: {found:?}");
}

fn stalls_of(s: &Snapshot, now: u64) -> Vec<Stall> {
    spira_runtime::tasks::e::watch_transitions::stalls(s, now, &config())
}

#[test]
fn a_kind_never_seen_back_to_the_horizon_counts_as_at_least_the_horizons_age() {
    let now = T0;
    let mut s = healthy(now);
    s.last.clear();
    s.horizon = now - 2 * HOUR;
    let found: Vec<String> = stalls_of(&s, now).into_iter().map(|x| x.key()).collect();
    assert!(found.contains(&"WORKING→SUBMITTED spira".to_string()), "{found:?}");
    s.horizon = now - 10 * MIN;
    assert!(stalls_of(&s, now).iter().all(|x| x.edge != "WORKING→SUBMITTED"), "a short horizon proves nothing");
}

#[test]
fn the_table_has_count_oldest_and_percentiles_per_state() {
    let now = T0;
    let t = table(&healthy(now), ALL, now);
    assert_eq!((t["WORKING"].count, t["WORKING"].oldest, t["WORKING"].p50, t["WORKING"].p90), (3, 70 * MIN, 40 * MIN, 70 * MIN));
    assert_eq!(t["REWORK"].count, 0);
    let line = table_line(&healthy(now), now, &config());
    assert!(line.starts_with("WAIT READY 1 oldest 20m"), "{line}");
    assert!(line.contains("spira: claim 10m submit 5m cut 15m delivered 20m activated 10m"), "{line}");
}

#[test]
fn a_dwell_answer_becomes_the_snapshot_with_deliver_tabled_as_cut() {
    let dwell = serde_json::json!({
        "rev": 9, "now": 5000, "window_s": 600, "horizon": 4400, "complete": true, "scanned": 3,
        "beads": [{ "id": "sp-a", "repo": "spira", "state": "WORKING", "since": 4000, "ask_held": false }],
        "last": { "spira": { "Claim": 4000, "Deliver": 4100, "Delivered": 4200 } },
        "landed_after": { "spira": 4200 }
    });
    let s = snapshot_of(&dwell, Some(4150)).unwrap();
    assert!(s.readable);
    assert_eq!(s.last["spira"].get("Cut"), Some(&4100));
    assert_eq!(s.landed_after.get("spira"), Some(&4200));
    assert_eq!(s.beads[0].since, 4000);
    assert!(snapshot_of(&serde_json::json!({ "error": "x" }), None).is_err(), "a malformed answer is an error, so the observe is BLIND");
}

#[test]
fn the_row_is_registered_and_its_boot_arms_the_configured_period() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), 300_000);
    assert_eq!(Config::pinned().map(|c| c.period_s), Ok(300), "every key resolves through the config door");
}

#[test]
fn every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_transitions.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test]
fn two_days_replay_identically() {
    let r = assert_deterministic(|_| rig(Arc::new(Mutex::new(healthy(T0)))), 7, 2);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn work_spread_thinly_across_repos_stalls_as_all_repos() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-a", "alpha", "WORKING", now - 2 * HOUR), bead("sp-b", "beta", "WORKING", now - 2 * HOUR)];
    s.last = last("alpha", &[("Submit", now - 2 * HOUR)]);
    let found: Vec<String> = stalls_of(&s, now).into_iter().map(|x| x.key()).collect();
    assert_eq!(found, ["WORKING→SUBMITTED *"], "one WORKING each is under the per-repo floor; together they are two");
}
