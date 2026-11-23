//! Tests for `watch.transitions`: the period's wait-time table, the STALL page when a state holds
//! work and no exit transition happens inside its bound, and BLIND when the store cannot be read.
//! Each runs on the task rig's virtual clock against a fake store answer.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, effects_runnable, TaskRig};
use runtime::event::EventKind;
use spira_runtime::tasks::a::landing::{PrRead, PrStatus};
use spira_runtime::tasks::a::{batcher_forge, batcher_local};
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
        pr_bound_s: 15 * MIN,
        gate_timeout_s: 45 * MIN,
        certify_par: 2,
        deliver_bound_s: 60 * MIN,
        activate_bound_s: 15 * MIN,
        ask_bound_s: 60 * MIN,
        release_repo: "spira".into(),
        heartbeat_bound_s: 120,
        task_bound_s: 600,
        round_max_open: 1,
        releases: "rel".into(),
        repo_map: "map".into(),
        activation_bound_s: 75,
        run: "run".into(),
    }
}

fn bead(id: &str, repo: &str, state: &str, since: u64) -> Bead {
    Bead { id: id.into(), repo: repo.into(), state: state.into(), since, ask_held: false, ask_since: None, held: false, tip: None, landed: false, gate: None, waits_on: None, wait_from: None, pr: None }
}

fn last(repo: &str, kinds: &[(&str, u64)]) -> BTreeMap<String, BTreeMap<String, u64>> {
    BTreeMap::from([(repo.to_string(), kinds.iter().map(|(k, t)| (k.to_string(), *t)).collect())])
}

/// Work moving through every state, every exit seen minutes ago, the release current.
fn healthy(now: u64) -> Snapshot {
    Snapshot {
        readable: true,
        gate_slots_free: 0,
        activation: None,
        maintenance: None,
        why: String::new(),
        horizon: now - 6 * HOUR,
        beads: vec![bead("sp-r1", "spira", "READY", now - 20 * MIN), bead("sp-w1", "spira", "WORKING", now - 40 * MIN), bead("sp-w2", "spira", "WORKING", now - 10 * MIN), bead("sp-w3", "spira", "WORKING", now - 70 * MIN), bead("sp-s1", "spira", "SUBMITTED", now - 5 * MIN), bead("sp-d1", "spira", "IN_DELIVERY", now - 15 * MIN)],
        last: last("spira", &[("Claim", now - 10 * MIN), ("Submit", now - 5 * MIN), ("Cut", now - 15 * MIN), ("Delivered", now - 20 * MIN)]),
        activated_at: Some(now - 10 * MIN),
        landed_after: BTreeMap::new(),
        open_rounds: Some(BTreeMap::new()),
        owners: BTreeMap::new(),
        halt: None,
        landing_quiet_s: None,
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
    let mut r = TaskRig::new(1, WatchTransitions::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(INBOX_APPEND, |_| Ok("appended".into())).double(LOG_APPEND, |_| Ok("appended".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn lines(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn log_lines(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == LOG_APPEND).map(|q| q.input.clone()).collect()
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
    .double(INBOX_APPEND, |_| Ok("appended".into())).double(LOG_APPEND, |_| Ok("appended".into()));
    r.advance(T0 * 1000).unwrap();
    r.tick(stream, kind).unwrap();
    (r, w)
}

#[test_id(tw15iqe)]
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
    assert!(log_lines(&r).iter().any(|l| l.contains("WAIT ") && l.contains("WORKING 6")), "the period's table line is written too");
}

#[test_id(t9cns03)]
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

#[test_id(t6oww7a)]
#[test]
fn a_healthy_flow_raises_no_stall() {
    let (mut r, _) = rig_at(healthy);
    r.advance(12 * 300_000).unwrap();
    assert_eq!(stalls(&r), Vec::<String>::new());
    let tables = |v: Vec<String>| v.into_iter().filter(|l| l.contains("[watch:transitions] WAIT ")).count();
    assert_eq!(tables(lines(&r)), 0, "a quiet pass wakes nobody");
    assert_eq!(tables(log_lines(&r)), 13, "one table line per period");
}

#[test_id(takgfwp)]
#[test]
fn a_row_over_its_bound_wakes_the_inbox_once_per_episode() {
    let late = |now: u64| {
        let mut s = healthy(now);
        s.beads.iter_mut().find(|b| b.id == "sp-d1").unwrap().since = now - 61 * MIN;
        s
    };
    let (mut r, w) = rig_at(late);
    r.advance(3 * 300_000).unwrap();
    let overs = |r: &TaskRig| lines(r).into_iter().filter(|l| l.contains("OVER IN_DELIVERY")).collect::<Vec<_>>();
    assert_eq!(overs(&r).len(), 1, "{:?}", lines(&r));
    assert!(overs(&r)[0].contains("(bound 1h00m)"), "{:?}", overs(&r));
    assert_eq!(log_lines(&r).len(), 4);
    *w.lock().unwrap() = healthy(T0);
    r.advance(300_000).unwrap();
    assert_eq!(overs(&r).len(), 1);
    *w.lock().unwrap() = late(T0);
    r.advance(300_000).unwrap();
    assert_eq!(overs(&r).len(), 2, "back inside, then over again: one more wake");
}

#[test_id(tzk71vr)]
#[test]
fn the_first_sighted_pass_after_blind_reaches_the_inbox() {
    let (mut r, w) = rig_at(|_| blind("spira-lc dwell: locked"));
    r.advance(300_000).unwrap();
    let tables = |r: &TaskRig| lines(r).into_iter().filter(|l| l.contains("[watch:transitions] WAIT ") && !l.contains("WAIT BLIND")).count();
    assert_eq!(tables(&r), 0);
    *w.lock().unwrap() = healthy(T0);
    r.advance(2 * 300_000).unwrap();
    assert_eq!(tables(&r), 1, "{:?}", lines(&r));
}

#[test_id(t2uo0z1)]
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

#[test_id(tz6ku5k)]
#[test]
fn an_observe_that_fails_is_blind_too() {
    let (_, stream, kind) = WatchTransitions::boot();
    let mut r = TaskRig::new(1, WatchTransitions::new(config()).registration()).unwrap().double(OBSERVE, |_| Err("lc-serve: connection refused".into())).double(INBOX_APPEND, |_| Ok("appended".into())).double(LOG_APPEND, |_| Ok("appended".into()));
    r.tick(stream, kind).unwrap();
    let all = lines(&r);
    assert!(all.iter().any(|l| l.contains("P0 BLIND") && l.contains("connection refused")), "{all:?}");
}

#[test_id(to85zk2)]
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

#[test_id(t367cda)]
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

#[test_id(tz6w9wb)]
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

#[test_id(tojplot)]
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

#[test_id(trte069)]
#[test]
fn the_row_is_registered_and_its_boot_arms_the_configured_period() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), 300_000);
    assert_eq!(Config::pinned().map(|c| c.period_s), Ok(300), "every key resolves through the config door");
}

#[test_id(tk4to0u)]
#[test]
fn every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_transitions.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test_id(tjo9rll)]
#[test]
fn two_days_replay_identically() {
    let r = assert_deterministic(|_| rig(Arc::new(Mutex::new(healthy(T0)))), 7, 2);
    acc::assert_healthy(&r, STREAM);
}

#[test_id(tne8maw)]
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

#[test_id(tz66i4o)]
#[test]
fn work_that_entered_delivery_minutes_ago_after_a_long_quiet_spell_raises_no_stall() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-d1", "spira", "IN_DELIVERY", now - 4 * MIN)];
    s.last = last("spira", &[("Delivered", now - 2 * HOUR), ("Cut", now - 2 * MIN)]);
    assert!(!edges(&s, now).iter().any(|k| k.starts_with("IN_DELIVERY→LANDED")), "{:?}", edges(&s, now));
}

#[test_id(tj6wuml)]
#[test]
fn work_in_delivery_past_the_bound_with_no_delivered_raises_the_stall() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-d1", "spira", "IN_DELIVERY", now - 61 * MIN)];
    s.last = last("spira", &[("Delivered", now - 3 * HOUR)]);
    assert!(edges(&s, now).contains(&"IN_DELIVERY→LANDED spira".to_string()));
}

#[test_id(tsme0in)]
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

#[test_id(tiltasx)]
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

#[test_id(t6uf5oe)]
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

#[test_id(t6lf3hl)]
#[test]
fn a_delivery_stall_with_the_round_beating_and_nothing_failed_is_not_paged() {
    let now = T0;
    assert_eq!(delivery_pages(&stuck_delivery(now), now), 1, "no owner heartbeat: paged as before");
    assert_eq!(delivery_pages(&owned(stuck_delivery(now), now, 30, None), now), 0, "a fresh heartbeat means the owner is handling it");
}

#[test_id(t7r5ys5)]
#[test]
fn a_delivery_stall_pages_when_the_round_heartbeat_has_gone_silent() {
    let now = T0;
    assert_eq!(delivery_pages(&owned(stuck_delivery(now), now, 121, None), now), 1);
}

#[test_id(t179p1b)]
#[test]
fn a_delivery_stall_pages_when_the_round_reports_a_failed_action_despite_a_fresh_beat() {
    let now = T0;
    assert_eq!(delivery_pages(&owned(stuck_delivery(now), now, 5, Some("stalled: no progress in 300s")), now), 1);
}

#[test_id(txmsr05)]
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

#[test_id(tvj7s5t)]
#[test]
fn a_bead_submitted_two_minutes_ago_is_not_stalled_by_a_cut_six_minutes_ago() {
    let (s, now) = submitted_ago(2, 0, 6);
    assert!(!five_minute_cut_bound(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")));
}

#[test_id(tik1fcn)]
#[test]
fn a_bead_submitted_ten_minutes_ago_behind_the_full_open_rounds_is_not_stalled() {
    let (s, now) = submitted_ago(10, 1, 6);
    assert!(!five_minute_cut_bound(&s, now).iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")));
}

#[test_id(tigjzkr)]
#[test]
fn a_bead_submitted_ten_minutes_ago_with_no_round_open_is_stalled() {
    let (s, now) = submitted_ago(10, 0, 6);
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test_id(twz520g)]
#[test]
fn a_certified_bead_past_the_bound_with_a_free_slot_is_stalled_even_after_a_recent_cut() {
    let now = T0;
    let mut s = healthy(now);
    s.beads = vec![bead("sp-c1", "spira", "CERTIFIED", now - 10 * MIN)];
    s.last = last("spira", &[("Cut", now - MIN)]);
    s.open_rounds = Some(rounds_in("spira", 0));
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test_id(t7i28vl)]
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

#[test_id(tntn7t0)]
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

#[test_id(t619gre)]
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

#[test_id(tppemdy)]
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

#[test_id(tz7gbub)]
#[test]
fn a_landing_contained_in_the_release_in_effect_raises_no_stall_however_old_the_recorded_activation(
) {
    let found = stalls_for_landing(1_000, LANDED, true, 1_000 + 30 * MIN);
    assert!(
        !found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")),
        "{found:?}"
    );
}

#[test_id(tf4jsbj)]
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

#[test_id(tjyprod)]
#[test]
fn a_landing_before_the_current_release_was_published_is_no_stall_though_no_activation_was_recorded_for_it() {
    let found = stalls_with_current_published_only(110, "", false, 110 + 30 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test_id(theq8sp)]
#[test]
fn a_landing_the_current_release_contains_is_no_stall_when_only_its_publication_was_recorded() {
    let found = stalls_with_current_published_only(130, LANDED, true, 130 + 30 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test_id(tkcmdm5)]
#[test]
fn a_landing_after_the_current_release_was_published_and_not_in_it_stalls_past_the_bound() {
    let found = stalls_with_current_published_only(130, "", false, 130 + 30 * MIN);
    assert!(found.contains(&"LANDED→ACTIVATED spira".to_string()), "{found:?}");
    let found = stalls_with_current_published_only(130, LANDED, false, 130 + 30 * MIN);
    assert!(found.contains(&"LANDED→ACTIVATED spira".to_string()), "{found:?}");
}

#[test_id(tfj1r1g)]
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

#[test_id(tkllbzw)]
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

#[test_id(tzd56em)]
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

#[test_id(tk3ctmm)]
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

#[test_id(tqlzib3)]
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

#[test_id(t23e8ap)]
#[test]
fn a_landing_whose_ancestry_cannot_be_tested_pages_its_edge_and_leaves_the_rest_watched() {
    let (run, rel) = stale_record_other_release_in_effect();
    let snap = observe(run.path(), rel.path(), "spira", &dwell_with_a_landing(1_000, LANDED), &|_, _| Err("bad object".into()), 3_600);
    assert!(snap.readable, "one edge's failed test must not blind the others: {}", snap.why);
    let found: Vec<String> = spira_runtime::tasks::e::watch_transitions::stalls(&snap, 1_000 + 16 * MIN, &config()).into_iter().map(|s| s.key()).collect();
    assert!(found.contains(&"LANDED→ACTIVATED spira".to_string()), "{found:?}");
}

#[test_id(tzmhtay)]
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

#[test_id(truq5ep)]
#[test]
fn a_round_open_in_another_repo_does_not_suppress_this_repos_stall() {
    let (mut s, now) = submitted_ago(20, 0, 24);
    s.open_rounds = Some(rounds_in("other", 1));
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

#[test_id(t3si34t)]
#[test]
fn every_non_terminal_batch_state_counts_as_an_open_round_for_its_repo() {
    let row = |repo: &str, state: &str| serde_json::json!({"repo": repo, "state": state});
    let mut rows: Vec<_> = ["OPEN", "CI_RUNNING", "ATTRIBUTING", "REBUILDING", "GREEN", "LANDING", "DEPLOYING"].iter().map(|s| row("spira", s)).collect();
    rows.extend([row("spira", "LANDED"), row("spira", "ABANDONED"), row("other", "SETTLED"), row("other", "DISCARDED"), row("other", "OPEN"), serde_json::json!({"state": "OPEN"})]);
    let open = spira_runtime::world::open_rounds_by_repo(&rows);
    assert_eq!(open, BTreeMap::from([("spira".to_string(), 7), ("other".to_string(), 1)]));
}

#[test_id(t3xvpj2)]
#[test]
fn the_all_repos_row_waits_on_work_behind_no_round_only() {
    let (mut s, now) = submitted_ago(20, 1, 24);
    s.beads.push(bead("sp-s2", "other", "SUBMITTED", now - MIN));
    let keys = five_minute_cut_bound(&s, now);
    assert!(!keys.iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "the covered repo's old bead and the other repo's young one: {keys:?}");
}

#[test_id(t3rzuhv)]
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

#[test_id(ttk547c)]
#[test]
fn a_round_in_any_phase_suppresses_the_stall_of_its_repo_and_the_all_repos_row() {
    for phase in ["OPEN", "CI_RUNNING", "ATTRIBUTING", "REBUILDING", "GREEN"] {
        let (mut s, now) = submitted_ago(20, 0, 24);
        s.open_rounds = Some(spira_runtime::world::open_rounds_by_repo(&[serde_json::json!({"repo": "spira", "state": phase})]));
        let keys = five_minute_cut_bound(&s, now);
        assert!(!keys.iter().any(|k| k.starts_with("SUBMITTED→IN_DELIVERY")), "{phase}: {keys:?}");
    }
}

#[test_id(tm8jw1r)]
#[test]
fn rounds_that_could_not_be_counted_leave_the_stall_standing() {
    let (mut s, now) = submitted_ago(20, 0, 24);
    s.open_rounds = None;
    assert!(five_minute_cut_bound(&s, now).contains(&"SUBMITTED→IN_DELIVERY spira".to_string()));
}

fn registration() -> runtime::event::Registration {
    WatchTransitions::new(config()).registration()
}

#[test_id(tcon92c)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&rig_at(todays_case).0, NAME);
    assert!(rows.iter().any(|k| k == "stalled"), "{rows:?}");
}

#[test_id(tozbumb)]
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

#[test_id(t3jljmm)]
#[test]
fn a_hand_landing_with_no_lifecycle_member_delivered_after_its_release_activated_is_no_stall_once_a_descendant_is_active() {
    let l = lineage();
    std::fs::write(l.rel.path().join("rel-b/MANIFEST"), format!("commit {}\n", "9".repeat(40))).unwrap();
    let found = landing_stalls(&l, 200, &l.a, 200 + 40 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test_id(tzisgos)]
#[test]
fn a_landing_the_active_release_does_not_contain_stalls_once_past_the_bound() {
    let l = lineage();
    let found = landing_stalls(&l, 200, &l.c, 200 + 40 * MIN);
    assert_eq!(found.iter().filter(|k| k.starts_with("LANDED→ACTIVATED")).count(), 1, "{found:?}");
}

#[test_id(tf20pi9)]
#[test]
fn a_landing_the_newest_activated_release_contains_is_no_stall_while_the_current_link_names_another() {
    let l = lineage();
    std::fs::write(l.rel.path().join("rel-b/MANIFEST"), format!("commit {}\n", l.c)).unwrap();
    let found = landing_stalls(&l, 200, &l.b, 200 + 40 * MIN);
    assert!(!found.iter().any(|k| k.starts_with("LANDED→ACTIVATED")), "{found:?}");
}

#[test_id(txnsyl6)]
#[test]
fn with_no_current_link_a_landing_whose_ancestry_cannot_be_tested_stalls() {
    let l = lineage();
    std::fs::remove_file(l.rel.path().join("current")).unwrap();
    let found = landing_stalls(&l, 400, &"8".repeat(40), 400 + 40 * MIN);
    assert_eq!(found.iter().filter(|k| k.starts_with("LANDED→ACTIVATED")).count(), 1, "{found:?}");
}

#[test_id(t0ldbt0)]
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

#[test_id(tbh0jqg)]
#[test]
fn a_stall_while_the_work_plane_is_halted_pages_nothing_and_the_row_names_the_halt() {
    let now = T0;
    let (carry, acts) = decide(&Carry::default(), &halted(todays_case(now), now - 600), now * 1000, &config());
    assert!(acts.is_empty(), "{:?}", acts.iter().map(|a| &a.input).collect::<Vec<_>>());
    assert!(carry.halted.contains("operator order: freeze") && carry.halted.contains("600s"), "{}", carry.halted);
}

#[test_id(t8nwlnk)]
#[test]
fn the_same_stall_with_the_plane_running_pages() {
    let now = T0;
    let (carry, acts) = decide(&Carry::default(), &todays_case(now), now * 1000, &config());
    assert_eq!(pages(&acts).len(), 1, "{:?}", pages(&acts));
    assert!(carry.halted.is_empty());
}

#[test_id(tpmv193)]
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

#[test_id(tts3vl1)]
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

#[test_id(tmlqthg)]
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

#[test_id(tf31c7h)]
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

#[test_id(ts686m4)]
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

#[test_id(tcmlbhi)]
#[test]
fn a_cut_stall_with_the_repos_batcher_passing_and_no_pause_is_not_paged() {
    let now = T0;
    assert_eq!(cut_pages(&stuck_cut(now), now), 1, "no owner heartbeat: paged as before");
    assert_eq!(cut_pages(&paced(stuck_cut(now), "spira", pace_owner(Some(now - 30), None)), now), 0);
}

#[test_id(tlroybh)]
#[test]
fn a_cut_stall_pages_when_the_repos_batcher_is_silent_paused_or_only_another_repos_beats() {
    let now = T0;
    assert_eq!(cut_pages(&paced(stuck_cut(now), "spira", pace_owner(Some(now - 121), None)), now), 1);
    assert_eq!(cut_pages(&paced(stuck_cut(now), "spira", pace_owner(Some(now - 5), Some("red:1"))), now), 1);
    assert_eq!(cut_pages(&paced(stuck_cut(now), "other", pace_owner(Some(now - 5), None)), now), 1);
    assert_eq!(pace_owner(None, None), Owner::default());
}

#[test_id(txqhiwy)]
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
    Snapshot { activated_at: Some(now - ago), ..blind("store unavailable: tried for 25 s") }
}

#[test_id(tci4l5j)]
#[test]
fn the_first_unreadable_tick_just_after_an_activation_pages_blind_with_the_stores_reason() {
    let (r, _) = rig_at(|now| blind_after_activation(now, 5));
    let all = lines(&r);
    let pages: Vec<&String> = all.iter().filter(|l| l.contains("P0 BLIND")).collect();
    assert_eq!(pages.len(), 1, "{all:?}");
    assert!(pages[0].contains("store unavailable: tried for 25 s"), "{pages:?}");
    assert!(!pages[0].contains("activation"), "the page does not reason about releases: {pages:?}");
}

#[test_id(t0gmccg)]
#[test]
fn a_second_unreadable_tick_does_not_page_again_and_a_readable_one_clears_blind() {
    let (mut r, w) = rig_at(|now| blind_after_activation(now, 5));
    r.advance(300_000).unwrap();
    assert_eq!(lines(&r).iter().filter(|l| l.contains("P0 BLIND")).count(), 1, "{:?}", lines(&r));
    *w.lock().unwrap() = healthy(T0);
    r.advance(300_000).unwrap();
    *w.lock().unwrap() = blind_after_activation(T0, 5);
    r.advance(300_000).unwrap();
    assert_eq!(lines(&r).iter().filter(|l| l.contains("P0 BLIND")).count(), 2, "a read in between re-arms the page: {:?}", lines(&r));
}

#[test_id(t60xow0)]
#[test]
fn a_timed_out_observe_pages_blind_once_and_the_next_timed_out_one_does_not() {
    let (_, stream, kind) = WatchTransitions::boot();
    let mut r = TaskRig::new(1, WatchTransitions::new(config()).registration()).unwrap().timing_out().double(INBOX_APPEND, |_| Ok("appended".into())).double(LOG_APPEND, |_| Ok("appended".into()));
    r.advance(T0 * 1000).unwrap();
    r.tick(stream, kind).unwrap();
    r.advance(300_000).unwrap();
    let pages = lines(&r).into_iter().filter(|l| l.contains("P0 BLIND") && l.contains("the observe timed out")).count();
    assert_eq!(pages, 1, "{:?}", lines(&r));
}

fn ask_stall(now: u64) -> Snapshot {
    let mut s = healthy(now);
    let mut asked = bead("sp-q1", "spira", "READY", now - 3 * HOUR);
    asked.ask_held = true;
    asked.ask_since = Some(now - 61 * MIN);
    s.beads.push(asked);
    s
}

fn activation_stall(now: u64) -> Snapshot {
    let mut s = healthy(now);
    s.landed_after.insert("spira".into(), now - 16 * MIN);
    s
}

fn submit_stall(now: u64) -> Snapshot {
    let mut s = todays_case(now);
    s.last = last("spira", &[("Claim", now - 20 * MIN), ("Submit", now - 3 * HOUR), ("Cut", now - 3 * HOUR), ("Delivered", now - 20 * MIN)]);
    s
}

fn owned_by_task(mut s: Snapshot, edge: &str, quiet: Option<u64>, now: u64) -> Snapshot {
    s.owners.insert(edge.into(), task_owner(quiet, now));
    s
}

fn edge_pages(s: &Snapshot, edge: &str, now: u64) -> usize {
    stalls_of(s, now).iter().filter(|x| x.edge == edge).count()
}

#[test_id(tqx7lzt)]
#[test]
fn each_task_owned_edge_pages_unowned_is_quiet_while_its_task_pulses_and_pages_when_it_is_silent() {
    let now = T0;
    type Make = fn(u64) -> Snapshot;
    let edges: [(&str, Make); 3] = [("ASK→ANSWERED", ask_stall), ("LANDED→ACTIVATED", activation_stall), ("WORKING→SUBMITTED", submit_stall)];
    for (edge, make) in edges {
        assert_eq!(edge_pages(&make(now), edge, now), 1, "{edge}: no owner record pages on the bound");
        assert_eq!(edge_pages(&owned_by_task(make(now), edge, Some(300), now), edge, now), 0, "{edge}: a task that observed 5m ago is acting");
        assert_eq!(edge_pages(&owned_by_task(make(now), edge, Some(601), now), edge, now), 1, "{edge}: a task silent past the task bound pages");
        assert_eq!(edge_pages(&owned_by_task(make(now), edge, None, now), edge, now), 1, "{edge}: a task that never observed pages");
    }
}

#[test_id(tyyrbxl)]
#[test]
fn the_task_pulse_window_is_the_task_bound_not_the_round_heartbeat_bound() {
    let now = T0;
    let cfg = Config { task_bound_s: 1_000, heartbeat_bound_s: 10, ..config() };
    let s = owned_by_task(ask_stall(now), "ASK→ANSWERED", Some(500), now);
    assert!(spira_runtime::tasks::e::watch_transitions::stalls(&s, now, &cfg).iter().all(|x| x.edge != "ASK→ANSWERED"));
}

#[test_id(to62pyd)]
#[test]
fn an_observe_reads_each_owner_task_pulse_from_the_run_dir() {
    let lc = |_: &[&str]| Ok(serde_json::json!({ "beads": [], "horizon": 0, "last": {}, "landed_after": {} }));
    let ancestor = |_: &str, _: &str| Ok(true);
    let (rec, rel) = stale_record_other_release_in_effect();
    let before = observe(rec.path(), rel.path(), "spira", &lc, &ancestor, 3_600);
    assert!(before.readable, "{}", before.why);
    for (edge, _) in TASK_OWNERS {
        assert_eq!(before.owners[edge].beat, None, "{edge}: nothing stamped yet");
    }
    for (_, task) in TASK_OWNERS {
        spira_config::ticked::stamp(rec.path(), task);
    }
    let after = observe(rec.path(), rel.path(), "spira", &lc, &ancestor, 3_600);
    for (edge, _) in TASK_OWNERS {
        assert!(after.owners[edge].beat.is_some(), "{edge}: stamped");
    }
}

#[test_id(t1ogry6)]
#[test]
fn a_bound_over_two_hours_or_inside_its_owners_window_is_refused() {
    assert!(config().bounds_fit_owners().is_ok());
    assert!(Config { submit_bound_s: 2 * HOUR + 1, ..config() }.bounds_fit_owners().is_err());
    assert!(Config { ask_bound_s: 600, ..config() }.bounds_fit_owners().is_err());
    assert!(Config { activate_bound_s: 601, ..config() }.bounds_fit_owners().is_ok());
}

fn pace_file(run: &testkit::TempDir, repo: &str, name: &str, body: &str, mtime: u64) {
    let dir = run.path().join("rounds-pace").join(repo);
    std::fs::create_dir_all(&dir).unwrap();
    let f = std::fs::File::create(dir.join(name)).unwrap();
    std::io::Write::write_all(&mut &f, body.as_bytes()).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime)).unwrap();
}

#[test_id(tpb4jm9)]
#[test]
fn each_repos_rounds_pace_state_is_its_cut_owner_and_a_standing_pause_is_its_failure() {
    let run = testkit::TempDir::new("wt-pace");
    pace_file(&run, "spira", "state", "infra=0\n", 1_700);
    pace_file(&run, "held", "state", "infra=0\n", 1_800);
    pace_file(&run, "held", "pause", "red: round 7\n", 1_800);
    std::fs::create_dir_all(run.path().join("rounds-pace/never-cut")).unwrap();
    let owners: BTreeMap<String, Owner> = pace_owners(run.path()).into_iter().collect();
    assert_eq!(owners.len(), 2, "a repo with no state file has no owner: {owners:?}");
    assert_eq!(owners[&cut_owner_key("spira")], Owner { beat: Some(1_700), failed: None });
    assert_eq!(owners[&cut_owner_key("held")], Owner { beat: Some(1_800), failed: Some("paused: red: round 7".into()) });
    assert!(pace_owners(&run.path().join("absent")).is_empty(), "no rounds-pace directory is no owner");
}

#[test_id(tqzzpw9)]
#[test]
fn a_scoped_delivery_owner_outranks_the_edges_and_the_edges_covers_a_repo_without_one() {
    let now = T0;
    let mut s = owned(stuck_delivery(now), now, 5, None);
    assert_eq!(delivery_pages(&s, now), 0, "the edge's own heartbeat covers repo spira");
    s.owners.insert(format!("{DELIVERY_EDGE} spira"), Owner { beat: Some(now - 500), failed: None });
    assert_eq!(delivery_pages(&s, now), 1, "a silent record for the repo outranks the edge's fresh one");
}

#[test_id(t9q41k6)]
#[test]
fn every_edge_reads_its_bound_from_the_config_and_an_unknown_edge_has_none() {
    let cfg = Config { submit_bound_s: 11, cut_bound_s: 22, pr_bound_s: 40, deliver_bound_s: 33, activate_bound_s: 44, ask_bound_s: 55, ..config() };
    let bounds: Vec<Option<u64>> = EDGES.iter().map(|e| cfg.bound(e)).collect();
    assert_eq!(bounds, [Some(11), Some(22), Some(40), Some(33), Some(44), Some(55)]);
    assert_eq!(cfg.bound("NOT→AN_EDGE"), None);
    assert_eq!(cfg.window_s(), 44 + 600, "the audit window covers the audit-read edges, not the ask bound");
    let asks_longest = Config { ask_bound_s: 9_999, ..cfg };
    assert_eq!(asks_longest.window_s(), 44 + 600, "an ask's age comes from the store, so it does not widen the audit read");
}

#[test_id(t6c3hsz)]
#[test]
fn a_stall_is_judged_on_the_bound_the_config_names_for_its_edge() {
    let now = T0;
    let s = stuck_delivery(now);
    let tight = Config { deliver_bound_s: 3 * HOUR, ..config() };
    let loose = Config { deliver_bound_s: HOUR, ..config() };
    assert!(spira_runtime::tasks::e::watch_transitions::stalls(&s, now, &tight).iter().all(|x| x.edge != DELIVERY_EDGE), "2h in delivery is inside a 3h bound");
    let page = spira_runtime::tasks::e::watch_transitions::stalls(&s, now, &loose).into_iter().find(|x| x.edge == DELIVERY_EDGE).expect("2h in delivery is past a 1h bound");
    assert!(page.evidence.contains("bound 1h00m"), "{}", page.evidence);
}

#[test_id(tike2zm)]
#[test]
fn a_stall_is_held_back_until_its_own_edges_bound_has_passed_since_the_plane_resumed() {
    let now = T0;
    let cfg = Config { submit_bound_s: 5_000, ..config() };
    let s = todays_case(now - 6_000);
    let age = |resumed_for: u64| Carry { resumed_at: now - resumed_for, ..Carry::default() };
    let held = decide(&age(4_999), &s, now * 1000, &cfg).1;
    assert!(pages(&held).is_empty(), "{:?}", pages(&held));
    let due = decide(&age(5_000), &s, now * 1000, &cfg).1;
    assert_eq!(pages(&due).len(), 1, "{:?}", pages(&due));
}

fn gate_cfg() -> Config {
    Config { cut_bound_s: 5 * MIN, gate_timeout_s: 45 * MIN, ..config() }
}

fn gating_fixture(submitted_ago: u64, gate: Option<Gating>, state: &str) -> (Snapshot, u64) {
    let now = T0;
    let mut s = healthy(now);
    let mut b = bead("sp-0m57dx", "spira", state, now - submitted_ago * MIN);
    b.gate = gate;
    s.beads = vec![b];
    s.last = last("spira", &[("Cut", now - 2 * MIN), ("Delivered", now - 20 * MIN)]);
    s.open_rounds = Some(BTreeMap::new());
    s.owners.clear();
    (s, now)
}

fn cut_stalls(s: &Snapshot, now: u64) -> Vec<Stall> {
    spira_runtime::tasks::e::watch_transitions::stalls(s, now, &gate_cfg()).into_iter().filter(|x| x.edge == CUT_EDGE).collect()
}

#[test_id(t6t6zek)]
#[test]
fn a_submitted_bead_whose_gate_is_running_inside_the_gate_timeout_does_not_page() {
    let (s, now) = gating_fixture(6, Some(Gating::Running { since: T0 - 387 }), "SUBMITTED");
    assert_eq!(cut_stalls(&s, now), vec![]);
}

#[test_id(tglcpqd)]
#[test]
fn a_submitted_bead_whose_gate_is_queued_does_not_page() {
    let (s, now) = gating_fixture(6, Some(Gating::Queued), "SUBMITTED");
    assert_eq!(cut_stalls(&s, now), vec![]);
}

#[test_id(tu6oilw)]
#[test]
fn a_submitted_bead_with_no_gate_pages_and_the_page_says_so() {
    let (s, now) = gating_fixture(6, None, "SUBMITTED");
    let found = cut_stalls(&s, now);
    assert_eq!(found.len(), 1);
    assert!(found[0].evidence.contains("sp-0m57dx") && found[0].evidence.contains("no gate queued or running"), "{}", found[0].evidence);
}

#[test_id(trzmbab)]
#[test]
fn a_submitted_bead_whose_gate_ran_past_the_gate_timeout_pages_and_the_page_says_so() {
    let (s, now) = gating_fixture(50, Some(Gating::Running { since: T0 - 46 * MIN }), "SUBMITTED");
    let found = cut_stalls(&s, now);
    assert_eq!(found.len(), 1);
    assert!(found[0].evidence.contains("past SPIRA_GATE_TIMEOUT"), "{}", found[0].evidence);
}

#[test_id(tu49z7k)]
#[test]
fn a_submitted_bead_the_landing_pass_backed_off_waits_on_the_backoff_and_does_not_page() {
    let (mut s, now) = gating_fixture(8, None, "SUBMITTED");
    s.beads[0].id = "sp-s1".into();
    s.beads[0].tip = Some("T".into());
    mark_gated(&mut s, &BTreeMap::new(), &[("T".to_string(), now + 10 * MIN), ("U".to_string(), 1)]);
    assert_eq!(s.beads[0].gate, Some(Gating::BackingOff { until: now + 10 * MIN }));
    assert_eq!(cut_stalls(&s, now), vec![]);
    let (_, acts) = decide(&Carry::default(), &s, now * 1000, &gate_cfg());
    assert_eq!(over_submitted(&acts), Vec::<String>::new());
}

#[test_id(tkigmuy)]
#[test]
fn a_backoff_that_ended_pages_and_names_when_it_ended() {
    let (mut s, now) = gating_fixture(8, None, "SUBMITTED");
    s.beads[0].tip = Some("T".into());
    mark_gated(&mut s, &BTreeMap::new(), &[("T".to_string(), now - MIN)]);
    let found = cut_stalls(&s, now);
    assert_eq!(found.len(), 1);
    assert!(found[0].evidence.contains("gate backed off until"), "{}", found[0].evidence);
}

#[test_id(tx4k5ok)]
#[test]
fn a_queued_gate_with_every_slot_held_does_not_page() {
    let (mut s, now) = gating_fixture(8, Some(Gating::Queued), "SUBMITTED");
    s.gate_slots_free = 0;
    assert_eq!(cut_stalls(&s, now), vec![]);
}

#[test_id(tpiewab)]
#[test]
fn a_queued_gate_with_a_slot_free_for_a_full_bound_pages_once_naming_the_bead() {
    let (mut s, now) = gating_fixture(8, Some(Gating::Queued), "SUBMITTED");
    s.beads[0].id = "sp-s1".into();
    let cfg = gate_cfg();
    let (c1, a1) = decide(&Carry::default(), &s, now * 1000, &cfg);
    assert_eq!(over_submitted(&a1), Vec::<String>::new());
    s.gate_slots_free = 1;
    let (c2, a2) = decide(&c1, &s, (now + 5 * MIN) * 1000, &cfg);
    assert_eq!(over_submitted(&a2), Vec::<String>::new());
    let (_, a3) = decide(&c2, &s, (now + 10 * MIN) * 1000, &cfg);
    let lines = over_submitted(&a3);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("sp-s1") && lines[0].contains("queued, a gate slot free"), "{}", lines[0]);
}

#[test_id(t76rj7b)]
#[test]
fn a_certified_bead_pages_whatever_its_gate_holds() {
    for gate in [None, Some(Gating::Queued), Some(Gating::Running { since: T0 - 60 })] {
        let (s, now) = gating_fixture(6, gate, "CERTIFIED");
        let found = cut_stalls(&s, now);
        assert_eq!(found.len(), 1);
        assert!(found[0].evidence.contains("certified, not cut"), "{}", found[0].evidence);
    }
}

#[test_id(t6vqa7a)]
#[test]
fn a_gate_is_read_from_the_queue_and_the_admission_leases() {
    use landing_pass::gateq::{Job, Where};
    use spira_config::admission::Lease;
    let job = |branch: &str, bead: &str| Job::new("spira", branch, bead, "t", false);
    let lease = |who: &str, since: u64| Lease { slot: 1, pid: 1, start: 1, who: who.into(), since, waited: 0, last: since, weight: 1, until: None };
    let jobs = vec![(Where::Inbox, job("spira/sp-q", "sp-q")), (Where::Claimed, job("spira/sp-a1", "sp-a1")), (Where::Claimed, job("spira/sp-w", "sp-w"))];
    let got = gates_of(&jobs, &[lease("spira/sp-a1", 100)]);
    assert_eq!(got.get("sp-q"), Some(&Gating::Queued));
    assert_eq!(got.get("sp-a1"), Some(&Gating::Running { since: 100 }));
    assert_eq!(got.get("sp-w"), Some(&Gating::Queued));
    assert_eq!(got.get("sp-none"), None);
}

#[test_id(t2u5ogm)]
#[test]
fn mark_gated_sets_a_gate_on_submitted_beads_only_and_clears_a_stale_one() {
    let now = T0;
    let mut s = healthy(now);
    let mut stale = bead("sp-gone", "spira", "SUBMITTED", now - 6 * MIN);
    stale.gate = Some(Gating::Queued);
    s.beads = vec![bead("sp-0m57dx", "spira", "SUBMITTED", now - 6 * MIN), bead("sp-c1", "spira", "CERTIFIED", now - 6 * MIN), stale];
    s.last = last("spira", &[("Cut", now - 2 * MIN), ("Delivered", now - 20 * MIN)]);
    s.open_rounds = Some(BTreeMap::new());
    s.owners.clear();
    let gates = BTreeMap::from([("sp-0m57dx".to_string(), Gating::Queued), ("sp-c1".to_string(), Gating::Queued)]);
    mark_gated(&mut s, &gates, &[]);
    let gate_of = |id: &str| s.beads.iter().find(|b| b.id == id).unwrap().gate.clone();
    assert_eq!((gate_of("sp-0m57dx"), gate_of("sp-c1"), gate_of("sp-gone")), (Some(Gating::Queued), None, None));
    let found = cut_stalls(&s, now);
    assert_eq!(found.len(), 1);
    assert!(found[0].evidence.contains("oldest uncovered: sp-c1 CERTIFIED") || found[0].evidence.contains("oldest uncovered: sp-gone"), "{}", found[0].evidence);
}

fn dependency_page_world(now: u64) -> Snapshot {
    dependency_world_with(now, "WORKING")
}

fn dependency_world_with(now: u64, blocker_state: &str) -> Snapshot {
    let mut s = healthy(now);
    let submitted = |id: &str, mins: u64| {
        let mut b = bead(id, "spira", "SUBMITTED", now - mins * MIN);
        b.gate = Some(Gating::Queued);
        b
    };
    s.beads = vec![submitted("sp-s1", 51), submitted("sp-s2", 36), submitted("sp-s3", 20), bead("sp-um4jw9", "spira", "CERTIFIED", now - 54 * MIN), bead("sp-f0cw5z", "spira", blocker_state, now - 5 * MIN)];
    s.last = last("spira", &[("Claim", now - 18), ("Submit", now - 12 * MIN - 51), ("Cut", now - 17), ("Delivered", now - 24 * MIN - 42)]);
    s.open_rounds = Some(rounds_in("spira", 0));
    s.owners.clear();
    s
}

fn dependency_rig(make: impl Fn(u64) -> Snapshot, carry: Option<&str>) -> TaskRig {
    let w = Arc::new(Mutex::new(make(T0)));
    let (_, stream, kind) = WatchTransitions::boot();
    let mut r = TaskRig::new(1, WatchTransitions::new(gate_cfg()).registration()).unwrap().double(OBSERVE, move |input| {
        let now = serde_json::from_str::<serde_json::Value>(input).unwrap()["now"].as_u64().unwrap();
        let mut s = w.lock().unwrap().clone();
        let shift = |t: u64| t + now - T0;
        s.horizon = shift(s.horizon);
        s.beads.iter_mut().for_each(|b| b.since = shift(b.since));
        s.last.values_mut().flat_map(|k| k.values_mut()).for_each(|t| *t = shift(*t));
        s.activated_at = s.activated_at.map(shift);
        Ok(serde_json::to_string(&s).unwrap())
    })
    .double(INBOX_APPEND, |_| Ok("appended".into())).double(LOG_APPEND, |_| Ok("appended".into()));
    r.advance(T0 * 1000).unwrap();
    if let Some(body) = carry {
        r.tick(batcher_local::STREAM, EventKind::new(batcher_local::STATE, body)).unwrap();
    }
    r.tick(stream, kind).unwrap();
    r
}

const BLOCKED_CARRY: &str = r#"{"repos":{"spira":{"state":"Idle","blocked":{"sp-um4jw9":"sp-f0cw5z"}}}}"#;

#[test_id(thke1xw)]
#[test]
fn the_2026_11_17_page_does_not_fire_for_a_certified_bead_waiting_on_an_unlanded_dependency() {
    let r = dependency_rig(dependency_page_world, Some(BLOCKED_CARRY));
    assert_eq!(stalls(&r), Vec::<String>::new());
    assert!(log_lines(&r).iter().any(|l| l.contains("WAIT ") && l.contains("sp-um4jw9 waiting on sp-f0cw5z")), "{:?}", log_lines(&r));
}

#[test_id(tqph3vl)]
#[test]
fn a_certified_bead_the_batcher_refused_for_a_dependency_is_not_a_cut_stall_and_an_unblocked_one_still_is() {
    let now = T0;
    let mut s = dependency_page_world(now);
    mark_blocked(&mut s, &BTreeMap::from([("sp-um4jw9".to_string(), "sp-f0cw5z".to_string())]));
    assert_eq!(cut_stalls(&s, now), vec![]);
    s.beads.push(bead("sp-c2", "spira", "CERTIFIED", now - 10 * MIN));
    let found = cut_stalls(&s, now);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].evidence.contains("oldest uncovered: sp-c2"), "{}", found[0].evidence);
    assert!(found[0].evidence.contains("sp-um4jw9 waiting on sp-f0cw5z"), "{}", found[0].evidence);
    assert!(found[0].evidence.contains("oldest uncovered waited 10m") && !found[0].evidence.contains("no Cut for"), "{}", found[0].evidence);
}

#[test_id(tyjltk2)]
#[test]
fn a_submitted_bead_is_never_marked_as_waiting_on_a_dependency() {
    let now = T0;
    let mut s = dependency_page_world(now);
    mark_blocked(&mut s, &BTreeMap::from([("sp-s1".to_string(), "sp-x".to_string())]));
    assert!(s.beads.iter().all(|b| b.waits_on.is_none()));
}

#[test_id(tarm9xi)]
#[test]
fn a_real_cut_stall_still_pages_once() {
    let mut r = dependency_rig(
        |now| {
            let mut s = dependency_page_world(now);
            s.last = last("spira", &[("Claim", now - 18), ("Submit", now - 12 * MIN - 51), ("Cut", now - 6 * MIN), ("Delivered", now - 24 * MIN - 42)]);
            s
        },
        None,
    );
    r.advance(300_000).unwrap();
    let (_, stream, kind) = WatchTransitions::boot();
    r.tick(stream, kind).unwrap();
    let pages: Vec<String> = stalls(&r).into_iter().filter(|l| l.contains("P0 STALL SUBMITTED→IN_DELIVERY (spira)")).collect();
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert!(pages[0].contains("sp-um4jw9 CERTIFIED") && pages[0].contains("(certified, not cut)"), "{}", pages[0]);
}

#[test_id(tekj2vz)]
#[test]
fn the_forge_batchers_carry_is_read_too() {
    let mut r = dependency_rig(dependency_page_world, None);
    r.advance(300_000).unwrap();
    r.tick(batcher_forge::STREAM, EventKind::new(batcher_forge::STATE, r#"{"repos":{"spira":{"state":"Idle","round_key":null,"held":[],"base_moved":null,"conflict_streaks":{},"blocked":{"sp-um4jw9":"sp-f0cw5z"}}}}"#)).unwrap();
    let (_, stream, kind) = WatchTransitions::boot();
    r.tick(stream, kind).unwrap();
    let last_wait = log_lines(&r).into_iter().rev().find(|l| l.contains("WAIT ")).unwrap();
    assert!(last_wait.contains("sp-um4jw9 waiting on sp-f0cw5z"), "{last_wait}");
}

#[test_id(twpqwyt)]
#[test]
fn mark_blocked_clears_a_stale_dependency_the_batcher_no_longer_carries() {
    let mut s = dependency_page_world(T0);
    mark_blocked(&mut s, &BTreeMap::from([("sp-um4jw9".to_string(), "sp-f0cw5z".to_string())]));
    mark_blocked(&mut s, &BTreeMap::new());
    assert!(s.beads.iter().all(|b| b.waits_on.is_none()));
}

#[test_id(tcyfmbz)]
#[test]
fn a_certified_bead_blocked_by_an_idle_ready_bead_still_pages_the_cut_edge() {
    let mut r = dependency_rig(
        |now| {
            let mut s = dependency_world_with(now, "READY");
            s.last = last("spira", &[("Claim", now - 18), ("Submit", now - 12 * MIN - 51), ("Cut", now - 6 * MIN), ("Delivered", now - 24 * MIN - 42)]);
            s
        },
        Some(BLOCKED_CARRY),
    );
    r.advance(300_000).unwrap();
    let (_, stream, kind) = WatchTransitions::boot();
    r.tick(stream, kind).unwrap();
    let pages: Vec<String> = stalls(&r).into_iter().filter(|l| l.contains("P0 STALL SUBMITTED→IN_DELIVERY (spira)")).collect();
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert!(pages[0].contains("sp-um4jw9"), "{}", pages[0]);
}

#[test_id(to5v5y6)]
#[test]
fn a_prerequisite_absent_from_the_dwell_rows_is_not_live() {
    let mut s = dependency_page_world(T0);
    s.beads.retain(|b| b.id != "sp-f0cw5z");
    mark_blocked(&mut s, &BTreeMap::from([("sp-um4jw9".to_string(), "sp-f0cw5z".to_string())]));
    assert!(s.beads.iter().all(|b| b.waits_on.is_none()));
}

fn unreadable_during(activation: Option<spira_runtime::watchers::Activation>) -> Snapshot {
    Snapshot { activation, ..blind("spira-lc dwell: cannot tell: deadline") }
}

#[test_id(t95ap23)]
#[test]
fn an_unreadable_store_inside_an_activation_window_waits_and_does_not_page() {
    let a = spira_runtime::watchers::Activation { sha: "0123456789abcdef0123".into(), since: T0 - 10 };
    let (next, acts) = decide(&Carry::default(), &unreadable_during(Some(a)), T0 * 1000, &config());
    let said: Vec<&str> = acts.iter().filter(|a| a.name == INBOX_APPEND).map(|a| a.input.as_str()).collect();
    assert!(!said.iter().any(|t| t.contains("P0 BLIND")), "{said:?}");
    assert_eq!(said.iter().filter(|t| t.contains("WAIT BLIND: store restarting") && t.contains("0123456789ab") && t.contains("bound 75s")).count(), 1, "{said:?}");
    assert!(!next.blind);
}

fn maintained(pid: u32) -> spira_world::freeze::Maintenance {
    spira_world::freeze::Maintenance { owner: "concierge".into(), why: "compact the store".into(), since: T0 - 10, pid }
}

#[test_id(t88fza2)]
#[test]
fn an_unreadable_store_under_live_maintenance_waits_and_does_not_page() {
    let snap = Snapshot { maintenance: Some(maintained(std::process::id())), ..blind("spira-lc dwell: cannot tell: deadline") };
    let (next, acts) = decide(&Carry::default(), &snap, T0 * 1000, &config());
    let said: Vec<&str> = acts.iter().map(|a| a.input.as_str()).collect();
    assert!(!said.iter().any(|t| t.contains("P0")), "{said:?}");
    assert_eq!(said.iter().filter(|t| t.contains("WAIT BLIND: store stopped for maintenance by concierge since") && t.contains("compact the store")).count(), 1, "{said:?}");
    assert!(!next.blind);
}

#[test_id(tivttow)]
#[test]
fn an_unreadable_store_under_orphaned_maintenance_pages_once_naming_thaw_store() {
    let snap = Snapshot { maintenance: Some(maintained(u32::MAX)), ..blind("spira-lc dwell: cannot tell: deadline") };
    let (next, acts) = decide(&Carry::default(), &snap, T0 * 1000, &config());
    let pages: Vec<&str> = acts.iter().map(|a| a.input.as_str()).filter(|t| t.contains("P0 BLIND") && t.contains("orphaned") && t.contains("world thaw-store")).collect();
    assert_eq!(pages.len(), 1, "{acts:?}");
    assert!(!next.blind);
    let (_, again) = decide(&next, &snap, (T0 + 15) * 1000, &config());
    assert!(again.iter().all(|a| !a.input.contains("P0")), "paged once");
}

#[test_id(teqoifn)]
#[test]
fn an_unreadable_store_outside_any_activation_window_pages_blind() {
    let (next, acts) = decide(&Carry::default(), &unreadable_during(None), T0 * 1000, &config());
    assert!(acts.iter().any(|a| a.input.contains("P0 BLIND")));
    assert!(next.blind);
}

#[test_id(t8yekzf)]
#[test]
fn over_names_each_judged_row_past_its_bound_and_never_ready_or_rework() {
    let now = T0;
    let mut s = healthy(now);
    assert_eq!(over(&s, now, &config()), std::collections::BTreeSet::new());
    s.beads.push(bead("sp-r2", "spira", "READY", now - 30 * HOUR));
    s.beads.push(bead("sp-rw", "spira", "REWORK", now - 30 * HOUR));
    s.beads.push(bead("sp-w9", "spira", "WORKING", now - 91 * MIN));
    let mut ask = bead("sp-a1", "spira", "WORKING", now - 5 * MIN);
    ask.ask_held = true;
    ask.ask_since = Some(now - 61 * MIN);
    s.beads.push(ask);
    assert_eq!(over(&s, now, &config()), ["ASK", "WORKING"].into_iter().collect());
}

fn over_submitted(acts: &[runtime::event::EffectSpec]) -> Vec<String> {
    acts.iter().map(|a| a.input.clone()).filter(|l| l.contains("OVER SUBMITTED")).collect()
}

fn wait_world(now: u64, state: &str, since: u64, rounds_open: u64, gate: Option<Gating>) -> Snapshot {
    let mut s = healthy(now);
    let mut b = bead(if state == "CERTIFIED" { "sp-c1" } else { "sp-s1" }, "spira", state, since);
    b.gate = gate;
    s.beads = vec![b];
    s.last = last("spira", &[("Cut", now - 2 * MIN), ("Delivered", now - MIN)]);
    s.open_rounds = Some(rounds_in("spira", rounds_open));
    s.owners.clear();
    s
}

#[test_id(tcbh0jq)]
#[test]
fn a_certified_wait_behind_an_open_round_counts_from_the_round_landing() {
    let cfg = gate_cfg();
    let since = T0 - 31 * MIN;
    let pass = |carry: &Carry, at: u64, rounds: u64| decide(carry, &wait_world(at, "CERTIFIED", since, rounds, None), at * 1000, &cfg);
    let (c1, a1) = pass(&Carry::default(), T0, 1);
    assert_eq!(over_submitted(&a1), Vec::<String>::new());
    let (c2, a2) = pass(&c1, T0 + 5 * MIN, 0);
    assert_eq!(over_submitted(&a2), Vec::<String>::new());
    let (_, a3) = pass(&c2, T0 + 10 * MIN, 0);
    let lines = over_submitted(&a3);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("sp-c1") && lines[0].contains("CERTIFIED") && lines[0].contains("certified, not cut"), "{}", lines[0]);
}

#[test_id(tsw9k2m)]
#[test]
fn a_submitted_wait_behind_a_gate_counts_from_the_gate_finishing() {
    let cfg = gate_cfg();
    let since = T0 - 8 * MIN;
    let running = Some(Gating::Running { since: T0 - 6 * MIN });
    let (c1, a1) = decide(&Carry::default(), &wait_world(T0, "SUBMITTED", since, 0, running), T0 * 1000, &cfg);
    assert_eq!(over_submitted(&a1), Vec::<String>::new());
    let t2 = T0 + MIN;
    let (c2, a2) = decide(&c1, &wait_world(t2, "SUBMITTED", since, 0, None), t2 * 1000, &cfg);
    assert_eq!(over_submitted(&a2), Vec::<String>::new());
    let t3 = t2 + 5 * MIN;
    let (_, a3) = decide(&c2, &wait_world(t3, "SUBMITTED", since, 0, None), t3 * 1000, &cfg);
    let lines = over_submitted(&a3);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("sp-s1") && lines[0].contains("no gate queued or running"), "{}", lines[0]);
}

#[test_id(tnm4x7q)]
#[test]
fn the_over_submitted_line_names_the_bead() {
    let cfg = gate_cfg();
    let (_, acts) = decide(&Carry::default(), &wait_world(T0, "SUBMITTED", T0 - 8 * MIN, 0, None), T0 * 1000, &cfg);
    let lines = over_submitted(&acts);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("sp-s1"), "{}", lines[0]);
}

#[test_id(t839wy5)]
#[test]
fn the_unexplained_map_survives_a_restart_including_an_explained_bead() {
    let cfg = gate_cfg();
    let (carry, _) = decide(&Carry::default(), &wait_world(T0, "CERTIFIED", T0 - 31 * MIN, 1, None), T0 * 1000, &cfg);
    assert_eq!(carry.unexplained.get("sp-c1"), Some(&None));
    let parts = [runtime::rows::Part::Map { name: "unexplained", drop: &[] }];
    let body = serde_json::to_string(&carry).unwrap();
    let back: Carry = serde_json::from_str(&runtime::rows::join(&runtime::rows::split(&body, &parts), &parts)).unwrap();
    assert_eq!(back.unexplained, carry.unexplained);
}

#[test_id(tgbj1gv)]
#[test]
fn a_wait_explained_again_after_a_gap_is_counted_afresh_and_a_departed_bead_is_forgotten() {
    let cfg = gate_cfg();
    let since = T0 - 31 * MIN;
    let at = |carry: &Carry, t: u64, rounds: u64| decide(carry, &wait_world(t, "CERTIFIED", since, rounds, None), t * 1000, &cfg);
    let (c1, _) = at(&Carry::default(), T0, 1);
    let (c2, _) = at(&c1, T0 + 6 * MIN, 0);
    assert_eq!(c2.unexplained.get("sp-c1"), Some(&Some(T0 + 6 * MIN)));
    let (c3, a3) = at(&c2, T0 + 8 * MIN, 1);
    assert_eq!(c3.unexplained.get("sp-c1"), Some(&None));
    assert_eq!(over_submitted(&a3), Vec::<String>::new());
    let (c4, a4) = at(&c3, T0 + 9 * MIN, 0);
    assert_eq!(c4.unexplained.get("sp-c1"), Some(&Some(T0 + 9 * MIN)));
    assert_eq!(over_submitted(&a4), Vec::<String>::new());
    let mut gone = wait_world(T0 + 10 * MIN, "CERTIFIED", since, 0, None);
    gone.beads.clear();
    let (c5, _) = decide(&c4, &gone, (T0 + 10 * MIN) * 1000, &cfg);
    assert!(c5.unexplained.is_empty(), "{:?}", c5.unexplained);
}

fn pr_bead(id: &str, repo: &str, since: u64, read: Option<(PrStatus, u64)>) -> Bead {
    let read = read.map(|(status, read_since)| PrRead { number: 232, status, since: read_since });
    Bead { pr: Some(PrWait { read }), ..bead(id, repo, "SUBMITTED", since) }
}

fn pr_snap(beads: Vec<Bead>, landing_quiet_s: Option<u64>) -> Snapshot {
    let mut s = healthy(T0);
    s.beads = beads;
    s.last = last("spira", &[("Delivered", T0 - 3 * MIN)]);
    s.landing_quiet_s = landing_quiet_s;
    s
}

fn pr_stalls(s: &Snapshot, now: u64) -> Vec<Stall> {
    let cfg = Config { cut_bound_s: 5 * MIN, ..config() };
    spira_runtime::tasks::e::watch_transitions::stalls(s, now, &cfg).into_iter().filter(|x| x.edge == PR_EDGE).collect()
}

#[test_id(tk7rhml)]
#[test]
fn a_pr_submission_with_running_checks_pages_nothing_past_the_cut_bound() {
    let s = pr_snap(vec![pr_bead("sp-p1", "spira", T0 - 7 * MIN, Some((PrStatus::Running, T0 - 7 * MIN)))], Some(90));
    let cfg = Config { cut_bound_s: 5 * MIN, ..config() };
    assert!(spira_runtime::tasks::e::watch_transitions::stalls(&s, T0, &cfg).is_empty());
}

#[test_id(ttfwpae)]
#[test]
fn a_pr_submission_whose_checks_are_red_pages_at_once_naming_the_pull_request_and_the_bound() {
    let s = pr_snap(vec![pr_bead("sp-p1", "spira", T0 - 10, Some((PrStatus::Red, T0 - 10)))], Some(90));
    let found = pr_stalls(&s, T0);
    assert_eq!(found.iter().map(Stall::key).collect::<Vec<_>>(), ["SUBMITTED→LANDED spira"]);
    assert!(found[0].evidence.contains("sp-p1") && found[0].evidence.contains("PR #232 red") && found[0].evidence.contains("bound 15m"), "{}", found[0].evidence);
}

#[test_id(tj1170f)]
#[test]
fn a_pr_wait_with_no_pull_request_unread_green_or_merged_pages_at_the_bound_and_not_a_second_before() {
    let waits = [("unread", Some(PrStatus::Unread)), ("green", Some(PrStatus::Green)), ("merged", Some(PrStatus::Merged))];
    for (what, status) in waits {
        let at = |age: u64| pr_snap(vec![pr_bead("sp-p1", "spira", T0 - 2 * HOUR, status.map(|st| (st, T0 - age)))], Some(90));
        assert_eq!(pr_stalls(&at(899), T0).len(), 0, "{what} at 899s");
        let found = pr_stalls(&at(900), T0);
        assert_eq!(found.len(), 1, "{what} at 900s");
        assert!(found[0].evidence.contains(what), "{what}: {}", found[0].evidence);
    }
    let no_pr = |age: u64| pr_snap(vec![pr_bead("sp-p1", "spira", T0 - age, None)], Some(90));
    assert_eq!(pr_stalls(&no_pr(899), T0).len(), 0);
    let found = pr_stalls(&no_pr(900), T0);
    assert_eq!(found.len(), 1);
    assert!(found[0].evidence.contains("no pull request"), "{}", found[0].evidence);
}

#[test_id(tc9nqhb)]
#[test]
fn running_checks_page_at_the_bound_when_the_landing_task_has_gone_silent() {
    let at = |age: u64, quiet: Option<u64>| pr_snap(vec![pr_bead("sp-p1", "spira", T0 - 2 * HOUR, Some((PrStatus::Running, T0 - age)))], quiet);
    assert_eq!(pr_stalls(&at(899, Some(601)), T0).len(), 0);
    assert_eq!(pr_stalls(&at(900, Some(601)), T0).len(), 1);
    assert_eq!(pr_stalls(&at(900, None), T0).len(), 1, "a landing task that never stamped is silent");
    assert_eq!(pr_stalls(&at(3 * HOUR, Some(599)), T0).len(), 0);
}

#[test_id(t8i47ff)]
#[test]
fn a_held_or_landed_pr_submission_does_not_page() {
    let old = Some((PrStatus::Red, T0 - HOUR));
    let held = Bead { held: true, ..pr_bead("sp-p1", "spira", T0 - 2 * HOUR, old) };
    let landed = Bead { landed: true, ..pr_bead("sp-p2", "spira", T0 - 2 * HOUR, old) };
    assert_eq!(pr_stalls(&pr_snap(vec![held, landed], Some(90)), T0).len(), 0);
}

#[test_id(tonlg15)]
#[test]
fn a_covered_pr_submission_beside_an_owned_gate_mode_wait_pages_nothing_on_all_repos() {
    let now = T0;
    let mut s = pr_snap(vec![pr_bead("sp-p1", "other", now - 7 * MIN, Some((PrStatus::Running, now - 7 * MIN))), bead("sp-g1", "spira", "SUBMITTED", now - 30 * MIN)], Some(90));
    s.last = last("spira", &[("Delivered", now - 3 * MIN)]);
    s.owners.insert(cut_owner_key("spira"), Owner { beat: Some(now - 10), failed: None });
    let cfg = Config { cut_bound_s: 5 * MIN, ..config() };
    assert!(spira_runtime::tasks::e::watch_transitions::stalls(&s, now, &cfg).is_empty());
}

#[test_id(tg1x0vn)]
#[test]
fn a_pr_page_goes_out_once_through_decide_and_names_the_edge() {
    let s = pr_snap(vec![pr_bead("sp-p1", "spira", T0 - 10, Some((PrStatus::Red, T0 - 10)))], Some(90));
    let (carry, acts) = decide(&Carry::default(), &s, T0 * 1000, &config());
    assert!(acts.iter().any(|a| a.input.contains("P0 STALL SUBMITTED→LANDED (spira)") && a.input.contains("sp-p1")), "{acts:?}");
    let (_, again) = decide(&carry, &s, T0 * 1000 + 1000, &config());
    assert!(!again.iter().any(|a| a.input.contains("P0 STALL")), "{again:?}");
}

#[test_id(t02y1zq)]
#[test]
fn mark_prs_marks_submitted_beads_of_pr_repos_and_fill_prs_gives_them_the_landing_read() {
    let mut s = pr_snap(vec![bead("sp-a", "prrepo", "SUBMITTED", T0), bead("sp-b", "gaterepo", "SUBMITTED", T0), bead("sp-c", "prrepo", "WORKING", T0)], None);
    mark_prs(&mut s, &|repo| repo == "prrepo");
    let read = PrRead { number: 7, status: PrStatus::Green, since: 5 };
    fill_prs(&mut s, &BTreeMap::from([("sp-a".to_string(), read), ("sp-b".to_string(), read), ("sp-c".to_string(), read)]));
    let marks: Vec<Option<PrWait>> = s.beads.iter().map(|b| b.pr).collect();
    assert_eq!(marks, vec![Some(PrWait { read: Some(read) }), None, None]);
}

const RELEASE_SHA: &str = "f72cbc540a1111111111111111111111111111aa";

fn run_with_record(published_at: u64) -> testkit::TempDir {
    let dir = testkit::TempDir::new("transitions-restart");
    let event = |to, at| release::machine::ReleaseEvent { to, reason: "r".into(), at, restarted: vec![] };
    release::machine::save(&dir.path().join("release"), &release::machine::Release { sha: RELEASE_SHA.into(), events: vec![event(release::machine::ReleaseState::Published, published_at)] }).unwrap();
    dir
}

fn restart_config(dir: &testkit::TempDir) -> Config {
    Config { run: dir.path().to_path_buf(), ..config() }
}

fn healthy_rig(cfg: Config) -> TaskRig {
    TaskRig::new(1, WatchTransitions::new(cfg).registration()).unwrap().double(OBSERVE, |input| {
        let now = serde_json::from_str::<serde_json::Value>(input).unwrap()["now"].as_u64().unwrap();
        Ok(serde_json::to_string(&healthy(now)).unwrap())
    })
    .double(INBOX_APPEND, |_| Ok("appended".into()))
    .double(LOG_APPEND, |_| Ok("appended".into()))
}

fn start(mut r: TaskRig) -> TaskRig {
    let (_, stream, kind) = WatchTransitions::boot();
    r.advance(T0 * 1000).unwrap();
    r.tick(stream, kind).unwrap();
    r
}

fn timing_out_rig(cfg: Config) -> TaskRig {
    TaskRig::new(1, WatchTransitions::new(cfg).registration()).unwrap().timing_out().double(INBOX_APPEND, |_| Ok("appended".into())).double(LOG_APPEND, |_| Ok("appended".into()))
}

fn blind_pages(r: &TaskRig) -> Vec<String> {
    lines(r).into_iter().filter(|l| l.contains("P0 BLIND")).collect()
}

fn restart_waits(r: &TaskRig) -> Vec<String> {
    lines(r).into_iter().filter(|l| l.contains("WAIT BLIND: store restarting")).collect()
}

#[test_id(tc0vbpa)]
#[test]
fn a_timed_out_observe_inside_an_activation_window_waits_and_the_next_period_past_the_bound_pages_once() {
    let dir = run_with_record(T0 - 10);
    let mut r = start(timing_out_rig(restart_config(&dir)));
    assert!(blind_pages(&r).is_empty(), "{:?}", lines(&r));
    let w = restart_waits(&r);
    assert_eq!(w.len(), 1, "{:?}", lines(&r));
    assert!(w[0].contains("f72cbc540a11") && w[0].contains("bound 75s"), "{w:?}");
    r.advance(300_000).unwrap();
    let p = blind_pages(&r);
    assert_eq!(p.len(), 1, "{:?}", lines(&r));
    assert!(p[0].contains("the observe timed out"), "{p:?}");
}

#[test_id(tskaato)]
#[test]
fn a_failed_observe_inside_an_activation_window_waits() {
    let dir = run_with_record(T0 - 10);
    let r = start(TaskRig::new(1, WatchTransitions::new(restart_config(&dir)).registration()).unwrap().double(OBSERVE, |_| Err("store unavailable".into())).double(INBOX_APPEND, |_| Ok("appended".into())).double(LOG_APPEND, |_| Ok("appended".into())));
    assert!(blind_pages(&r).is_empty(), "{:?}", lines(&r));
    assert_eq!(restart_waits(&r).len(), 1, "{:?}", lines(&r));
}

#[test_id(t0m6skv)]
#[test]
fn an_observe_overtaken_by_a_restart_and_reopened_inside_the_window_never_pages_and_the_table_returns() {
    let dir = run_with_record(T0 - 10);
    let cfg = restart_config(&dir);
    let mut r = start(healthy_rig(cfg.clone()).hanging(OBSERVE));
    r.advance(5_000).unwrap();
    r.reopen(WatchTransitions::new(cfg).registration()).unwrap();
    let (_, stream, kind) = WatchTransitions::boot();
    r.tick(stream, kind).unwrap();
    r.advance(1_000).unwrap();
    assert!(blind_pages(&r).is_empty(), "{:?}", lines(&r));
    assert!(log_lines(&r).iter().any(|l| l.contains("READY")), "{:?}", log_lines(&r));
}

#[test_id(txg33jn)]
#[test]
fn a_timed_out_observe_under_a_live_maintenance_waits_without_a_page() {
    let dir = run_with_record(T0 - 10_000);
    let held = spira_world::freeze::Maintenance { owner: "operator".into(), why: "restoring the store".into(), since: T0 - 5, pid: std::process::id() };
    std::fs::write(spira_world::freeze::maintenance_path(dir.path()), serde_json::to_string(&held).unwrap()).unwrap();
    let r = start(timing_out_rig(restart_config(&dir)));
    assert!(blind_pages(&r).is_empty(), "{:?}", lines(&r));
    assert!(lines(&r).iter().any(|l| l.contains("WAIT BLIND: store stopped for maintenance")), "{:?}", lines(&r));
}

#[test_id(t8j2pfo)]
#[test]
fn a_request_inside_the_bound_with_its_outcome_past_it_waits_and_both_past_it_pages() {
    let inside = run_with_record(T0 - 60);
    let r = start(timing_out_rig(restart_config(&inside)));
    assert!(blind_pages(&r).is_empty(), "{:?}", lines(&r));
    assert_eq!(restart_waits(&r).len(), 1, "{:?}", lines(&r));
    let past = run_with_record(T0 - 100);
    let r = start(timing_out_rig(restart_config(&past)));
    assert_eq!(blind_pages(&r).len(), 1, "{:?}", lines(&r));
    assert!(restart_waits(&r).is_empty(), "{:?}", lines(&r));
}
