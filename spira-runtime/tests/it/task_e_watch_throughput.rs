//! Tests for `watch.throughput`: a landing stall files one express P0 incident and halts nothing,
//! and the per-repo "abandoned rounds, no landings" page.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, DAY_MS, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::{HALT_STREAM, SCHEDULER_HALTED};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_throughput::*;
use spira_runtime::watchers::INBOX_APPEND;

fn config() -> Config {
    Config { stall_s: 600, abandon_count: 5, abandon_window_s: 7_200, ratio_min_pct: 40, ratio_min_runs: 6, passed_grace_s: 300 }
}

/// When the fake host says the last landing happened, relative to the clock it is asked at.
#[derive(Clone, Copy)]
enum Last {
    Never,
    Ago(u64),
    FirstSight,
}

#[derive(Clone)]
struct Host {
    snap: Snapshot,
    last: Last,
    first_seen: Option<u64>,
}

fn landed(n: u64) -> Host {
    let mut hourly = vec![n / 24; 24];
    hourly[23] += n % 24;
    Host { snap: Snapshot { readable: true, landings: vec![RepoLanding { repo: "alpha".into(), hourly, last_landing: 0, waiting: 0 }], ..Default::default() }, last: Last::Ago(60), first_seen: None }
}

fn waiting(mut h: Host, n: u64, last: Last) -> Host {
    h.snap.landings[0].waiting = n;
    h.last = last;
    h
}

fn starved(repo: &str, abandoned: u64, landed_recent: u64) -> RepoRounds {
    RepoRounds { repo: repo.into(), cut: abandoned + landed_recent, abandoned, landed: landed_recent, abandoned_recent: abandoned, landed_recent, green_runs: 0 }
}

fn rig(world: Arc<Mutex<Host>>) -> TaskRig {
    let (_, stream, kind) = WatchThroughput::boot();
    let observe = move |input: &str| {
        let now = serde_json::from_str::<serde_json::Value>(input).unwrap()["now"].as_u64().unwrap();
        let mut h = world.lock().unwrap();
        let first = *h.first_seen.get_or_insert(now);
        let at = match h.last {
            Last::Never => 0,
            Last::Ago(s) => now - s,
            Last::FirstSight => first,
        };
        let mut snap = h.snap.clone();
        snap.landings.iter_mut().for_each(|l| l.last_landing = at);
        Ok(serde_json::to_string(&snap).unwrap())
    };
    let mut r = TaskRig::new(1, WatchThroughput::new(config()).registration()).unwrap().double(OBSERVE, observe).double(FILE, |_| Ok("filed".into())).double(CLEAR, |_| Ok("cleared".into())).double(INBOX_APPEND, |_| Ok("appended".into()));
    r.clock.advance_to(3 * DAY_MS);
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
fn the_row_is_registered_and_its_boot_arms_the_one_minute_tick() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(CADENCE_MS, 60_000);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
}

const CADENCE_S: u64 = CADENCE_MS / 1000;

fn episodes(r: &TaskRig) -> Vec<std::collections::BTreeMap<String, u64>> {
    r.journaled().iter().filter(|e| e.kind.tag == STATE).filter_map(|e| serde_json::from_str::<Carry>(&e.kind.body).ok()).map(|c| c.stalled).collect()
}

#[test]
fn fifty_five_landings_in_a_day_with_steady_landings_file_no_incident_and_halt_nothing() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 4, Last::Ago(60))));
    let mut r = rig(w);
    r.advance(12 * CADENCE_MS).unwrap();
    assert_eq!((halts(&r), named(&r, FILE).len(), named(&r, INBOX_APPEND).len()), (0, 0, 0));
    assert_silent_tick(&mut r, STREAM);
}

#[test]
fn no_landing_for_ten_minutes_with_beads_waiting_files_exactly_one_incident_and_halts_nothing() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 3, Last::FirstSight)));
    let mut r = rig(w);
    r.advance(9 * CADENCE_MS).unwrap();
    assert!(named(&r, FILE).is_empty(), "nine minutes is not yet a stall");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(600, 10 * CADENCE_S, "the tenth tick is the ten-minute mark");
    r.advance(10 * CADENCE_MS).unwrap();
    let filings = named(&r, FILE);
    assert_eq!(filings.len(), 1, "one incident for the whole episode: {filings:?}");
    let f: Filing = serde_json::from_str(&filings[0]).unwrap();
    assert!(f.title.contains("alpha") && f.title.contains("3 beads waiting"), "{}", f.title);
    assert!(f.body.contains("landings per hour") && f.body.contains("rounds over 24h"), "{}", f.body);
    assert_eq!(f.reference(), format!("incident:throughput-stall:alpha:{}", f.last_landing));
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("P0 alpha: landing stalled"), "{pages:?}");
    assert_eq!(halts(&r), 0, "a stall starts a repair, never a halt");
}

#[test]
fn a_stall_with_nothing_waiting_files_nothing() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 0, Last::Never)));
    let r = rig(w);
    assert_eq!((named(&r, FILE).len(), named(&r, INBOX_APPEND).len()), (0, 0));
}

#[test]
fn a_landing_clears_the_episode_and_the_next_stall_files_again() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 3, Last::Never)));
    let mut r = rig(w.clone());
    assert_eq!(named(&r, FILE).len(), 1);
    assert!(episodes(&r).last().is_some_and(|e| e.contains_key("alpha")), "the episode is recorded once filed");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, FILE).len(), 1, "a standing stall is filed once");
    w.lock().unwrap().last = Last::Ago(60);
    r.advance(CADENCE_MS).unwrap();
    assert!(episodes(&r).last().is_some_and(|e| e.is_empty()), "the landing clears the episode");
    w.lock().unwrap().last = Last::Never;
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, FILE).len(), 2, "a new stall is a new episode");
}

#[test]
fn a_filing_that_failed_is_asked_again_and_pages_nothing_until_it_succeeds() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 3, Last::Never)));
    let (_, stream, kind) = WatchThroughput::boot();
    let w2 = w.clone();
    let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let up2 = up.clone();
    let mut r = TaskRig::new(1, WatchThroughput::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&w2.lock().unwrap().snap).unwrap()))
        .double(FILE, move |_| if up2.load(std::sync::atomic::Ordering::SeqCst) { Ok("filed".into()) } else { Err("store down".into()) })
        .double(CLEAR, |_| Ok("cleared".into()))
        .double(INBOX_APPEND, |_| Ok("appended".into()));
    r.clock.advance_to(3 * DAY_MS);
    w.lock().unwrap().snap.landings[0].last_landing = 0;
    r.tick(stream, kind).unwrap();
    r.advance(CADENCE_MS).unwrap();
    assert!(named(&r, FILE).len() >= 2, "refiled on the next tick");
    assert!(named(&r, INBOX_APPEND).is_empty());
    assert!(episodes(&r).iter().all(|e| e.is_empty()));
    up.store(true, std::sync::atomic::Ordering::SeqCst);
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1, "paged once the filing succeeded");
    r.advance(3 * CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1);
}

#[test]
fn five_abandoned_rounds_and_no_landing_page_the_repo_by_name_without_stopping_the_world() {
    let mut s = landed(400);
    s.snap.repos = vec![starved("stuck-repo", 5, 0), starved("quiet", 4, 0), starved("busy", 9, 1)];
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
    s.snap.repos = vec![starved("stuck-repo", 5, 0)];
    let w = Arc::new(Mutex::new(s.clone()));
    let mut r = rig(w.clone());
    s.snap.repos = vec![starved("stuck-repo", 5, 1)];
    *w.lock().unwrap() = s.clone();
    r.advance(CADENCE_MS).unwrap();
    s.snap.repos = vec![starved("stuck-repo", 6, 0)];
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
    s.snap.repos = vec![repo_with_runs("diluted", 10, 3), repo_with_runs("healthy", 10, 4), repo_with_runs("thin", 5, 0)];
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
    s.snap.unlanded = vec![unlanded("b-old", "GREEN", 300), unlanded("b-fresh", "LANDING", 299)];
    let w = Arc::new(Mutex::new(s.clone()));
    let mut r = rig(w.clone());
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("round b-old passed 300s ago and has not landed (still GREEN)") && !pages[0].contains("b-fresh"), "{pages:?}");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1, "paged once while it stands");
    s.snap.unlanded = vec![unlanded("b-fresh", "LANDING", 301)];
    *w.lock().unwrap() = s;
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 2, "the fresh one is paged when it crosses five minutes");
}

#[test]
fn leftover_round_records_are_cleared_and_the_ones_refused_are_paged_by_name_once() {
    let mut s = landed(400);
    s.snap.records = Records { clear: vec!["/q/a/rec-one".into(), "/q/a/rec-two".into()], held: vec!["/q/b/rec-three names round b-1, which is OPEN".into()] };
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
    let got = w.landed_on_refs(now, "sp").expect("the landing ref is read");
    assert_eq!(got.len(), 1);
    let (hourly, repo, last) = (&got[0].hourly, &got[0].repo, got[0].last_landing);
    assert_eq!((repo.as_str(), last), ("spira", now - 1_800), "the last landing on local/main, not on the other branch");
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

fn registration() -> runtime::event::Registration {
    WatchThroughput::new(config()).registration()
}

/// A landing stall filed and a starved repo said, both still standing.
fn filed_and_said() -> TaskRig {
    let mut host = waiting(landed(48), 3, Last::Ago(8_000));
    host.snap.repos = vec![starved("alpha", 5, 0)];
    let r = rig(Arc::new(Mutex::new(host)));
    assert_eq!(named(&r, FILE).len(), 1);
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&filed_and_said(), NAME);
    assert_eq!(rows, ["blind", "diluted", "halted", "held", "resumed_at", "retried", "stalled/alpha", "starved", "unlanded"]);
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(filed_and_said, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}

#[test]
fn the_last_landing_is_when_the_round_landed_not_when_it_was_cut() {
    use spira_runtime::world::stamp_landings;
    let cut = 1_000_000u64;
    let mut landings = vec![RepoLanding { repo: "a".into(), last_landing: cut, ..Default::default() }, RepoLanding { repo: "b".into(), last_landing: 500, ..Default::default() }];
    let rows = [serde_json::json!({ "repo": "a", "state": "LANDED", "last_at": cut + 2_400 }), serde_json::json!({ "repo": "a", "state": "ABANDONED", "last_at": cut + 9_000 })];
    stamp_landings(&mut landings, &rows);
    assert_eq!(landings[0].last_landing, cut + 2_400);
    let now_ms = (cut + 2_400 + 7_000) * 1000;
    let cfg = Config { stall_s: 3_600, abandon_count: 3, abandon_window_s: 7_200, ratio_min_pct: 10, ratio_min_runs: 10, passed_grace_s: 3_600 };
    let snap = Snapshot { readable: true, landings: vec![RepoLanding { waiting: 2, ..landings[0].clone() }], ..Default::default() };
    let v = decide(&Carry::default(), &snap, now_ms, &cfg);
    let filed: Filing = serde_json::from_str(&v.acts.iter().find(|a| a.name == FILE).expect("a stall is filed").input).unwrap();
    assert!(filed.line.contains("(7000s ago)"), "{}", filed.line);
    assert_eq!(filed.reference(), format!("incident:throughput-stall:a:{}", cut + 2_400));
    assert_eq!(landings[1].last_landing, 500, "a repo with no landed round keeps the ref's time");
}

fn stalled_snapshot(now_s: u64) -> Snapshot {
    Snapshot { readable: true, landings: vec![RepoLanding { repo: "alpha".into(), hourly: vec![0; 24], last_landing: now_s - 10_000, waiting: 3 }], ..Default::default() }
}

fn halt(since: u64) -> Option<spira_runtime::watchers::halt::Halt> {
    Some(spira_runtime::watchers::halt::Halt { since, why: "operator order: freeze".into() })
}

#[test]
fn a_landing_stall_while_the_work_plane_is_halted_files_nothing_and_the_row_names_the_halt() {
    let now = 3 * DAY_MS;
    let snap = Snapshot { halt: halt(now / 1000 - 600), ..stalled_snapshot(now / 1000) };
    let v = decide(&Carry::default(), &snap, now, &config());
    assert!(v.acts.is_empty(), "{:?}", v.acts.iter().map(|a| &a.name).collect::<Vec<_>>());
    assert!(v.carry.halted.contains("operator order: freeze") && v.carry.halted.contains("600s"), "{}", v.carry.halted);
}

#[test]
fn the_same_landing_stall_with_the_plane_running_files_the_incident() {
    let now = 3 * DAY_MS;
    let v = decide(&Carry::default(), &stalled_snapshot(now / 1000), now, &config());
    assert_eq!(v.acts.iter().filter(|a| a.name == FILE).count(), 1);
    assert!(v.carry.halted.is_empty());
}

#[test]
fn after_a_halt_lifts_the_stall_clock_restarts() {
    let now = 3 * DAY_MS;
    let held = decide(&Carry::default(), &Snapshot { halt: halt(now / 1000 - 600), ..stalled_snapshot(now / 1000) }, now, &config());
    let v = decide(&held.carry, &stalled_snapshot((now + 60_000) / 1000), now + 60_000, &config());
    assert!(v.acts.is_empty(), "no page inside one stall bound of the resume");
    let later = now + 7_300_000;
    let v = decide(&held.carry, &stalled_snapshot(later / 1000), later, &config());
    assert_eq!(v.acts.iter().filter(|a| a.name == FILE).count(), 1);
}

fn blind_after_activation(host: Arc<Mutex<Host>>) -> TaskRig {
    let (_, stream, kind) = WatchThroughput::boot();
    let observe = move |input: &str| {
        let now = serde_json::from_str::<serde_json::Value>(input).unwrap()["now"].as_u64().unwrap();
        let h = host.lock().unwrap();
        let mut snap = h.snap.clone();
        snap.activated_at = Some(now - 5);
        Ok(serde_json::to_string(&snap).unwrap())
    };
    let mut r = TaskRig::new(1, WatchThroughput::new(config()).registration()).unwrap().double(OBSERVE, observe).double(FILE, |_| Ok("filed".into())).double(CLEAR, |_| Ok("cleared".into())).double(INBOX_APPEND, |_| Ok("appended".into()));
    r.clock.advance_to(3 * DAY_MS);
    r.tick(stream, kind).unwrap();
    r
}

#[test]
fn a_landings_read_that_fails_just_after_an_activation_is_retried_and_pages_nothing_if_the_next_reads() {
    let host = Arc::new(Mutex::new(Host { snap: Snapshot::default(), ..landed(24) }));
    let mut r = blind_after_activation(host.clone());
    assert_eq!(named(&r, INBOX_APPEND), Vec::<String>::new(), "the first unreadable tick after an activation says nothing");
    host.lock().unwrap().snap.readable = true;
    r.advance(60_000).unwrap();
    assert_eq!(named(&r, INBOX_APPEND), Vec::<String>::new(), "the next tick reads");
}

#[test]
fn a_landings_read_that_stays_unreadable_for_two_ticks_after_an_activation_pages_blind_naming_it() {
    let host = Arc::new(Mutex::new(Host { snap: Snapshot::default(), ..landed(24) }));
    let mut r = blind_after_activation(host);
    r.advance(60_000).unwrap();
    let pages = named(&r, INBOX_APPEND);
    assert_eq!(pages.len(), 1, "{pages:?}");
    assert!(pages[0].contains("P0 BLIND") && pages[0].contains("after the activation"), "{pages:?}");
}
