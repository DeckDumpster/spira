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
    Config {
        period_s: 300,
        working_min: 2,
        submit_bound_s: 90 * MIN,
        cut_bound_s: 45 * MIN,
        deliver_bound_s: 60 * MIN,
        activate_bound_s: 15 * MIN,
        ask_bound_s: 60 * MIN,
        release_repo: "spira".into(),
        heartbeat_bound_s: 120,
        round_max_open: 1,
        releases: "/rel".into(),
        repo_map: "/map".into(),
    }
}

fn bead(id: &str, repo: &str, state: &str, since: u64) -> Bead {
    Bead { id: id.into(), repo: repo.into(), state: state.into(), since, ask_held: false, ask_since: None, held: false, tip: None, landed: false }
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
        halt: None,
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
        "landed_after": {}
    });
    let s = snapshot_of(
        &dwell,
        Some(4150),
        BTreeMap::from([("spira".to_string(), 4200)]),
    )
    .unwrap();
    assert!(s.readable);
    assert_eq!(s.last["spira"].get("Cut"), Some(&4100));
    assert_eq!(s.landed_after.get("spira"), Some(&4200));
    assert_eq!(s.beads[0].since, 4000);
    assert!(
        snapshot_of(&serde_json::json!({ "error": "x" }), None, BTreeMap::new()).is_err(),
        "a malformed answer is an error, so the observe is BLIND"
    );
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

#[test]
fn submitted_beads_with_no_recorded_repo_wait_behind_a_full_round_in_a_named_repo() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-s1", "-", "SUBMITTED", now - 7 * MIN), bead("sp-s2", "-", "SUBMITTED", now - 5 * MIN)];
    s.last = BTreeMap::from([("-".to_string(), BTreeMap::from([("Cut".to_string(), now - 9 * MIN)])), ("spira".to_string(), BTreeMap::from([("Deliver".to_string(), now - 9 * MIN)]))]);
    let rows = [serde_json::json!({"repo": "spira", "state": "CI_RUNNING"}), serde_json::json!({"repo": "spira", "state": "LANDED"})];
    s.open_rounds = Some(spira_runtime::world::open_rounds_by_repo(&rows));
    assert!(five_minute_cut_bound(&s, now).is_empty(), "{:?}", five_minute_cut_bound(&s, now));
    s.open_rounds = Some(rounds_in("spira", 0));
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY -".to_string()), "no round open: the wait pages");
}

const IN_EFFECT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LANDED: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// A run dir whose only release activated at epoch second 100, and a releases dir whose
/// `current` names a release built from [`IN_EFFECT`].
fn stale_record_other_release_in_effect() -> (testkit::TempDir, testkit::TempDir) {
    let run = testkit::TempDir::new("wt-run");
    let mut r = release::machine::Release::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    for (to, at) in [(release::machine::ReleaseState::Cut, 1), (release::machine::ReleaseState::Accepted, 2), (release::machine::ReleaseState::Published, 3), (release::machine::ReleaseState::Activated, 100)] {
        r.record(to, "t", at).unwrap();
    }
    release::machine::save(&run.path().join("release"), &r).unwrap();
    let rel = testkit::TempDir::new("wt-releases");
    std::fs::create_dir(rel.path().join("rel-b")).unwrap();
    std::fs::write(
        rel.path().join("rel-b/MANIFEST"),
        format!("commit {IN_EFFECT}\n"),
    )
    .unwrap();
    std::os::unix::fs::symlink("rel-b", rel.path().join("current")).unwrap();
    (run, rel)
}

/// A dwell answer reporting one landing of `commit` at `at` in `spira`, when it is newer than `--landed-after`.
fn dwell_with_a_landing(
    at: u64,
    commit: &'static str,
) -> impl Fn(&[&str]) -> Result<serde_json::Value, String> {
    move |argv| {
        let after = argv
            .iter()
            .position(|a| *a == "--landed-after")
            .and_then(|i| argv[i + 1].parse::<u64>().ok());
        let landed = if after.is_some_and(|a| at > a) {
            serde_json::json!({ "spira": [{ "at": at, "commit": commit }] })
        } else {
            serde_json::json!({})
        };
        Ok(
            serde_json::json!({ "beads": [], "horizon": 0, "last": { "spira": { "Delivered": at } }, "landed_after": landed }),
        )
    }
}

fn stalls_for_landing(at: u64, commit: &'static str, contained: bool, now: u64) -> Vec<String> {
    let (run, rel) = stale_record_other_release_in_effect();
    let asked = std::cell::RefCell::new(vec![]);
    let ancestor = |c: &str, r: &str| {
        asked.borrow_mut().push((c.to_string(), r.to_string()));
        Ok(contained)
    };
    let snap = observe(
        run.path(),
        rel.path(),
        "spira",
        &dwell_with_a_landing(at, commit),
        &ancestor,
        3_600,
    );
    assert!(snap.readable, "{}", snap.why);
    if !commit.is_empty() {
        let mut want = vec![(commit.to_string(), IN_EFFECT.to_string())];
        if !contained {
            want.push((commit.to_string(), "a".repeat(40)));
        }
        assert_eq!(*asked.borrow(), want, "ancestry is tested against the release `current` names, then the newest activated");
    }
    spira_runtime::tasks::e::watch_transitions::stalls(&snap, now, &config())
        .into_iter()
        .map(|s| s.key())
        .collect()
}

#[test]
fn a_landing_contained_in_the_release_in_effect_raises_no_stall_however_old_the_recorded_activation(
) {
    let found = stalls_for_landing(1_000, LANDED, true, 1_000 + 30 * MIN);
    assert!(
        !found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")),
        "{found:?}"
    );
}

#[test]
fn a_landing_missing_from_the_release_in_effect_raises_the_stall_past_the_bound_whatever_the_recorded_activation(
) {
    let found = stalls_for_landing(50, LANDED, false, 50 + 16 * MIN);
    assert!(
        found.contains(&"LANDED→ACTIVATED spira".to_string()),
        "a landing older than the recorded activation is still unreleased: {found:?}"
    );
}

/// [`stale_record_other_release_in_effect`] plus the record a round landing leaves for the
/// release `current` names: published at 120, never marked activated.
fn current_release_published_not_activated() -> (testkit::TempDir, testkit::TempDir) {
    let (run, rel) = stale_record_other_release_in_effect();
    let mut r = release::machine::Release::new(IN_EFFECT);
    for (to, at) in [(release::machine::ReleaseState::Cut, 100), (release::machine::ReleaseState::Accepted, 110), (release::machine::ReleaseState::Published, 120)] {
        r.record(to, "t", at).unwrap();
    }
    release::machine::save(&run.path().join("release"), &r).unwrap();
    (run, rel)
}

fn stalls_with_current_published_only(at: u64, commit: &'static str, contained: bool, now: u64) -> Vec<String> {
    let (run, rel) = current_release_published_not_activated();
    let ancestor = |_: &str, _: &str| Ok(contained);
    let snap = observe(run.path(), rel.path(), "spira", &dwell_with_a_landing(at, commit), &ancestor, 3_600);
    assert!(snap.readable, "{}", snap.why);
    spira_runtime::tasks::e::watch_transitions::stalls(&snap, now, &config()).into_iter().map(|s| s.key()).collect()
}

#[test]
fn a_landing_before_the_current_release_was_published_is_no_stall_though_no_activation_was_recorded_for_it() {
    let found = stalls_with_current_published_only(110, "", false, 110 + 30 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test]
fn a_landing_the_current_release_contains_is_no_stall_when_only_its_publication_was_recorded() {
    let found = stalls_with_current_published_only(130, LANDED, true, 130 + 30 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test]
fn a_landing_after_the_current_release_was_published_and_not_in_it_stalls_past_the_bound() {
    let found = stalls_with_current_published_only(130, "", false, 130 + 30 * MIN);
    assert!(found.contains(&"LANDED→ACTIVATED spira".to_string()), "{found:?}");
    let found = stalls_with_current_published_only(130, LANDED, false, 130 + 30 * MIN);
    assert!(found.contains(&"LANDED→ACTIVATED spira".to_string()), "{found:?}");
}

#[test]
fn a_landing_that_recorded_no_commit_is_judged_by_time_against_the_recorded_activation() {
    let after = stalls_for_landing(1_000, "", false, 1_000 + 16 * MIN);
    assert!(
        after.contains(&"LANDED→ACTIVATED spira".to_string()),
        "{after:?}"
    );
    let before = stalls_for_landing(60, "", false, 60 + 16 * MIN);
    assert!(
        !before.iter().any(|k| k.starts_with("LANDED→ACTIVATED")),
        "{before:?}"
    );
}

#[test]
fn the_stall_ages_from_the_oldest_landing_the_release_lacks_not_from_a_contained_older_one() {
    let (run, rel) = stale_record_other_release_in_effect();
    let (old, new) = (1_000u64, 1_000 + 40 * MIN);
    let dwell = move |_: &[&str]| -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({ "beads": [], "horizon": 0,
            "last": { "spira": { "Delivered": new } },
            "landed_after": { "spira": [{ "at": new, "commit": "bbbb" }, { "at": old, "commit": "aaaa" }] } }))
    };
    let ancestor = |c: &str, _: &str| Ok(c == "aaaa");
    let snap = observe(run.path(), rel.path(), "spira", &dwell, &ancestor, 3_600);
    let keys = |now| -> Vec<String> {
        spira_runtime::tasks::e::watch_transitions::stalls(&snap, now, &config()).into_iter().map(|s| s.key()).collect()
    };
    assert!(!keys(new + MIN).iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "aged from the contained landing");
    assert!(keys(new + 16 * MIN).contains(&"LANDED→ACTIVATED spira".to_string()));
}

#[test]
fn a_release_containing_the_newest_landing_clears_every_landing_without_testing_the_older_ones() {
    let (run, rel) = stale_record_other_release_in_effect();
    let (old, new) = (1_000u64, 1_000 + 40 * MIN);
    let dwell = move |_: &[&str]| -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({ "beads": [], "horizon": 0,
            "last": { "spira": { "Delivered": new } },
            "landed_after": { "spira": [{ "at": new, "commit": "bbbb" }, { "at": old, "commit": "aaaa" }] } }))
    };
    let ancestor = |c: &str, _: &str| Ok(c == "bbbb");
    let snap = observe(run.path(), rel.path(), "spira", &dwell, &ancestor, 3_600);
    let found: Vec<String> = spira_runtime::tasks::e::watch_transitions::stalls(&snap, new + 16 * MIN, &config()).into_iter().map(|s| s.key()).collect();
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test]
fn an_old_landing_without_a_commit_is_judged_by_time_while_a_newer_one_is_tested_by_ancestry() {
    let (run, rel) = stale_record_other_release_in_effect();
    let (old, new) = (50u64, 1_000 + 40 * MIN);
    let dwell = move |_: &[&str]| -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({ "beads": [], "horizon": 0,
            "last": { "spira": { "Delivered": new } },
            "landed_after": { "spira": [{ "at": new, "commit": "bbbb" }, { "at": old, "commit": "" }] } }))
    };
    let ancestor = |c: &str, _: &str| Ok(c == "bbbb");
    let snap = observe(run.path(), rel.path(), "spira", &dwell, &ancestor, 3_600);
    let found: Vec<String> = spira_runtime::tasks::e::watch_transitions::stalls(&snap, new + 16 * MIN, &config()).into_iter().map(|s| s.key()).collect();
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test]
fn with_no_current_link_a_landing_is_tested_against_the_newest_activated_release() {
    let (run, rel) = stale_record_other_release_in_effect();
    std::fs::remove_file(rel.path().join("current")).unwrap();
    let only_recorded = |_: &str, r: &str| -> Result<bool, String> { assert_eq!(r, "a".repeat(40)); Ok(false) };
    let snap = observe(run.path(), rel.path(), "spira", &dwell_with_a_landing(1_000, LANDED), &only_recorded, 3_600);
    assert!(snap.readable, "{}", snap.why);
    let found: Vec<String> = spira_runtime::tasks::e::watch_transitions::stalls(&snap, 1_000 + 16 * MIN, &config()).into_iter().map(|s| s.key()).collect();
    assert!(found.contains(&"LANDED→ACTIVATED spira".to_string()), "{found:?}");
}

#[test]
fn a_landing_whose_ancestry_cannot_be_tested_pages_its_edge_and_leaves_the_rest_watched() {
    let (run, rel) = stale_record_other_release_in_effect();
    let snap = observe(run.path(), rel.path(), "spira", &dwell_with_a_landing(1_000, LANDED), &|_, _| Err("bad object".into()), 3_600);
    assert!(snap.readable, "one edge's failed test must not blind the others: {}", snap.why);
    let found: Vec<String> = spira_runtime::tasks::e::watch_transitions::stalls(&snap, 1_000 + 16 * MIN, &config()).into_iter().map(|s| s.key()).collect();
    assert!(found.contains(&"LANDED→ACTIVATED spira".to_string()), "{found:?}");
}

#[test]
fn the_repo_map_ancestry_test_tells_contained_from_not_contained_and_from_unknown() {
    let dir = testkit::TempDir::new("wt-git");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "one"]);
    let one = git(&["rev-parse", "HEAD"]);
    git(&["commit", "-q", "--allow-empty", "-m", "two"]);
    let two = git(&["rev-parse", "HEAD"]);
    let map = dir.path().join("map");
    std::fs::write(
        &map,
        format!("spira|{}|local/main|main\n", dir.path().display()),
    )
    .unwrap();
    let ancestor = repo_ancestor(&map, "spira");
    assert_eq!(ancestor(&one, &two), Ok(true));
    assert_eq!(ancestor(&two, &one), Ok(false));
    assert!(
        ancestor(LANDED, &two).is_err(),
        "a commit the checkout lacks is unknown, not unreleased"
    );
    assert!(
        repo_ancestor(&map, "other")(&one, &two).is_err(),
        "a repo with no map row cannot be asked"
    );
}

#[test]
fn a_round_open_in_another_repo_does_not_suppress_this_repos_stall() {
    let (mut s, now) = submitted_ago(20, 0, 24);
    s.open_rounds = Some(rounds_in("other", 1));
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test]
fn every_non_terminal_batch_state_counts_as_an_open_round_for_its_repo() {
    let row = |repo: &str, state: &str| serde_json::json!({"repo": repo, "state": state});
    let mut rows: Vec<_> = ["OPEN", "CI_RUNNING", "ATTRIBUTING", "REBUILDING", "GREEN", "LANDING", "DEPLOYING"].iter().map(|s| row("spira", s)).collect();
    rows.extend([row("spira", "LANDED"), row("spira", "ABANDONED"), row("other", "SETTLED"), row("other", "DISCARDED"), row("other", "OPEN"), serde_json::json!({"state": "OPEN"})]);
    let open = spira_runtime::world::open_rounds_by_repo(&rows);
    assert_eq!(open, BTreeMap::from([("spira".to_string(), 7), ("other".to_string(), 1)]));
}

#[test]
fn the_all_repos_row_waits_on_work_behind_no_round_only() {
    let (mut s, now) = submitted_ago(20, 1, 24);
    s.beads.push(bead("sp-s2", "other", "SUBMITTED", now - MIN));
    let keys = five_minute_cut_bound(&s, now);
    assert!(!keys.iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "the covered repo's old bead and the other repo's young one: {keys:?}");
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

#[test]
fn a_round_in_any_phase_suppresses_the_stall_of_its_repo_and_the_all_repos_row() {
    for phase in ["OPEN", "CI_RUNNING", "ATTRIBUTING", "REBUILDING", "GREEN"] {
        let (mut s, now) = submitted_ago(20, 0, 24);
        s.open_rounds = Some(spira_runtime::world::open_rounds_by_repo(&[serde_json::json!({"repo": "spira", "state": phase})]));
        let keys = five_minute_cut_bound(&s, now);
        assert!(!keys.iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "{phase}: {keys:?}");
    }
}

#[test]
fn rounds_that_could_not_be_counted_leave_the_stall_standing() {
    let (mut s, now) = submitted_ago(20, 0, 24);
    s.open_rounds = None;
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

fn registration() -> runtime::event::Registration {
    WatchTransitions::new(config()).registration()
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig_at(todays_case).0, NAME);
    assert!(rows.iter().any(|k| k == "stalled"), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(|| rig_at(todays_case).0, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}

struct Lineage {
    run: testkit::TempDir,
    rel: testkit::TempDir,
    map: std::path::PathBuf,
    _repo: testkit::TempDir,
    a: String,
    b: String,
    c: String,
}

/// A real repo with A <- B and a sibling C off A; machine records for a release of A
/// activated at 100 and of B activated at 300; `current` names B.
fn lineage() -> Lineage {
    let repo = testkit::TempDir::new("wt-lineage");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").arg("-C").arg(repo.path()).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "a"]);
    let a = git(&["rev-parse", "HEAD"]);
    git(&["commit", "-q", "--allow-empty", "-m", "b"]);
    let b = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "-b", "side", &a]);
    git(&["commit", "-q", "--allow-empty", "-m", "c"]);
    let c = git(&["rev-parse", "HEAD"]);
    let map = repo.path().join("map");
    std::fs::write(&map, format!("spira|{}|local/main|main\n", repo.path().display())).unwrap();
    let run = testkit::TempDir::new("wt-run");
    for (commit, at) in [(&a, 100u64), (&b, 300)] {
        let mut r = release::machine::Release::new(commit);
        for (to, t) in [(release::machine::ReleaseState::Cut, at - 3), (release::machine::ReleaseState::Accepted, at - 2), (release::machine::ReleaseState::Published, at - 1), (release::machine::ReleaseState::Activated, at)] {
            r.record(to, "t", t).unwrap();
        }
        release::machine::save(&run.path().join("release"), &r).unwrap();
    }
    let rel = testkit::TempDir::new("wt-releases");
    std::fs::create_dir(rel.path().join("rel-b")).unwrap();
    std::fs::write(rel.path().join("rel-b/MANIFEST"), format!("commit {b}\n")).unwrap();
    std::os::unix::fs::symlink("rel-b", rel.path().join("current")).unwrap();
    Lineage { run, rel, map, _repo: repo, a, b, c }
}

fn landing_stalls(l: &Lineage, at: u64, commit: &str, now: u64) -> Vec<String> {
    let commit = commit.to_string();
    let dwell = move |argv: &[&str]| -> Result<serde_json::Value, String> {
        let after = argv.iter().position(|a| *a == "--landed-after").and_then(|i| argv[i + 1].parse::<u64>().ok()).unwrap_or(0);
        let landed = if at > after { serde_json::json!({ "spira": [{ "at": at, "commit": commit }] }) } else { serde_json::json!({}) };
        Ok(serde_json::json!({ "beads": [], "horizon": 0, "last": { "spira": { "Delivered": at } }, "landed_after": landed }))
    };
    let ancestor = repo_ancestor(&l.map, "spira");
    let snap = observe(l.run.path(), l.rel.path(), "spira", &dwell, &ancestor, 3_600);
    assert!(snap.readable, "{}", snap.why);
    spira_runtime::tasks::e::watch_transitions::stalls(&snap, now, &config()).into_iter().map(|s| s.key()).collect()
}

#[test]
fn a_hand_landing_with_no_lifecycle_member_delivered_after_its_release_activated_is_no_stall_once_a_descendant_is_active() {
    let l = lineage();
    std::fs::write(l.rel.path().join("rel-b/MANIFEST"), format!("commit {}\n", "9".repeat(40))).unwrap();
    let found = landing_stalls(&l, 200, &l.a, 200 + 40 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test]
fn a_landing_the_active_release_does_not_contain_stalls_once_past_the_bound() {
    let l = lineage();
    let found = landing_stalls(&l, 200, &l.c, 200 + 40 * MIN);
    assert_eq!(found.iter().filter(|k| k.starts_with("LANDED→ACTIVATED")).count(), 1, "{found:?}");
}

#[test]
fn a_landing_the_newest_activated_release_contains_is_no_stall_while_the_current_link_names_another() {
    let l = lineage();
    std::fs::write(l.rel.path().join("rel-b/MANIFEST"), format!("commit {}\n", l.c)).unwrap();
    let found = landing_stalls(&l, 200, &l.b, 200 + 40 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test]
fn with_no_current_link_a_landing_whose_ancestry_cannot_be_tested_stalls() {
    let l = lineage();
    std::fs::remove_file(l.rel.path().join("current")).unwrap();
    let found = landing_stalls(&l, 400, &"8".repeat(40), 400 + 40 * MIN);
    assert_eq!(found.iter().filter(|k| k.starts_with("LANDED→ACTIVATED")).count(), 1, "{found:?}");
}

#[test]
fn a_landing_an_activated_ancestor_of_the_active_release_is_no_stall() {
    let l = lineage();
    let found = landing_stalls(&l, 200, &l.a, 200 + 40 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

fn halted(snap: Snapshot, since: u64) -> Snapshot {
    Snapshot { halt: Some(spira_runtime::watchers::halt::Halt { since, why: "operator order: freeze".into() }), ..snap }
}

fn pages(acts: &[runtime::event::EffectSpec]) -> Vec<String> {
    acts.iter().map(|a| a.input.clone()).filter(|l| l.contains("P0 ")).collect()
}

#[test]
fn a_stall_while_the_work_plane_is_halted_pages_nothing_and_the_row_names_the_halt() {
    let now = T0;
    let (carry, acts) = decide(&Carry::default(), &halted(todays_case(now), now - 600), now * 1000, &config());
    assert!(acts.is_empty(), "{:?}", acts.iter().map(|a| &a.input).collect::<Vec<_>>());
    assert!(carry.halted.contains("operator order: freeze") && carry.halted.contains("600s"), "{}", carry.halted);
}

#[test]
fn the_same_stall_with_the_plane_running_pages() {
    let now = T0;
    let (carry, acts) = decide(&Carry::default(), &todays_case(now), now * 1000, &config());
    assert_eq!(pages(&acts).len(), 1, "{:?}", pages(&acts));
    assert!(carry.halted.is_empty());
}

#[test]
fn after_a_halt_lifts_each_edge_waits_its_bound_again_before_paging() {
    let now = T0;
    let (carry, _) = decide(&Carry::default(), &halted(todays_case(now), now - 600), now * 1000, &config());
    let (carry, acts) = decide(&carry, &todays_case(now + 60), (now + 60) * 1000, &config());
    assert!(pages(&acts).is_empty(), "{:?}", pages(&acts));
    let later = now + 90 * MIN + 60;
    let (_, acts) = decide(&carry, &todays_case(later), later * 1000, &config());
    assert_eq!(pages(&acts).len(), 1, "{:?}", pages(&acts));
}

#[test]
fn a_stall_already_paged_neither_repages_nor_clears_across_a_halt() {
    let now = T0;
    let (carry, acts) = decide(&Carry::default(), &todays_case(now), now * 1000, &config());
    assert_eq!(pages(&acts).len(), 1);
    let (carry, acts) = decide(&carry, &halted(todays_case(now + 300), now), (now + 300) * 1000, &config());
    assert!(acts.is_empty());
    let (_, acts) = decide(&carry, &todays_case(now + 600), (now + 600) * 1000, &config());
    assert!(acts.iter().all(|a| !a.input.contains("STALL")), "{:?}", acts.iter().map(|a| &a.input).collect::<Vec<_>>());
}

fn waiting_keys(s: &Snapshot, now: u64, round_max_open: u64) -> Vec<String> {
    let cfg = Config { cut_bound_s: 5 * MIN, round_max_open, ..config() };
    spira_runtime::tasks::e::watch_transitions::stalls(s, now, &cfg).into_iter().map(|x| x.key()).collect()
}

#[test]
fn a_round_open_at_the_limit_silences_the_cut_stall_and_a_free_slot_below_the_limit_does_not() {
    let (mut s, now) = submitted_ago(10, 1, 6);
    assert!(!waiting_keys(&s, now, 1).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "one round open at a limit of one");
    assert!(waiting_keys(&s, now, 2).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()), "one round open at a limit of two leaves a slot");
    s.open_rounds = Some(rounds_in("spira", 2));
    assert!(!waiting_keys(&s, now, 2).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "two open at a limit of two");
}

fn certified_tipped(id: &str, now: u64) -> Bead {
    Bead { tip: Some("t".into()), ..bead(id, "-", "CERTIFIED", now - 9 * HOUR) }
}

#[test]
fn a_held_bead_or_one_whose_tip_is_on_the_landing_ref_is_not_counted_as_waiting() {
    let now = T0;
    let mut s = healthy(now);
    s.last = last("spira", &[("Cut", now - 6 * MIN)]);
    s.beads = vec![certified_tipped("sp-a", now), Bead { held: true, ..certified_tipped("sp-b", now) }, Bead { landed: true, ..certified_tipped("sp-c", now) }];
    let t = table(&s, ALL, now);
    assert_eq!(t["CERTIFIED"].count, 1, "only the unheld, unlanded bead waits");
    s.beads.truncate(1);
    assert!(waiting_keys(&s, now, 1).contains(&"SUBMITTED→IN_DELIVERY -".to_string()), "the control: a waiting bead pages");
    s.beads = vec![Bead { held: true, ..certified_tipped("sp-b", now) }, Bead { landed: true, ..certified_tipped("sp-c", now) }];
    assert!(!waiting_keys(&s, now, 1).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "{:?}", waiting_keys(&s, now, 1));
}

#[test]
fn mark_landed_flags_the_tip_the_ref_contains_and_leaves_an_untestable_tip_waiting() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![Bead { tip: Some("on".into()), ..bead("sp-a", "-", "SUBMITTED", now) }, Bead { tip: Some("off".into()), ..bead("sp-b", "-", "SUBMITTED", now) }, Bead { tip: Some("err".into()), ..bead("sp-c", "-", "SUBMITTED", now) }, bead("sp-d", "-", "SUBMITTED", now)];
    mark_landed(&mut s, &|_, tip| match tip {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err("unknown commit".into()),
    });
    assert_eq!(s.beads.iter().map(|b| b.landed).collect::<Vec<_>>(), [true, false, false, false]);
}

fn stuck_cut(now: u64) -> Snapshot {
    let mut s = healthy(now);
    s.beads = vec![bead("sp-s1", "spira", "SUBMITTED", now - 50 * MIN)];
    s.last = last("spira", &[("Cut", now - 50 * MIN)]);
    s
}

fn cut_pages(s: &Snapshot, now: u64) -> usize {
    stalls_of(s, now).iter().filter(|x| x.edge == CUT_EDGE).count()
}

fn paced(mut s: Snapshot, repo: &str, owner: Owner) -> Snapshot {
    s.owners.insert(cut_owner_key(repo), owner);
    s
}

#[test]
fn a_cut_stall_with_the_repos_batcher_passing_and_no_pause_is_not_paged() {
    let now = T0;
    assert_eq!(cut_pages(&stuck_cut(now), now), 1, "no owner heartbeat: paged as before");
    assert_eq!(cut_pages(&paced(stuck_cut(now), "spira", pace_owner(Some(now - 30), None)), now), 0);
}

#[test]
fn a_cut_stall_pages_when_the_repos_batcher_is_silent_paused_or_only_another_repos_beats() {
    let now = T0;
    assert_eq!(cut_pages(&paced(stuck_cut(now), "spira", pace_owner(Some(now - 121), None)), now), 1);
    assert_eq!(cut_pages(&paced(stuck_cut(now), "spira", pace_owner(Some(now - 5), Some("red:1"))), now), 1);
    assert_eq!(cut_pages(&paced(stuck_cut(now), "other", pace_owner(Some(now - 5), None)), now), 1);
    assert_eq!(pace_owner(None, None), Owner::default());
}

#[test]
fn a_cut_stall_is_excused_per_repo_and_never_by_another_repos_batcher() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-s1", "a", "SUBMITTED", now - 50 * MIN), bead("sp-s2", "b", "CERTIFIED", now - 50 * MIN)];
    s.last = BTreeMap::new();
    let keys = |s: &Snapshot| stalls_of(s, now).iter().filter(|x| x.edge == CUT_EDGE).map(Stall::key).collect::<Vec<_>>();
    s.owners.insert(cut_owner_key("a"), pace_owner(Some(now - 5), None));
    assert_eq!(keys(&s), [format!("{CUT_EDGE} b")], "b has no batcher beating and is paged alone");
    s.owners.insert(cut_owner_key("b"), pace_owner(Some(now - 5), None));
    assert!(keys(&s).is_empty(), "{:?}", keys(&s));
}

/// A store that cannot be read `ago` seconds after an activation, as `observe` answers it.
fn blind_after_activation(now: u64, ago: u64) -> Snapshot {
    Snapshot { activated_at: Some(now - ago), ..blind("spira-lc dwell: cannot tell: deadline") }
}

#[test]
fn a_read_that_fails_just_after_an_activation_is_retried_on_the_next_tick_and_pages_nothing() {
    let (mut r, w) = rig_at(|now| blind_after_activation(now, 5));
    assert_eq!(lines(&r), Vec::<String>::new(), "the first unreadable tick after an activation says nothing: {:?}", lines(&r));
    *w.lock().unwrap() = healthy(T0);
    r.advance(300_000).unwrap();
    let all = lines(&r);
    assert!(all.iter().all(|l| !l.contains("BLIND")), "{all:?}");
    assert!(all.iter().any(|l| l.contains("[watch:transitions] WAIT ") && l.contains("WORKING 3")), "the next tick reads: {all:?}");
}

#[test]
fn a_store_that_stays_unreadable_for_two_ticks_after_an_activation_still_pages_blind_naming_it() {
    let (mut r, _) = rig_at(|now| blind_after_activation(now, 5));
    r.advance(300_000).unwrap();
    let all = lines(&r);
    let pages: Vec<&String> = all.iter().filter(|l| l.contains("P0 BLIND")).collect();
    assert_eq!(pages.len(), 1, "{all:?}");
    assert!(pages[0].contains("after the activation"), "the page names the activation: {pages:?}");
}

#[test]
fn an_unreadable_store_long_after_an_activation_pages_at_once() {
    let (r, _) = rig_at(|now| blind_after_activation(now, 10 * MIN));
    assert_eq!(lines(&r).iter().filter(|l| l.contains("P0 BLIND")).count(), 1, "{:?}", lines(&r));
}
