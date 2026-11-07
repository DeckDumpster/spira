//! Tests for `watch.throughput`: the 24 h landing floor that stops the world and pages, and the
//! per-repo "abandoned rounds, no landings" page.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::{HALT_STREAM, SCHEDULER_HALTED};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_throughput::*;
use spira_runtime::watchers::INBOX_APPEND;

const HOUR_MS: u64 = 3_600_000;

fn config() -> Config {
    Config { floor: 150, abandon_count: 5, abandon_window_s: 7_200, restop_after_s: 3_600 }
}

fn landed(n: u64) -> Snapshot {
    let mut hourly = vec![n / 24; 24];
    hourly[23] += n % 24;
    Snapshot { readable: true, halted: false, hourly, landed_refs: n, repos: vec![] }
}

fn starved(repo: &str, abandoned: u64, landed_recent: u64) -> RepoRounds {
    RepoRounds { repo: repo.into(), cut: abandoned + landed_recent, abandoned, landed: landed_recent, abandoned_recent: abandoned, landed_recent }
}

fn rig(world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchThroughput::boot();
    let mut r = TaskRig::new(1, WatchThroughput::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(STAMP, |_| Ok("stamped".into())).double(INBOX_APPEND, |_| Ok("appended".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn named(r: &TaskRig, effect: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == effect).map(|q| q.input.clone()).collect()
}

fn halts(r: &TaskRig) -> usize {
    r.journaled().iter().filter(|e| e.stream.0 == HALT_STREAM && e.kind.tag == SCHEDULER_HALTED).count()
}

#[test]
fn the_row_is_registered_and_its_boot_arms_the_fifteen_minute_tick() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(CADENCE_MS, 15 * 60_000);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
}

#[test]
fn a_hundred_and_forty_nine_landings_in_a_day_stop_the_world_and_page_once() {
    let r = rig(Arc::new(Mutex::new(landed(149))));
    assert_eq!(halts(&r), 1, "the scheduler is halted");
    assert_eq!(named(&r, STAMP).len(), 1, "the world.halted stamp is written");
    let pages = named(&r, INBOX_APPEND);
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert!(pages[0].contains("THROUGHPUT FLOOR: 149 landed/24h < 150 — world stopped; root-cause and propose"), "{pages:?}");
}

#[test]
fn a_hundred_and_fifty_landings_in_a_day_do_not_stop_it() {
    let mut r = rig(Arc::new(Mutex::new(landed(150))));
    assert_eq!((halts(&r), named(&r, STAMP).len(), named(&r, INBOX_APPEND).len()), (0, 0, 0));
    assert_silent_tick(&mut r, STREAM);
}

#[test]
fn the_page_carries_the_hourly_counts_and_each_repos_rounds() {
    let mut s = landed(100);
    s.hourly = (0..24).map(|h| h % 5).collect();
    s.repos = vec![RepoRounds { repo: "alpha".into(), cut: 9, abandoned: 4, landed: 3, abandoned_recent: 0, landed_recent: 1 }];
    let r = rig(Arc::new(Mutex::new(s)));
    let page = named(&r, INBOX_APPEND).remove(0);
    assert!(page.contains("hourly: 0 1 2 3 4 0 1 2 3 4 0 1 2 3 4 0 1 2 3 4 0 1 2 3"), "{page}");
    assert!(page.contains("alpha: cut 9 abandoned 4 landed 3"), "{page}");
}

#[test]
fn the_larger_of_the_lifecycle_and_the_landing_refs_is_the_count() {
    let mut s = landed(149);
    s.landed_refs = 150;
    assert_eq!(s.landed(), 150, "an export that lags the refs does not stop the world");
    let r = rig(Arc::new(Mutex::new(s)));
    assert_eq!(halts(&r), 0);
}

#[test]
fn it_does_not_stop_again_until_resumed_and_an_hour_has_passed() {
    let w = Arc::new(Mutex::new(landed(10)));
    let mut r = rig(w.clone());
    assert_eq!(halts(&r), 1);
    w.lock().unwrap().halted = true;
    r.advance(2 * CADENCE_MS).unwrap();
    assert_eq!(halts(&r), 1, "still stopped: once");
    w.lock().unwrap().halted = false;
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(halts(&r), 1, "resumed, but within the hour");
    r.advance(HOUR_MS + CADENCE_MS).unwrap();
    let stops = r.journaled().iter().filter(|e| e.kind.tag == STATE).filter(|e| serde_json::from_str::<Carry>(&e.kind.body).is_ok_and(|c| c.stop == Stop::Stopped)).count();
    assert_eq!(stops, 2, "an hour after the resume it decides to stop again (the rig stays halted, so its effects are held)");
}

#[test]
fn a_world_already_stopped_is_not_stopped_or_paged_again() {
    let mut s = landed(0);
    s.halted = true;
    let r = rig(Arc::new(Mutex::new(s)));
    assert_eq!((halts(&r), named(&r, INBOX_APPEND).len()), (0, 0));
}

#[test]
fn a_count_that_cannot_be_read_pages_blind_once_and_never_stops_the_world() {
    let w = Arc::new(Mutex::new(Snapshot { readable: false, ..landed(0) }));
    let mut r = rig(w);
    r.advance(4 * CADENCE_MS).unwrap();
    assert_eq!(halts(&r), 0);
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("BLIND"), "{pages:?}");
}

#[test]
fn five_abandoned_rounds_and_no_landing_page_the_repo_by_name_without_stopping_the_world() {
    let mut s = landed(400);
    s.repos = vec![starved("stuck-repo", 5, 0), starved("quiet", 4, 0), starved("busy", 9, 1)];
    let mut r = rig(Arc::new(Mutex::new(s)));
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("stuck-repo") && pages[0].contains("5 abandoned") && !pages[0].contains("quiet") && !pages[0].contains("busy"), "{pages:?}");
    assert_eq!(halts(&r), 0);
    r.advance(4 * CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1, "a standing condition pages once");
}

#[test]
fn a_repo_that_lands_again_and_starves_again_is_paged_again() {
    let mut s = landed(400);
    s.repos = vec![starved("stuck-repo", 5, 0)];
    let w = Arc::new(Mutex::new(s.clone()));
    let mut r = rig(w.clone());
    s.repos = vec![starved("stuck-repo", 5, 1)];
    *w.lock().unwrap() = s.clone();
    r.advance(CADENCE_MS).unwrap();
    s.repos = vec![starved("stuck-repo", 6, 0)];
    *w.lock().unwrap() = s;
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 2);
}

#[test]
fn every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_throughput.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test]
fn two_days_replay_identically_inside_the_budget() {
    let r = assert_deterministic(|_| rig(Arc::new(Mutex::new(landed(300)))), 7, 2);
    acc::assert_healthy(&r, STREAM);
    assert_journal_budget(&r, 1);
}

#[test]
fn bead_ids_are_the_prefixed_words_in_commit_subjects_and_nothing_near_them() {
    use spira_runtime::world::bead_ids;
    let found = bead_ids("spira: land sp-ipwlic — Every watcher\nsp-nxsurz: round — x\nsp-ipwlic: again\nspira-lc: not a bead\nsp-: bare\nresp-abc no", "sp");
    assert_eq!(found.into_iter().collect::<Vec<_>>(), ["sp-ipwlic", "sp-nxsurz"]);
    assert!(bead_ids("sp-ipwlic: x", "zz").is_empty(), "a configured prefix, not a literal");
}

#[test]
fn round_counts_split_a_repos_rounds_by_how_they_ended_and_how_recently() {
    use spira_runtime::world::round_counts;
    let b = |repo: &str, state: &str, last: u64| serde_json::json!({ "repo": repo, "state": state, "opened_at": 1, "last_at": last });
    let rows = [b("a", "ABANDONED", 9_900), b("a", "ABANDONED", 1_000), b("a", "LANDED", 9_000), b("a", "CI_RUNNING", 9_990), b("b", "SETTLED", 100)];
    let got = round_counts(&rows, 10_000, 7_200);
    assert_eq!(got[0], RepoRounds { repo: "a".into(), cut: 4, abandoned: 2, landed: 1, abandoned_recent: 1, landed_recent: 1 });
    assert_eq!(got[1], RepoRounds { repo: "b".into(), cut: 1, abandoned: 0, landed: 1, abandoned_recent: 0, landed_recent: 0 });
}

#[test]
fn landed_rows_are_bucketed_by_hour_with_the_newest_hour_last() {
    use spira_runtime::world::hourly_landed;
    let now = 1_000_000u64;
    let iso = |t: u64| spira_runtime::watchers::iso8601(t);
    let row = |t: u64, machine: &str, state: &str, applied: bool| serde_json::json!({ "ts": iso(t), "machine": machine, "to_state": state, "applied": applied });
    let rows = [row(now - 10, "bead", "LANDED", true), row(now - 3_599, "bead", "LANDED", true), row(now - 3_600, "bead", "LANDED", true), row(now - 86_399, "bead", "LANDED", true), row(now - 86_400, "bead", "LANDED", true), row(now - 5, "bead", "LANDED", false), row(now - 5, "batch", "LANDED", true), row(now - 5, "bead", "SUBMITTED", true)];
    let h = hourly_landed(&rows, now);
    assert_eq!((h.len(), h[23], h[22], h[0], h.iter().sum::<u64>()), (24, 2, 1, 1, 4), "{h:?}");
}
