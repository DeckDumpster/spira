//! The conditions `watch.throughput` holds on `watch/throughput/<repo>`, on the task rig's
//! virtual clock against a fake host. The world's times stand still while the clock runs, so
//! every age in a line moves pass by pass.

#[macro_use]
extern crate spira_test_id;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, TaskRig, DAY_MS};
use spira_runtime::tasks::e::watch_throughput::*;
use spira_runtime::watch_topics::Change;
use spira_runtime::watchers::{Activation, INBOX_APPEND, WATCH_PUBLISH};

const NOW: u64 = 3 * 86_400;

fn config() -> Config {
    Config { stall_s: 600, abandon_count: 5, abandon_window_s: 7_200, ratio_min_pct: 40, ratio_min_runs: 6, passed_grace_s: 300, activation_bound_s: 75 }
}

fn stalled_repo(repo: &str) -> RepoLanding {
    RepoLanding { repo: repo.into(), hourly: vec![0; 24], last_landing: NOW - 3_600, waiting: 1, oldest: Some(Waiting { bead: format!("sp-{repo}"), since: Some(NOW - 3_600) }), undated: None, beads: vec![] }
}

fn stalled(repos: &[&str]) -> Snapshot {
    Snapshot { readable: true, landings: repos.iter().map(|r| stalled_repo(r)).collect(), ..Default::default() }
}

fn quiet() -> Snapshot {
    Snapshot { readable: true, ..Default::default() }
}

fn abandoned(repo: &str, n: u64) -> RepoRounds {
    RepoRounds { repo: repo.into(), cut: n, abandoned: n, abandoned_recent: n, ..Default::default() }
}

fn unticked(world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let r = TaskRig::new(1, WatchThroughput::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(FILE, |_| Ok("filed".into()))
        .double(CLEAR, |_| Ok("cleared".into()))
        .double(INBOX_APPEND, |_| Ok("appended".into()))
        .double(WATCH_PUBLISH, |_| Ok("published".into()));
    r.clock.advance_to(NOW * 1000);
    r
}

fn start(mut r: TaskRig) -> TaskRig {
    let (_, stream, kind) = WatchThroughput::boot();
    r.tick(stream, kind).unwrap();
    r
}

fn rig(world: Arc<Mutex<Snapshot>>) -> TaskRig {
    start(unticked(world))
}

fn world(s: Snapshot) -> (TaskRig, Arc<Mutex<Snapshot>>) {
    let w = Arc::new(Mutex::new(s));
    (rig(w.clone()), w)
}

fn pass(r: &mut TaskRig, w: &Mutex<Snapshot>, s: Snapshot) {
    *w.lock().unwrap() = s;
    r.advance(CADENCE_MS).unwrap();
}

fn changes(r: &TaskRig) -> Vec<Change> {
    r.requested().iter().filter(|q| q.name == WATCH_PUBLISH).map(|q| serde_json::from_str(&q.input).unwrap()).collect()
}

fn lines(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn subjects(c: &Change) -> (Vec<&str>, Vec<&str>) {
    (c.set.keys().map(String::as_str).collect(), c.clear.iter().map(String::as_str).collect())
}

#[test_id(tofjpuz)]
#[test]
fn a_stall_held_over_three_passes_is_one_publish_and_the_clearing_pass_clears_it() {
    let (mut r, w) = world(stalled(&["alpha"]));
    r.advance(2 * CADENCE_MS).unwrap();
    let said = changes(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!((said[0].row.as_str(), subjects(&said[0])), ("throughput", (vec!["alpha"], vec![])));
    let v = &said[0].set["alpha"];
    assert!(v.text.starts_with("P0 alpha: landing stalled"), "{v:?}");
    assert_eq!((v.level.as_str(), v.since, v.at), ("P0", NOW, NOW));
    assert_eq!(lines(&r).len(), 1);
    assert!(lines(&r)[0].ends_with(&v.text), "the topic says the inbox line, untagged: {:?}", lines(&r));
    r.advance(60 * CADENCE_MS).unwrap();
    assert_eq!(changes(&r).len(), 1, "an hour on it is still the one value");
    pass(&mut r, &w, quiet());
    let said = changes(&r);
    assert_eq!(said.len(), 2, "{said:?}");
    assert_eq!(subjects(&said[1]), (vec![], vec!["alpha"]));
}

#[test_id(tmzu1xy)]
#[test]
fn two_stalled_repos_are_two_subjects() {
    let (mut r, w) = world(stalled(&["spira", "dumpstar"]));
    r.advance(2 * CADENCE_MS).unwrap();
    let said = changes(&r);
    let set: BTreeSet<&str> = said.iter().flat_map(|c| c.set.keys().map(String::as_str)).collect();
    assert_eq!(set, ["dumpstar", "spira"].into(), "{said:?}");
    assert!(said.iter().all(|c| c.clear.is_empty()) && said.len() == 2, "{said:?}");
    pass(&mut r, &w, stalled(&["spira"]));
    assert_eq!(subjects(changes(&r).last().unwrap()), (vec![], vec!["dumpstar"]));
}

#[test_id(tgs7cy3)]
#[test]
fn one_repos_conditions_join_in_line_order_under_the_most_urgent_level_and_keep_their_since() {
    let mut s = quiet();
    s.landings = vec![RepoLanding { oldest: None, undated: Some("sp-null".into()), ..stalled_repo("alpha") }];
    let (mut r, w) = world(s.clone());
    let first = changes(&r);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!((first[0].set["alpha"].level.as_str(), first[0].set["alpha"].since), ("P2", NOW), "a null since alone");
    assert!(first[0].set["alpha"].text.starts_with("P2 alpha: data defect"), "{first:?}");

    s.repos = vec![RepoRounds { green_runs: 10, ..abandoned("alpha", 5) }];
    pass(&mut r, &w, s.clone());
    let said = changes(&r);
    let v = &said[1].set["alpha"];
    let parts: Vec<&str> = v.text.split("; ").filter(|p| p.starts_with("P0 alpha") || p.starts_with("P2 alpha")).collect();
    assert_eq!(parts.len(), 3, "{v:?}");
    assert!(parts[0].contains("data defect") && parts[1].contains("5 abandoned rounds") && parts[2].contains("passing test runs"), "{v:?}");
    assert_eq!((v.level.as_str(), v.since, v.at), ("P0", NOW, NOW + 60));

    s.repos = vec![RepoRounds { green_runs: 11, ..abandoned("alpha", 9) }];
    pass(&mut r, &w, s);
    assert_eq!(changes(&r).len(), 2, "a standing breach whose counts move is the same condition");
}

#[test_id(t1gqpuc)]
#[test]
fn a_passed_round_a_refused_record_and_a_dependency_wait_are_no_conditions() {
    let mut s = quiet();
    s.unlanded = vec![Unlanded { repo: "alpha".into(), batch: "b-1".into(), state: "GREEN".into(), age_s: 900 }];
    s.records = Records { clear: vec![], held: vec!["q/b/rec names round b-1, which is OPEN".into()] };
    s.landings = vec![RepoLanding { beads: vec!["sp-w".into()], ..stalled_repo("beta") }];
    s.states = [("sp-dep".to_string(), "WORKING".to_string())].into();
    let (stream, state) = (spira_runtime::tasks::a::batcher_local::STREAM, spira_runtime::tasks::a::batcher_local::STATE);
    let mut r = unticked(Arc::new(Mutex::new(s)));
    r.tick(stream, runtime::event::EventKind::new(state, r#"{"repos":{"beta":{"state":"Idle","blocked":{"sp-w":"sp-dep"}}}}"#)).unwrap();
    let mut r = start(r);
    r.advance(2 * CADENCE_MS).unwrap();
    for said in ["round b-1 passed", "round record refused clearing", "beta: landing waits on a dependency"] {
        assert!(lines(&r).iter().any(|l| l.contains(said)), "{said} is still said: {:?}", lines(&r));
    }
    assert_eq!(changes(&r), vec![]);
}

#[test_id(tv6zp55)]
#[test]
fn blind_is_a_condition_on_all_once_paged_and_keeps_the_standing_ones_until_a_good_read() {
    let (mut r, w) = world(stalled(&["alpha"]));
    let blind = Snapshot { why: "landings: gone".into(), ..Default::default() };
    pass(&mut r, &w, Snapshot { activation: Some(Activation { sha: "0123456789abcdef".into(), since: NOW }), ..blind.clone() });
    *w.lock().unwrap() = blind;
    r.advance(10 * CADENCE_MS).unwrap();
    assert_eq!(changes(&r).len(), 1, "WAIT BLIND and a blind inside its bound are no condition: {:?}", changes(&r));
    r.advance(5 * CADENCE_MS).unwrap();
    let said = changes(&r);
    assert_eq!(said.len(), 2, "{said:?}");
    assert_eq!(subjects(&said[1]), (vec!["all"], vec![]), "the stall it can no longer see is not cleared");
    assert!(said[1].set["all"].text.starts_with("P0 BLIND: the landings cannot be read"), "{said:?}");
    pass(&mut r, &w, stalled(&["alpha"]));
    let said = changes(&r);
    assert_eq!(said.len(), 3, "{said:?}");
    assert_eq!(subjects(&said[2]), (vec![], vec!["all"]));
}

#[test_id(tku2pbr)]
#[test]
fn a_restarted_row_publishes_nothing_again_and_asks_its_observe_with_what_it_published() {
    let (mut r, _w) = world(stalled(&["alpha"]));
    r.reopen(WatchThroughput::new(config()).registration()).unwrap();
    let mark = r.requested().len();
    let (_, stream, kind) = WatchThroughput::boot();
    r.tick(stream, kind).unwrap();
    let asked = &r.requested()[mark..];
    assert!(asked.iter().all(|q| q.name != WATCH_PUBLISH), "{asked:?}");
    let observe = asked.iter().find(|q| q.name == OBSERVE).expect("the boot observes");
    let input: serde_json::Value = serde_json::from_str(&observe.input).unwrap();
    assert_eq!(input["row"], "throughput");
    assert!(input["published"]["alpha"]["text"].as_str().unwrap().contains("landing stalled"), "{input}");
    assert_eq!(input["abandon_window_s"], 7_200, "{input}");
}

#[test_id(tln6msz)]
#[test]
fn the_carry_is_kept_as_rows_including_what_is_published() {
    let (r, _w) = world(stalled(&["alpha"]));
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.iter().any(|k| k.starts_with("published/")) && rows.iter().any(|k| k == "conditions"), "{rows:?}");
}

#[test_id(typg3bc)]
#[test]
fn two_days_of_a_standing_stall_replay_identically() {
    let r = assert_deterministic(|_| rig(Arc::new(Mutex::new(stalled(&["alpha"])))), 7, 2);
    assert!(r.now() >= NOW * 1000 + 2 * DAY_MS);
    assert_eq!(changes(&r).len(), 1, "{:?}", changes(&r));
}

fn orphaned(since: u64) -> Snapshot {
    let m = spira_world::freeze::Maintenance { owner: "concierge".into(), why: "compact".into(), since, pid: u32::MAX };
    Snapshot { maintenance: Some(m), why: "landings: gone".into(), ..Default::default() }
}

fn all_text(c: &Change) -> &str {
    c.set.get("all").map_or("", |v| v.text.as_str())
}

#[test_id(tn4r4r8)]
#[test]
fn an_orphaned_maintenance_is_held_on_all_until_it_stops_holding_and_the_store_blind_says_its_own_line() {
    let (mut r, w) = world(orphaned(NOW));
    r.advance(2 * CADENCE_MS).unwrap();
    let said = changes(&r);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(all_text(&said[0]).contains("orphaned"), "{said:?}");
    *w.lock().unwrap() = Snapshot { why: "landings: gone".into(), ..Default::default() };
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(subjects(changes(&r).last().unwrap()), (vec![], vec!["all"]), "thawed, and not yet blind past the bound");
    r.advance(12 * CADENCE_MS).unwrap();
    let said = changes(&r);
    assert_eq!(said.len(), 3, "{said:?}");
    let text = all_text(&said[2]);
    assert!(text.starts_with("P0 BLIND: the landings cannot be read") && !text.contains("orphaned"), "{said:?}");
    pass(&mut r, &w, orphaned(NOW + 5));
    let text = all_text(changes(&r).last().unwrap()).to_string();
    assert!(text.contains("orphaned") && text.contains("P0 BLIND: the landings"), "both stand: {text}");
    pass(&mut r, &w, quiet());
    assert_eq!(subjects(changes(&r).last().unwrap()), (vec![], vec!["all"]));
}

#[test_id(tn9ieha)]
#[test]
fn a_clear_lost_with_a_stopping_runtime_is_asked_for_again() {
    let w = Arc::new(Mutex::new(stalled(&["alpha"])));
    let mut r = start(unticked(w.clone()).hanging(WATCH_PUBLISH));
    pass(&mut r, &w, quiet());
    let mark = changes(&r).len();
    r.reopen(WatchThroughput::new(config()).registration()).unwrap();
    r.run_until_idle().unwrap();
    let again = changes(&r)[mark..].to_vec();
    assert_eq!(again.len(), 1, "{again:?}");
    assert_eq!(subjects(&again[0]), (vec![], vec!["alpha"]), "the lost set is past; the lost clear is not");
}
