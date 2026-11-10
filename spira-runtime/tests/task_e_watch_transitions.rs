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
    Config { period_s: 300, working_min: 2, submit_bound_s: 90 * MIN, cut_bound_s: 45 * MIN, deliver_bound_s: 60 * MIN, activate_bound_s: 15 * MIN, ask_bound_s: 60 * MIN, release_repo: "spira".into(), heartbeat_bound_s: 120, releases: "/rel".into() }
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
        open_rounds: Some(BTreeMap::new()),
        owners: BTreeMap::new(),
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

fn edges(s: &Snapshot, now: u64) -> Vec<String> {
    stalls_of(s, now).into_iter().map(|x| x.key()).collect()
}

#[test]
fn work_that_entered_delivery_minutes_ago_after_a_long_quiet_spell_raises_no_stall() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-d1", "spira", "IN_DELIVERY", now - 4 * MIN)];
    s.last = last("spira", &[("Delivered", now - 2 * HOUR), ("Cut", now - 2 * MIN)]);
    assert!(!edges(&s, now).iter().any(|k| k.starts_with("IN_DELIVERY→LANDED")), "{:?}", edges(&s, now));
}

#[test]
fn work_in_delivery_past_the_bound_with_no_delivered_raises_the_stall() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-d1", "spira", "IN_DELIVERY", now - 61 * MIN)];
    s.last = last("spira", &[("Delivered", now - 3 * HOUR)]);
    assert!(edges(&s, now).contains(&"IN_DELIVERY→LANDED spira".to_string()));
}

#[test]
fn submitted_work_behind_a_full_set_of_open_rounds_is_queued_not_stalled() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-s1", "spira", "SUBMITTED", now - 50 * MIN)];
    s.last = last("spira", &[("Cut", now - 50 * MIN)]);
    s.open_rounds = Some(rounds_in("spira", 1));
    assert!(!edges(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "{:?}", edges(&s, now));
    s.open_rounds = Some(rounds_in("spira", 0));
    assert!(edges(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()), "a free round slot and no Cut is a stall");
    s.open_rounds = None;
    assert!(edges(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()), "rounds that cannot be counted do not excuse the wait");
}

#[test]
fn a_bead_that_leaves_delivery_for_landed_refreshes_the_last_delivered_time() {
    let now = T0;
    let mut before = healthy(now);
    before.beads = vec![bead("sp-d1", "-", "IN_DELIVERY", now - 3 * MIN), bead("sp-d2", "-", "IN_DELIVERY", now - 3 * HOUR)];
    before.last = last("-", &[("Delivered", now - 80 * MIN)]);
    let c = config();
    let (carry, _) = decide(&Carry::default(), &before, now * 1000, &c);
    let mut after = before.clone();
    after.beads.remove(0);
    let t2 = now + 5 * MIN;
    let (carry, acts) = decide(&carry, &after, t2 * 1000, &c);
    let pages: Vec<&str> = acts.iter().map(|a| a.input.as_str()).filter(|l| l.contains("P0 STALL")).collect();
    assert!(pages.is_empty(), "sp-d2 is 3h old but a bead landed this period: {pages:?}");
    assert!(carry.stalled.is_empty());
}

#[test]
fn a_bead_returned_to_rework_is_not_a_landing_and_a_blind_period_keeps_what_was_seen() {
    let now = T0;
    let c = config();
    let mut before = healthy(now);
    before.beads = vec![bead("sp-d1", "spira", "IN_DELIVERY", now - 3 * HOUR)];
    before.last = last("spira", &[("Delivered", now - 80 * MIN)]);
    let (carry, _) = decide(&Carry::default(), &before, now * 1000, &c);
    let (carry, _) = decide(&carry, &blind("down"), (now + 5 * MIN) * 1000, &c);
    let mut back = before.clone();
    back.beads = vec![bead("sp-d1", "spira", "REWORK", now)];
    let (carry, _) = decide(&carry, &back, (now + 10 * MIN) * 1000, &c);
    assert!(carry.delivered.is_empty(), "a bead still waiting elsewhere did not land: {carry:?}");
    let mut gone = before.clone();
    gone.beads.clear();
    let (carry, _) = decide(&Carry { in_delivery: [("sp-d1".to_string(), "spira".to_string())].into(), ..Carry::default() }, &gone, (now + 15 * MIN) * 1000, &c);
    assert_eq!(carry.delivered.get("spira"), Some(&(now + 15 * MIN)));
}

fn stuck_delivery(now: u64) -> Snapshot {
    let mut s = healthy(now);
    s.beads = vec![bead("sp-d1", "spira", "IN_DELIVERY", now - 2 * HOUR)];
    s.last = last("spira", &[("Claim", now - 10 * MIN), ("Submit", now - 5 * MIN), ("Cut", now - 2 * HOUR), ("Delivered", now - 3 * HOUR)]);
    s
}

fn owned(mut s: Snapshot, now: u64, beat_ago: u64, failed: Option<&str>) -> Snapshot {
    s.owners.insert(DELIVERY_EDGE.into(), Owner { beat: Some(now - beat_ago), failed: failed.map(str::to_string) });
    s
}

fn delivery_pages(s: &Snapshot, now: u64) -> usize {
    stalls_of(s, now).iter().filter(|x| x.edge == DELIVERY_EDGE).count()
}

#[test]
fn a_delivery_stall_with_the_round_beating_and_nothing_failed_is_not_paged() {
    let now = T0;
    assert_eq!(delivery_pages(&stuck_delivery(now), now), 1, "no owner heartbeat: paged as before");
    assert_eq!(delivery_pages(&owned(stuck_delivery(now), now, 30, None), now), 0, "a fresh heartbeat means the owner is handling it");
}

#[test]
fn a_delivery_stall_pages_when_the_round_heartbeat_has_gone_silent() {
    let now = T0;
    assert_eq!(delivery_pages(&owned(stuck_delivery(now), now, 121, None), now), 1);
}

#[test]
fn a_delivery_stall_pages_when_the_round_reports_a_failed_action_despite_a_fresh_beat() {
    let now = T0;
    assert_eq!(delivery_pages(&owned(stuck_delivery(now), now, 5, Some("stalled: no progress in 300s")), now), 1);
}

#[test]
fn a_round_progress_file_becomes_the_owner_and_a_stalled_detail_is_its_failure() {
    let ok = round_owner(r#"{"phase":"vm","detail":"VM 109 ready","updated_at":1700}"#).unwrap();
    assert_eq!(ok, Owner { beat: Some(1700), failed: None });
    let bad = round_owner(r#"{"detail":"stalled: no progress in 300s","updated_at":1800}"#).unwrap();
    assert_eq!(bad.failed.as_deref(), Some("stalled: no progress in 300s"));
    assert_eq!(round_owner(r#"{"detail":"x"}"#), None, "no updated_at is no heartbeat");
    assert_eq!(round_owner("not json"), None);
}

fn rounds_in(repo: &str, n: u64) -> BTreeMap<String, u64> {
    BTreeMap::from([(repo.to_string(), n)])
}

fn five_minute_cut_bound(s: &Snapshot, now: u64) -> Vec<String> {
    let cfg = Config { cut_bound_s: 5 * MIN, ..config() };
    spira_runtime::tasks::e::watch_transitions::stalls(s, now, &cfg).into_iter().map(|x| x.key()).collect()
}

fn submitted_ago(mins: u64, open: u64, cut_ago: u64) -> (Snapshot, u64) {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-s1", "spira", "SUBMITTED", now - mins * MIN)];
    s.last = last("spira", &[("Cut", now - cut_ago * MIN)]);
    s.open_rounds = Some(rounds_in("spira", open));
    (s, now)
}

#[test]
fn a_bead_submitted_two_minutes_ago_is_not_stalled_by_a_cut_six_minutes_ago() {
    let (s, now) = submitted_ago(2, 0, 6);
    assert!(!five_minute_cut_bound(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")));
}

#[test]
fn a_bead_submitted_ten_minutes_ago_behind_the_full_open_rounds_is_not_stalled() {
    let (s, now) = submitted_ago(10, 1, 6);
    assert!(!five_minute_cut_bound(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")));
}

#[test]
fn a_bead_submitted_ten_minutes_ago_with_no_round_open_is_stalled() {
    let (s, now) = submitted_ago(10, 0, 6);
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test]
fn a_certified_bead_past_the_bound_with_a_free_slot_is_stalled_even_after_a_recent_cut() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-c1", "spira", "CERTIFIED", now - 10 * MIN)];
    s.last = last("spira", &[("Cut", now - MIN)]);
    s.open_rounds = Some(rounds_in("spira", 0));
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test]
fn an_old_bead_is_not_stalled_just_after_the_round_ahead_of_it_closed() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-s1", "spira", "SUBMITTED", now - 3 * HOUR)];
    s.last = last("spira", &[("Delivered", now - 2 * MIN)]);
    s.open_rounds = Some(rounds_in("spira", 0));
    assert!(!edges(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "{:?}", edges(&s, now));
    s.last = last("spira", &[("Delivered", now - 46 * MIN)]);
    assert!(edges(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()), "the slot has been free past the bound");
}

#[test]
fn a_certified_bead_past_a_non_default_bound_is_quiet_for_that_bound_after_a_delivered() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-c1", "spira", "CERTIFIED", now - 3 * HOUR)];
    s.last = last("spira", &[("Delivered", now - 2 * MIN)]);
    s.open_rounds = Some(rounds_in("spira", 0));
    assert!(!five_minute_cut_bound(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")));
    s.last = last("spira", &[("Delivered", now - 6 * MIN)]);
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test]
fn a_submitted_bead_waiting_twenty_minutes_behind_an_open_round_is_queued_not_stalled() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-s1", "spira", "SUBMITTED", now - 20 * MIN)];
    s.last = last("spira", &[("Cut", now - 24 * MIN), ("Delivered", now - 3 * HOUR)]);
    s.open_rounds = Some(rounds_in("spira", 1));
    let cfg = Config { cut_bound_s: 5 * MIN, ..config() };
    let keys = |s: &Snapshot| spira_runtime::tasks::e::watch_transitions::stalls(s, now, &cfg).into_iter().map(|x| x.key()).collect::<Vec<_>>();
    assert!(!keys(&s).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "{:?}", keys(&s));
    s.open_rounds = Some(rounds_in("spira", 0));
    assert!(keys(&s).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()), "no round open and the wait past the bound pages");
}

/// A run dir whose only release activated at epoch second 100, and a releases dir whose
/// `current` link was switched just now.
fn stale_record_fresh_switch() -> (testkit::TempDir, testkit::TempDir, u64) {
    let run = testkit::TempDir::new("wt-run");
    let mut r = release::machine::Release::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    for (to, at) in [(release::machine::ReleaseState::Cut, 1), (release::machine::ReleaseState::Accepted, 2), (release::machine::ReleaseState::Published, 3), (release::machine::ReleaseState::Activated, 100)] {
        r.record(to, "t", at).unwrap();
    }
    release::machine::save(&run.path().join("release"), &r).unwrap();
    let rel = testkit::TempDir::new("wt-releases");
    std::os::unix::fs::symlink("bbbbbbbb", rel.path().join("current")).unwrap();
    let switched = std::fs::symlink_metadata(rel.path().join("current")).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    (run, rel, switched)
}

/// A dwell answer that reports `delivered` as a landing only when it is newer than `--landed-after`.
fn dwell_with_a_landing_at(delivered: u64) -> impl Fn(&[&str]) -> Result<serde_json::Value, String> {
    move |argv| {
        let after = argv.iter().position(|a| *a == "--landed-after").and_then(|i| argv[i + 1].parse::<u64>().ok()).unwrap_or(0);
        let landed = if delivered > after { serde_json::json!({ "spira": delivered }) } else { serde_json::json!({}) };
        Ok(serde_json::json!({ "beads": [], "horizon": 0, "last": { "spira": { "Delivered": delivered } }, "landed_after": landed }))
    }
}

#[test]
fn a_landing_under_the_release_in_effect_raises_no_stall_however_old_the_recorded_activation() {
    let (run, rel, switched) = stale_record_fresh_switch();
    let delivered = switched - 20 * MIN;
    let snap = observe(run.path(), rel.path(), &dwell_with_a_landing_at(delivered), 3_600);
    assert!(snap.readable, "{}", snap.why);
    let found = spira_runtime::tasks::e::watch_transitions::stalls(&snap, switched + 30 * MIN, &config());
    assert!(!found.iter().any(|s| s.edge == "LANDED→ACTIVATED"), "{found:?}");
}

#[test]
fn a_landing_after_the_release_in_effect_was_switched_in_raises_the_stall_past_the_bound() {
    let (run, rel, switched) = stale_record_fresh_switch();
    let snap = observe(run.path(), rel.path(), &dwell_with_a_landing_at(switched + 60), 3_600);
    let found = spira_runtime::tasks::e::watch_transitions::stalls(&snap, switched + 60 + 16 * MIN, &config());
    assert!(found.iter().any(|s| s.edge == "LANDED→ACTIVATED" && s.scope == "spira"), "{found:?}");
}

#[test]
fn a_round_open_in_the_repo_suppresses_its_stall_whatever_phase_it_has_reached() {
    let (s, now) = submitted_ago(20, 1, 24);
    assert!(!five_minute_cut_bound(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")));
}

#[test]
fn a_round_open_in_another_repo_does_not_suppress_this_repos_stall() {
    let (mut s, now) = submitted_ago(20, 0, 24);
    s.open_rounds = Some(rounds_in("other", 1));
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test]
fn the_all_repos_row_is_suppressed_when_its_only_submitted_work_is_behind_an_open_round() {
    let (s, now) = submitted_ago(20, 1, 24);
    let keys = five_minute_cut_bound(&s, now);
    assert!(!keys.iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "{keys:?}");
}

#[test]
fn the_all_repos_row_still_pages_when_no_round_is_open_and_work_waits_past_the_bound() {
    let (s, now) = submitted_ago(20, 0, 24);
    let cfg = Config { cut_bound_s: 5 * MIN, ..config() };
    let mut thin = s.clone();
    thin.beads.push(bead("sp-s2", "other", "SUBMITTED", now - 20 * MIN));
    thin.last.extend(last("other", &[("Cut", now - 24 * MIN)]));
    let all: Vec<String> = spira_runtime::tasks::e::watch_transitions::stalls(&thin, now, &cfg).into_iter().map(|x| x.key()).collect();
    assert!(all.contains(&"SUBMITTED→IN_DELIVERY spira".to_string()) && all.contains(&"SUBMITTED→IN_DELIVERY other".to_string()), "{all:?}");
    thin.open_rounds = Some(rounds_in("spira", 1));
    let one: Vec<String> = spira_runtime::tasks::e::watch_transitions::stalls(&thin, now, &cfg).into_iter().map(|x| x.key()).collect();
    assert!(!one.contains(&"SUBMITTED→IN_DELIVERY spira".to_string()) && one.contains(&"SUBMITTED→IN_DELIVERY other".to_string()), "{one:?}");
}
