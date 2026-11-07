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
    Config { floor: 150, abandon_count: 5, abandon_window_s: 7_200, restop_after_s: 3_600, ratio_min_pct: 40, ratio_min_runs: 6, passed_grace_s: 300 }
}

fn landed(n: u64) -> Snapshot {
    let mut hourly = vec![n / 24; 24];
    hourly[23] += n % 24;
    Snapshot { readable: true, halted: false, hourly, landed_refs: n, ..Default::default() }
}

fn starved(repo: &str, abandoned: u64, landed_recent: u64) -> RepoRounds {
    RepoRounds { repo: repo.into(), cut: abandoned + landed_recent, abandoned, landed: landed_recent, abandoned_recent: abandoned, landed_recent, green_runs: 0 }
}

fn rig(world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchThroughput::boot();
    let mut r = TaskRig::new(1, WatchThroughput::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(STAMP, |_| Ok("stamped".into())).double(CLEAR, |_| Ok("cleared".into())).double(INBOX_APPEND, |_| Ok("appended".into()));
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
    s.repos = vec![RepoRounds { repo: "alpha".into(), cut: 9, abandoned: 4, landed: 3, abandoned_recent: 0, landed_recent: 1, green_runs: 0 }];
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

fn repo_with_runs(repo: &str, green_runs: u64, landed: u64) -> RepoRounds {
    RepoRounds { repo: repo.into(), cut: green_runs, landed, green_runs, ..Default::default() }
}

#[test]
fn a_repo_whose_passing_runs_mostly_do_not_land_is_paged_once_and_a_healthy_or_thin_one_is_not() {
    let mut s = landed(400);
    s.repos = vec![repo_with_runs("diluted", 10, 3), repo_with_runs("healthy", 10, 4), repo_with_runs("thin", 5, 0)];
    let mut r = rig(Arc::new(Mutex::new(s)));
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("diluted: 3 landed against 10 passing test runs in 24h, under 40%"), "{pages:?}");
    assert_eq!(halts(&r), 0);
    r.advance(4 * CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1, "a standing ratio pages once");
}

fn unlanded(batch: &str, state: &str, age_s: u64) -> Unlanded {
    Unlanded { repo: "alpha".into(), batch: batch.into(), state: state.into(), age_s }
}

#[test]
fn a_round_that_passed_and_has_not_landed_in_five_minutes_pages_once_and_a_fresh_one_does_not() {
    let mut s = landed(400);
    s.unlanded = vec![unlanded("b-old", "GREEN", 300), unlanded("b-fresh", "LANDING", 299)];
    let w = Arc::new(Mutex::new(s.clone()));
    let mut r = rig(w.clone());
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("round b-old passed 300s ago and has not landed (still GREEN)") && !pages[0].contains("b-fresh"), "{pages:?}");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1, "paged once while it stands");
    s.unlanded = vec![unlanded("b-fresh", "LANDING", 301)];
    *w.lock().unwrap() = s;
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 2, "the fresh one is paged when it crosses five minutes");
}

#[test]
fn leftover_round_records_are_cleared_and_the_ones_refused_are_paged_by_name_once() {
    let mut s = landed(400);
    s.records = Records { clear: vec!["/q/a/rec-one".into(), "/q/a/rec-two".into()], held: vec!["/q/b/rec-three names round b-1, which is OPEN".into()] };
    let mut r = rig(Arc::new(Mutex::new(s)));
    assert_eq!(named(&r, CLEAR), [r#"["/q/a/rec-one","/q/a/rec-two"]"#]);
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("refused clearing: /q/b/rec-three names round b-1, which is OPEN"), "{pages:?}");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1, "a standing refusal pages once");
}

#[test]
fn no_leftover_records_means_no_clear_effect() {
    let r = rig(Arc::new(Mutex::new(landed(400))));
    assert!(named(&r, CLEAR).is_empty());
}

#[test]
fn green_runs_are_counted_from_each_rounds_passes_and_unlanded_means_green_or_landing_with_its_age() {
    use spira_runtime::world::{round_counts, unlanded_rounds};
    let b = |id: &str, state: &str, last: u64, verdicts: &[&str]| serde_json::json!({ "batch_id": id, "repo": "a", "state": state, "last_at": last, "passes": verdicts.iter().map(|v| serde_json::json!({ "verdict": v })).collect::<Vec<_>>() });
    let rows = [b("x", "GREEN", 9_000, &["red", "green"]), b("y", "LANDED", 9_500, &["green"]), b("z", "LANDING", 9_900, &["green"]), b("w", "OPEN", 9_900, &["red"])];
    assert_eq!(round_counts(&rows, 10_000, 7_200)[0].green_runs, 3);
    let got = unlanded_rounds(&rows, 10_000);
    assert_eq!(got, [Unlanded { repo: "a".into(), batch: "x".into(), state: "GREEN".into(), age_s: 1_000 }, Unlanded { repo: "a".into(), batch: "z".into(), state: "LANDING".into(), age_s: 100 }]);
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
    assert_eq!(got[0], RepoRounds { repo: "a".into(), cut: 4, abandoned: 2, landed: 1, abandoned_recent: 1, landed_recent: 1, green_runs: 0 });
    assert_eq!(got[1], RepoRounds { repo: "b".into(), cut: 1, abandoned: 0, landed: 1, abandoned_recent: 0, landed_recent: 0, green_runs: 0 });
}

/// A repository whose landing ref is `local/main` (the map row's `base`), with `n` landings
/// inside the day, one landing older than the day and one commit on another branch.
fn landing_repo(n: usize, now: u64) -> testkit::TempDir {
    let d = testkit::TempDir::new("throughput-landref");
    let p = d.path();
    let commit = |msg: &str, at: u64| {
        let date = format!("@{at} +0000");
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", msg])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    };
    testkit::git(p, &["init", "-q", "-b", "local/main"]);
    commit("spira: land sp-old — before the day", now - 2 * 86_400);
    for i in 0..n {
        commit(&format!("sp-w{i}: the work"), now - 1_800 - 3_600 * i as u64 - 60);
        commit(&format!("spira: land sp-w{i} — the work"), now - 1_800 - 3_600 * i as u64);
    }
    testkit::git(p, &["checkout", "-q", "-b", "elsewhere"]);
    commit("spira: land sp-never — not on the landing ref", now - 60);
    testkit::git(p, &["checkout", "-q", "local/main"]);
    d
}

#[test]
fn the_landing_count_reads_the_resolved_landing_ref_not_the_lane_name() {
    use spira_runtime::world::World;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let repo = landing_repo(3, now);
    let dir = testkit::TempDir::new("throughput-map");
    let map = dir.join("repos");
    // the lane column names the landing machine (queue.local), never a ref git can read
    std::fs::write(&map, format!("spira|{}|queue.local|local/main\n", repo.path().display())).unwrap();
    let w = World { repo_map: map, home_repo: "spira".into(), ..World::default() };
    let (hourly, landed) = w.landed_on_refs(now, "sp").expect("the landing ref is read");
    assert_eq!(landed, 3, "three beads landed on local/main inside the day");
    assert_eq!(hourly.iter().sum::<u64>(), 3, "each landing in one hour: {hourly:?}");
    assert_eq!((hourly[23], hourly[22], hourly[21]), (1, 1, 1), "half an hour ago, then each hour before: {hourly:?}");
}

#[test]
fn a_repo_whose_landing_ref_cannot_be_resolved_is_unreadable() {
    use spira_runtime::world::World;
    let dir = testkit::TempDir::new("throughput-nomap");
    let map = dir.join("repos");
    std::fs::write(&map, format!("spira|{}|queue.local|local/main\n", dir.join("not-a-repo").display())).unwrap();
    let w = World { repo_map: map, home_repo: "spira".into(), ..World::default() };
    assert!(w.landed_on_refs(1_000_000, "sp").is_err(), "no landing ref is a blind count, not zero");
}
