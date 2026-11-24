//! Tests for `watch.throughput`: a landing stall files one express P0 incident and halts nothing,
//! and the per-repo "abandoned rounds, no landings" page.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, DAY_MS, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::{HALT_STREAM, SCHEDULER_HALTED};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_throughput::*;
use spira_runtime::watchers::INBOX_APPEND;

fn config() -> Config {
    Config { stall_s: 600, abandon_count: 5, abandon_window_s: 7_200, ratio_min_pct: 40, ratio_min_runs: 6, passed_grace_s: 300, activation_bound_s: 75 }
}

/// When the fake host says the last landing happened, relative to the clock it is asked at.
#[derive(Clone, Copy)]
enum Last {
    Never,
    Undated,
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
    Host { snap: Snapshot { readable: true, landings: vec![RepoLanding { repo: "alpha".into(), hourly, last_landing: 0, waiting: 0, oldest: None, undated: None, beads: vec![] }], ..Default::default() }, last: Last::Ago(60), first_seen: None }
}

fn waiting(mut h: Host, n: u64, last: Last) -> Host {
    h.snap.landings[0].waiting = n;
    h.snap.landings[0].oldest = (n > 0).then(|| Waiting { bead: "sp-x".into(), since: Some(1) });
    h.last = last;
    h
}

fn starved(repo: &str, abandoned: u64, landed_recent: u64) -> RepoRounds {
    RepoRounds { repo: repo.into(), cut: abandoned + landed_recent, abandoned, landed: landed_recent, abandoned_recent: abandoned, landed_recent, green_runs: 0 }
}

fn rig(world: Arc<Mutex<Host>>) -> TaskRig {
    let (_, stream, kind) = WatchThroughput::boot();
    let mut r = rig_unticked(world);
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn rig_unticked(world: Arc<Mutex<Host>>) -> TaskRig {
    let observe = move |input: &str| {
        let now = serde_json::from_str::<serde_json::Value>(input).unwrap()["now"].as_u64().unwrap();
        let mut h = world.lock().unwrap();
        let first = *h.first_seen.get_or_insert(now);
        let at = match h.last {
            Last::Never | Last::Undated => 0,
            Last::Ago(s) => now - s,
            Last::FirstSight => first,
        };
        let mut snap = h.snap.clone();
        snap.landings.iter_mut().for_each(|l| {
            l.last_landing = at;
            if matches!(h.last, Last::Undated) {
                l.undated = l.oldest.take().map(|o| o.bead);
            }
            if let Some(o) = &mut l.oldest {
                o.since = Some(match h.last {
                    Last::Never => now - 100_000,
                    _ => at,
                });
            }
        });
        Ok(serde_json::to_string(&snap).unwrap())
    };
    let r = TaskRig::new(1, WatchThroughput::new(config()).registration()).unwrap().double(OBSERVE, observe).double(FILE, |_| Ok("filed".into())).double(CLEAR, |_| Ok("cleared".into())).double(INBOX_APPEND, |_| Ok("appended".into()));
    r.clock.advance_to(3 * DAY_MS);
    r
}

fn named(r: &TaskRig, effect: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == effect).map(|q| q.input.clone()).collect()
}

fn halts(r: &TaskRig) -> usize {
    r.journaled().iter().filter(|e| e.stream.0 == HALT_STREAM && e.kind.tag == SCHEDULER_HALTED).count()
}

#[test_id(tqapu1m)]
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

#[test_id(tmmgt3k)]
#[test]
fn fifty_five_landings_in_a_day_with_steady_landings_file_no_incident_and_halt_nothing() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 4, Last::Ago(60))));
    let mut r = rig(w);
    r.advance(12 * CADENCE_MS).unwrap();
    assert_eq!((halts(&r), named(&r, FILE).len(), named(&r, INBOX_APPEND).len()), (0, 0, 0));
    assert_silent_tick(&mut r, STREAM);
}

#[test_id(tsthgw8)]
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

#[test_id(t0t0f8v)]
#[test]
fn a_waiting_bead_with_no_entry_time_files_no_stall_and_pages_the_data_defect_once() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 3, Last::Undated)));
    let mut r = rig(w);
    r.advance(5 * CADENCE_MS).unwrap();
    assert!(named(&r, FILE).is_empty(), "an unknown wait is not a stall");
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("data defect") && pages[0].contains("sp-x"), "{pages:?}");
    assert!(pages[0].contains("] P2 ") && !pages[0].contains("P0") && pages[0].contains("`since`"), "{pages:?}");
}

#[test_id(t0ssrfa)]
#[test]
fn a_stall_with_nothing_waiting_files_nothing() {
    let w = Arc::new(Mutex::new(waiting(landed(55), 0, Last::Never)));
    let r = rig(w);
    assert_eq!((named(&r, FILE).len(), named(&r, INBOX_APPEND).len()), (0, 0));
}

#[test_id(ty8r6dv)]
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

#[test_id(tqhw6sl)]
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

#[test_id(tk3wnjq)]
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

#[test_id(t55nlmn)]
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

#[test_id(tfj4ohf)]
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

#[test_id(toecaby)]
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

#[test_id(t7dmlgx)]
#[test]
fn leftover_round_records_are_cleared_and_the_ones_refused_are_paged_by_name_once() {
    let mut s = landed(400);
    s.snap.records = Records { clear: vec!["q/a/rec-one".into(), "q/a/rec-two".into()], held: vec!["q/b/rec-three names round b-1, which is OPEN".into()] };
    let mut r = rig(Arc::new(Mutex::new(s)));
    assert_eq!(named(&r, CLEAR), [r#"["q/a/rec-one","q/a/rec-two"]"#]);
    let pages = named(&r, INBOX_APPEND);
    assert!(pages.len() == 1 && pages[0].contains("refused clearing: q/b/rec-three names round b-1, which is OPEN"), "{pages:?}");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(named(&r, INBOX_APPEND).len(), 1, "a standing refusal pages once");
}

#[test_id(t1fzog6)]
#[test]
fn a_snapshot_whose_records_are_all_live_clears_and_pages_nothing() {
    let mut s = landed(400);
    s.snap.records = Records { clear: vec![], held: vec![] };
    let r = rig(Arc::new(Mutex::new(s)));
    assert!(named(&r, CLEAR).is_empty());
    assert!(named(&r, INBOX_APPEND).is_empty());
}

#[test_id(tci5cqy)]
#[test]
fn no_leftover_records_means_no_clear_effect() {
    let r = rig(Arc::new(Mutex::new(landed(400))));
    assert!(named(&r, CLEAR).is_empty());
}

#[test_id(ta2tgkp)]
#[test]
fn green_runs_are_counted_from_each_rounds_passes_and_unlanded_means_green_or_landing_with_its_age() {
    use spira_runtime::world::{round_counts, unlanded_rounds};
    let b = |id: &str, state: &str, last: u64, verdicts: &[&str]| serde_json::json!({ "batch_id": id, "repo": "a", "state": state, "last_at": last, "passes": verdicts.iter().map(|v| serde_json::json!({ "verdict": v })).collect::<Vec<_>>() });
    let rows = [b("x", "GREEN", 9_000, &["red", "green"]), b("y", "LANDED", 9_500, &["green"]), b("z", "LANDING", 9_900, &["green"]), b("w", "OPEN", 9_900, &["red"])];
    assert_eq!(round_counts(&rows, 10_000, 7_200)[0].green_runs, 3);
    let got = unlanded_rounds(&rows, 10_000);
    assert_eq!(got, [Unlanded { repo: "a".into(), batch: "x".into(), state: "GREEN".into(), age_s: 1_000 }, Unlanded { repo: "a".into(), batch: "z".into(), state: "LANDING".into(), age_s: 100 }]);
}

#[test_id(tqyf5c7)]
#[test]
fn every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_throughput.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test_id(tt7nic9)]
#[test]
fn two_days_replay_identically_inside_the_budget() {
    let r = assert_deterministic(|_| rig(Arc::new(Mutex::new(landed(300)))), 7, 2);
    acc::assert_healthy(&r, STREAM);
    assert_journal_budget(&r, 1);
}

#[test_id(tsp93bm)]
#[test]
fn bead_ids_are_the_prefixed_words_in_commit_subjects_and_nothing_near_them() {
    use spira_runtime::world::bead_ids;
    let found = bead_ids("spira: land sp-ipwlic — Every watcher\nsp-nxsurz: round — x\nsp-ipwlic: again\nspira-lc: not a bead\nsp-: bare\nresp-abc no", "sp");
    assert_eq!(found.into_iter().collect::<Vec<_>>(), ["sp-ipwlic", "sp-nxsurz"]);
    assert!(bead_ids("sp-ipwlic: x", "zz").is_empty(), "a configured prefix, not a literal");
}

#[test_id(tl014f2)]
#[test]
fn round_counts_split_a_repos_rounds_by_how_they_ended_and_how_recently() {
    use spira_runtime::world::round_counts;
    let b = |repo: &str, state: &str, last: u64| serde_json::json!({ "repo": repo, "state": state, "opened_at": 1, "last_at": last });
    let rows = [b("a", "ABANDONED", 9_900), b("a", "ABANDONED", 1_000), b("a", "LANDED", 9_000), b("a", "CI_RUNNING", 9_990), b("b", "SETTLED", 100)];
    let got = round_counts(&rows, 10_000, 7_200);
    assert_eq!(got[0], RepoRounds { repo: "a".into(), cut: 4, abandoned: 2, landed: 1, abandoned_recent: 1, landed_recent: 1, green_runs: 0 });
    assert_eq!(got[1], RepoRounds { repo: "b".into(), cut: 1, abandoned: 0, landed: 1, abandoned_recent: 0, landed_recent: 0, green_runs: 0 });
}

/// An empty commit whose author and committer dates are `at`, whatever the ambient dates are.
fn commit_at(p: &std::path::Path, msg: &str, at: u64) {
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
}

/// A repository whose landing ref is `local/main` (the map row's `base`), with `n` landings
/// inside the day, one landing older than the day and one commit on another branch.
fn landing_repo(n: usize, now: u64) -> testkit::TempDir {
    let d = testkit::TempDir::new("throughput-landref");
    let p = d.path();
    let commit = |msg: &str, at: u64| commit_at(p, msg, at);
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

#[test_id(tbt7ekx)]
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

#[test_id(toenbo7)]
#[test]
fn a_repo_whose_landing_ref_cannot_be_resolved_is_unreadable() {
    use spira_runtime::world::World;
    let dir = testkit::TempDir::new("throughput-nomap");
    let map = dir.join("repos");
    std::fs::write(&map, format!("spira|{}|queue.local|local/main\n", dir.join("not-a-repo").display())).unwrap();
    let w = World { repo_map: map, home_repo: "spira".into(), ..World::default() };
    assert!(w.landed_on_refs(1_000_000, "sp").is_err(), "no landing ref is a blind count, not zero");
}

#[test_id(te0oe00)]
#[test]
fn a_missing_landing_ref_names_the_repo_the_ref_tried_and_gits_status() {
    use spira_runtime::world::World;
    let dir = testkit::TempDir::new("throughput-noref");
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    testkit::git(&repo, &["init", "-q", "-b", "other"]);
    commit_at(&repo, "one", 1_700_000_000);
    let map = dir.join("repos");
    std::fs::write(&map, format!("spira|{}|queue.local|local/main||\n", repo.display())).unwrap();
    let w = World { repo_map: map, home_repo: "spira".into(), ..World::default() };
    let e = w.landed_on_refs(1_700_000_100, "sp").unwrap_err();
    assert!(e.starts_with("spira: no landing ref resolves: ") && e.contains("local/main") && e.contains("exit 1"), "{e}");
}

/// A `git` that behaves as git does, except that the one call of a repo whose `.git/flaky`
/// holds a pattern and matches it fails once, after running `.git/flaky-fix`.
fn flaky_git_bin(dir: &std::path::Path) -> std::path::PathBuf {
    let real = std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|d| d.join("git")).find(|g| g.is_file()).expect("a git on PATH");
    let bin = dir.join("flaky-git-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = -C ] && [ -f \"$2/.git/flaky\" ] && [ ! -e \"$2/.git/flaky-fired\" ]; then\nread pat < \"$2/.git/flaky\"\ncase \" $* \" in *\"$pat\"*) : > \"$2/.git/flaky-fired\"; [ -f \"$2/.git/flaky-fix\" ] && {real} -C \"$2\" branch local/main; exit 1;; esac\nfi\nexec {real} \"$@\"\n",
        real = real.display()
    );
    testkit::write_exe(bin.join("git"), &script);
    bin
}

/// A repo with one landed bead whose next git call matching `pat` fails once; `fix` makes the
/// failed call's ref appear behind it.
fn flaky_git_repo(name: &str, pat: &str, fix: bool, now: u64) -> (testkit::TempDir, spira_runtime::world::World) {
    let dir = testkit::TempDir::new(name);
    let bin = flaky_git_bin(&dir);
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    testkit::git(&repo, &["init", "-q", "-b", "other"]);
    commit_at(&repo, "sp-aaaaaa landed", now - 60);
    std::fs::write(repo.join(".git/flaky"), format!("{pat}\n")).unwrap();
    if fix {
        std::fs::write(repo.join(".git/flaky-fix"), "").unwrap();
    }
    let map = dir.join("repos");
    std::fs::write(&map, format!("spira|{}|queue.local|local/main||\n", repo.display())).unwrap();
    let w = spira_runtime::world::World { repo_map: map, home_repo: "spira".into(), git_bin: Some(bin.join("git")), ..Default::default() };
    (dir, w)
}

#[test_id(tt5aqm0)]
#[test]
fn a_landing_ref_that_fails_once_and_resolves_on_the_second_read_is_read() {
    let now = 1_700_000_000u64;
    let (dir, w) = flaky_git_repo("throughput-reread", "rev-parse --verify -q local/main", true, now);
    let landings = w.landed_on_refs(now, "sp").expect("the second read resolves the ref");
    assert!(dir.join("repo/.git/flaky-fired").exists(), "the first read did fail");
    assert_eq!(landings[0].hourly.iter().sum::<u64>(), 1);
}

#[test_id(trby1x3)]
#[test]
fn a_git_log_that_fails_once_is_read_again() {
    let now = 1_700_000_000u64;
    let (dir, w) = flaky_git_repo("throughput-logreread", " log local/main", false, now);
    testkit::git(&dir.join("repo"), &["branch", "local/main"]);
    let landings = w.landed_on_refs(now, "sp").expect("the second log read answers");
    assert!(dir.join("repo/.git/flaky-fired").exists(), "the first log read did fail");
    assert_eq!(landings[0].hourly.iter().sum::<u64>(), 1);
}

#[test_id(thqgsxz)]
#[test]
fn an_unreadable_landing_ref_names_gits_own_exit_and_stderr() {
    use spira_runtime::world::World;
    let dir = testkit::TempDir::new("throughput-gitwords");
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    testkit::git(&repo, &["init", "-q", "-b", "other"]);
    let now = 1_700_000_000u64;
    commit_at(&repo, "one", now - 120);
    commit_at(&repo, "two", now - 60);
    testkit::git(&repo, &["branch", "local/main"]);
    // landref only checks that the tip resolves, so a missing parent passes it and fails the log walk
    let parent = testkit::git(&repo, &["rev-parse", "HEAD~1"]);
    std::fs::remove_file(repo.join(format!(".git/objects/{}/{}", &parent[..2], &parent[2..]))).unwrap();
    let map = dir.join("repos");
    std::fs::write(&map, format!("spira|{}|queue.local|local/main\n", repo.display())).unwrap();
    let w = World { repo_map: map, home_repo: "spira".into(), ..World::default() };
    let e = w.landed_on_refs(now, "sp").unwrap_err();
    assert!(e.contains("git log other") && e.contains("exit 128") && e.contains("fatal: Failed to traverse") && e.contains(&repo.display().to_string()), "{e}");
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

#[test_id(tve51q7)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&filed_and_said(), NAME);
    assert_eq!(rows, ["blind", "blind_since_ms", "diluted", "halted", "held", "orphan_paged", "resumed_at", "stalled/alpha", "starved", "undated", "unlanded"]);
}

#[test_id(t94hd7b)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(filed_and_said, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}

#[test_id(to9lv4p)]
#[test]
fn the_last_landing_is_when_the_round_landed_not_when_it_was_cut() {
    use spira_runtime::world::stamp_landings;
    let cut = 1_000_000u64;
    let mut landings = vec![RepoLanding { repo: "a".into(), last_landing: cut, ..Default::default() }, RepoLanding { repo: "b".into(), last_landing: 500, ..Default::default() }];
    let rows = [serde_json::json!({ "repo": "a", "state": "LANDED", "last_at": cut + 2_400 }), serde_json::json!({ "repo": "a", "state": "ABANDONED", "last_at": cut + 9_000 })];
    stamp_landings(&mut landings, &rows);
    assert_eq!(landings[0].last_landing, cut + 2_400);
    let now_ms = (cut + 2_400 + 7_000) * 1000;
    let cfg = Config { stall_s: 3_600, abandon_count: 3, abandon_window_s: 7_200, ratio_min_pct: 10, ratio_min_runs: 10, passed_grace_s: 3_600, activation_bound_s: 75 };
    let snap = Snapshot { readable: true, landings: vec![RepoLanding { waiting: 2, oldest: Some(Waiting { bead: "sp-x".into(), since: Some(cut) }), ..landings[0].clone() }], ..Default::default() };
    let v = decide(&Carry::default(), &snap, now_ms, &cfg);
    let filed: Filing = serde_json::from_str(&v.acts.iter().find(|a| a.name == FILE).expect("a stall is filed").input).unwrap();
    assert!(filed.line.contains("(7000s ago)"), "{}", filed.line);
    assert_eq!(filed.reference(), format!("incident:throughput-stall:a:{}", cut + 2_400));
    assert_eq!(landings[1].last_landing, 500, "a repo with no landed round keeps the ref's time");
}

fn stalled_snapshot(now_s: u64) -> Snapshot {
    Snapshot { readable: true, landings: vec![RepoLanding { repo: "alpha".into(), hourly: vec![0; 24], last_landing: now_s - 10_000, waiting: 3, oldest: Some(Waiting { bead: "sp-x".into(), since: Some(now_s - 10_000) }), undated: None, beads: vec![] }], ..Default::default() }
}

fn halt(since: u64) -> Option<spira_runtime::watchers::halt::Halt> {
    Some(spira_runtime::watchers::halt::Halt { since, why: "operator order: freeze".into() })
}

#[test_id(torrxyk)]
#[test]
fn a_landing_stall_while_the_work_plane_is_halted_files_nothing_and_the_row_names_the_halt() {
    let now = 3 * DAY_MS;
    let snap = Snapshot { halt: halt(now / 1000 - 600), ..stalled_snapshot(now / 1000) };
    let v = decide(&Carry::default(), &snap, now, &config());
    assert!(v.acts.is_empty(), "{:?}", v.acts.iter().map(|a| &a.name).collect::<Vec<_>>());
    assert!(v.carry.halted.contains("operator order: freeze") && v.carry.halted.contains("600s"), "{}", v.carry.halted);
}

#[test_id(tm5dwzt)]
#[test]
fn the_same_landing_stall_with_the_plane_running_files_the_incident() {
    let now = 3 * DAY_MS;
    let v = decide(&Carry::default(), &stalled_snapshot(now / 1000), now, &config());
    assert_eq!(v.acts.iter().filter(|a| a.name == FILE).count(), 1);
    assert!(v.carry.halted.is_empty());
}

#[test_id(t8iv477)]
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

#[test_id(tymwgua)]
#[test]
fn the_first_unreadable_landings_tick_just_after_an_activation_pages_nothing() {
    let host = Arc::new(Mutex::new(Host { snap: Snapshot { why: REFUSAL.into(), ..Default::default() }, ..landed(24) }));
    let r = blind_after_activation(host);
    assert!(named(&r, INBOX_APPEND).is_empty(), "{:?}", named(&r, INBOX_APPEND));
}

fn blind_pages(r: &TaskRig) -> usize {
    named(r, INBOX_APPEND).iter().filter(|t| t.contains("P0 BLIND")).count()
}

fn clears(r: &TaskRig) -> usize {
    named(r, INBOX_APPEND).iter().filter(|t| t.contains("CLEAR: the landings are read again")).count()
}

#[test_id(t657hor)]
#[test]
fn a_refused_read_followed_by_a_good_one_inside_the_stall_bound_says_nothing() {
    let host = Arc::new(Mutex::new(Host { snap: Snapshot::default(), ..landed(24) }));
    let mut r = blind_after_activation(host.clone());
    r.advance(60_000).unwrap();
    host.lock().unwrap().snap.readable = true;
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    assert!(named(&r, INBOX_APPEND).is_empty(), "{:?}", named(&r, INBOX_APPEND));
}

#[test_id(tznn0yq)]
#[test]
fn landings_unreadable_past_the_stall_bound_page_blind_once_and_a_good_read_says_clear_once() {
    let host = Arc::new(Mutex::new(Host { snap: Snapshot::default(), ..landed(24) }));
    let mut r = blind_after_activation(host.clone());
    for _ in 0..9 {
        r.advance(60_000).unwrap();
    }
    assert_eq!(blind_pages(&r), 0, "silent through 600 s: {:?}", named(&r, INBOX_APPEND));
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    assert_eq!(blind_pages(&r), 1, "{:?}", named(&r, INBOX_APPEND));
    for _ in 0..3 {
        r.advance(60_000).unwrap();
    }
    assert_eq!(blind_pages(&r), 1, "no repeat: {:?}", named(&r, INBOX_APPEND));
    host.lock().unwrap().snap.readable = true;
    r.advance(60_000).unwrap();
    r.advance(60_000).unwrap();
    assert_eq!(clears(&r), 1, "{:?}", named(&r, INBOX_APPEND));
    assert_eq!(blind_pages(&r), 1);
}

const REFUSAL: &str = "store unavailable: store not running: nothing was sent; waited 50 s for run/lc.sock";

#[test_id(t667tdb)]
#[test]
fn a_blind_page_carries_the_stores_reason_and_says_nothing_of_restarts_or_activations() {
    let snap = Snapshot { why: REFUSAL.into(), activated_at: Some(1_794_686_201), ..Default::default() };
    let carry = Carry { blind_since_ms: Some(1_794_686_100_000), ..Default::default() };
    let v = decide(&carry, &snap, 1_794_686_801_000, &config());
    let lines: Vec<String> = v.acts.iter().filter(|a| a.name == INBOX_APPEND).map(|a| a.input.clone()).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains(&format!("({REFUSAL})")) && lines[0].contains("P0 BLIND"), "{}", lines[0]);
    assert!(!lines[0].contains("restart") && !lines[0].contains("activation"), "{}", lines[0]);
}

fn one_waiting(now_s: u64, last_landing_ago: u64, bead: &str, waited: u64) -> Snapshot {
    let oldest = Some(Waiting { bead: bead.into(), since: Some(now_s - waited) });
    Snapshot { readable: true, landings: vec![RepoLanding { repo: "alpha".into(), hourly: vec![0; 24], last_landing: now_s - last_landing_ago, waiting: 1, oldest, undated: None, beads: vec![] }], ..Default::default() }
}

#[test_id(tpez4x2)]
#[test]
fn a_bead_that_arrived_a_minute_ago_on_a_repo_idle_for_hours_files_and_pages_nothing() {
    let now = 3 * DAY_MS;
    let v = decide(&Carry::default(), &one_waiting(now / 1000, 14 * 3600, "sp-new", 60), now, &config());
    assert!(!v.acts.iter().any(|a| a.name == FILE || a.name == INBOX_APPEND), "{:?}", v.acts.iter().map(|a| &a.name).collect::<Vec<_>>());
}

#[test_id(tnqkov2)]
#[test]
fn a_stall_names_the_oldest_waiting_bead_and_how_long_it_has_waited() {
    let now = 3 * DAY_MS;
    let v = decide(&Carry::default(), &one_waiting(now / 1000, 14 * 3600, "sp-old", 700), now, &config());
    let files: Vec<_> = v.acts.iter().filter(|a| a.name == FILE).collect();
    assert_eq!(files.len(), 1);
    let f: Filing = serde_json::from_str(&files[0].input).unwrap();
    assert!(f.title.contains("sp-old") && f.title.contains("700s"), "{}", f.title);
    assert!(f.line.contains("sp-old") && f.line.contains("700s") && f.line.starts_with("P0 alpha: landing stalled"), "{}", f.line);
    assert!(f.body.contains("CERTIFIED"), "{}", f.body);
}

#[test_id(tlxilwb)]
#[test]
fn a_long_waiting_bead_with_a_landing_five_minutes_ago_is_silent() {
    let now = 3 * DAY_MS;
    let v = decide(&Carry::default(), &one_waiting(now / 1000, 300, "sp-old", 7_200), now, &config());
    assert!(!v.acts.iter().any(|a| a.name == FILE || a.name == INBOX_APPEND));
}

#[test_id(tnuva7o)]
#[test]
fn stamp_waiting_counts_all_three_states_and_picks_the_oldest_dated_with_a_null_since_undated() {
    use spira_runtime::world::stamp_waiting;
    let row = |bead: &str, repo: &str, state: &str, since: serde_json::Value| serde_json::json!({ "bead_id": bead, "repo": repo, "state": state, "since": since });
    let mut landings = vec![RepoLanding { repo: "a".into(), ..Default::default() }, RepoLanding { repo: "b".into(), ..Default::default() }];
    let rows = [row("sp-1", "a", "SUBMITTED", 900.into()), row("sp-2", "a", "CERTIFIED", 400.into()), row("sp-3", "a", "IN_DELIVERY", 700.into()), row("sp-4", "b", "SUBMITTED", 100.into())];
    stamp_waiting(&mut landings, &rows);
    assert_eq!(landings[0].waiting, 3);
    assert_eq!(landings[0].oldest, Some(Waiting { bead: "sp-2".into(), since: Some(400) }));
    assert_eq!((landings[1].waiting, landings[1].oldest.clone()), (1, Some(Waiting { bead: "sp-4".into(), since: Some(100) })));
    let rows = [row("sp-1", "a", "SUBMITTED", 900.into()), row("sp-5", "a", "SUBMITTED", serde_json::Value::Null)];
    stamp_waiting(&mut landings, &rows);
    assert_eq!(landings[0].undated.as_deref(), Some("sp-5"));
    assert_eq!(landings[0].oldest, Some(Waiting { bead: "sp-1".into(), since: Some(900) }));
    assert_eq!((landings[1].waiting, landings[1].oldest.clone(), landings[1].undated.clone()), (0, None, None));
}

fn printed_row(bead: &str, since: u64) -> serde_json::Value {
    serde_json::json!({ "bead_id": bead, "repo": "alpha", "state": "SUBMITTED", "since": since.to_string(), "updated_at": (since + 77).to_string(), "lease_until": null })
}

#[test_id(t1i2pdk)]
#[test]
fn rows_as_spira_lc_prints_them_read_their_since_from_the_digit_string() {
    use spira_runtime::world::stamp_waiting;
    let mut landings = vec![RepoLanding { repo: "alpha".into(), ..Default::default() }];
    let rows = vec![printed_row("sp-1", 900), printed_row("sp-2", 400), printed_row("sp-3", 700)];
    stamp_waiting(&mut landings, &rows);
    assert_eq!(landings[0].oldest, Some(Waiting { bead: "sp-2".into(), since: Some(400) }));
    assert_eq!(landings[0].undated, None);
}

#[test_id(t3inslb)]
#[test]
fn a_bead_submitted_five_seconds_ago_as_the_store_prints_it_pages_and_files_nothing() {
    use spira_runtime::world::stamp_waiting;
    let now = 3 * DAY_MS;
    let mut landings = vec![RepoLanding { repo: "alpha".into(), hourly: vec![0; 24], last_landing: now / 1000 - 14 * 3600, ..Default::default() }];
    stamp_waiting(&mut landings, &[printed_row("sp-new", now / 1000 - 5)]);
    let v = decide(&Carry::default(), &Snapshot { readable: true, landings, ..Default::default() }, now, &config());
    assert!(!v.acts.iter().any(|a| a.name == FILE || a.name == INBOX_APPEND), "{:?}", v.acts);
}

#[test_id(t2bz6nc)]
#[test]
fn a_string_since_older_than_the_stall_threshold_files_exactly_one_stall() {
    use spira_runtime::world::stamp_waiting;
    let now = 3 * DAY_MS;
    let mut landings = vec![RepoLanding { repo: "alpha".into(), hourly: vec![0; 24], last_landing: now / 1000 - 14 * 3600, ..Default::default() }];
    stamp_waiting(&mut landings, &[printed_row("sp-old", now / 1000 - 7_200)]);
    let v = decide(&Carry::default(), &Snapshot { readable: true, landings, ..Default::default() }, now, &config());
    assert_eq!(v.acts.iter().filter(|a| a.name == FILE).count(), 1, "{:?}", v.acts);
}

#[test_id(tvg4vwj)]
#[test]
fn a_null_since_beside_a_stalled_dated_row_pages_p2_naming_it_and_still_files_the_stall() {
    use spira_runtime::world::stamp_waiting;
    let now = 3 * DAY_MS;
    let mut landings = vec![RepoLanding { repo: "alpha".into(), hourly: vec![0; 24], last_landing: now / 1000 - 14 * 3600, ..Default::default() }];
    let mut null_row = printed_row("sp-null", 0);
    null_row["since"] = serde_json::Value::Null;
    stamp_waiting(&mut landings, &[printed_row("sp-old", now / 1000 - 7_200), null_row]);
    let v = decide(&Carry::default(), &Snapshot { readable: true, landings, ..Default::default() }, now, &config());
    assert_eq!(v.acts.iter().filter(|a| a.name == FILE).count(), 1, "{:?}", v.acts);
    let pages: Vec<_> = v.acts.iter().filter(|a| a.name == INBOX_APPEND && a.input.contains("] P2 ") && a.input.contains("sp-null")).collect();
    assert_eq!(pages.len(), 1, "{:?}", v.acts);
}

const REPO: &str = "remarkable-pipeline";
const CARRY: &str = r#"{"repos":{"remarkable-pipeline":{"state":"Idle","blocked":{"sp-j9ci07":"sp-tec2pp"}}}}"#;
const FORGE_CARRY: &str = r#"{"repos":{"remarkable-pipeline":{"state":"Idle","round_key":null,"held":[],"base_moved":null,"conflict_streaks":{},"blocked":{"sp-j9ci07":"sp-tec2pp"}}}}"#;

fn dependency_world(beads: &[&str], states: &[(&str, &str)]) -> Host {
    let mut h = waiting(landed(5), beads.len() as u64, Last::Ago(7_234));
    h.snap.landings[0].repo = REPO.into();
    h.snap.landings[0].beads = beads.iter().map(|b| b.to_string()).collect();
    h.snap.landings[0].oldest = Some(Waiting { bead: beads[0].into(), since: Some(1) });
    h.snap.states = states.iter().map(|(b, s)| (b.to_string(), s.to_string())).collect();
    h
}

fn dependency_run(h: Host, stream: &'static str, tag: runtime::event::EventKindTag, carry: &str) -> TaskRig {
    let (_, s, kind) = WatchThroughput::boot();
    let mut r = rig_unticked(Arc::new(Mutex::new(h)));
    r.tick(stream, runtime::event::EventKind::new(tag, carry)).unwrap();
    r.tick(s, kind).unwrap();
    r
}

fn inbox(r: &TaskRig) -> Vec<String> {
    named(r, INBOX_APPEND)
}

#[test_id(tljpr5x)]
#[test]
fn a_stall_whose_only_waiting_bead_has_a_working_blocker_files_nothing_and_says_so_once() {
    let h = dependency_world(&["sp-j9ci07"], &[("sp-tec2pp", "WORKING"), ("sp-j9ci07", "CERTIFIED")]);
    let mut r = dependency_run(h, spira_runtime::tasks::a::batcher_local::STREAM, spira_runtime::tasks::a::batcher_local::STATE, CARRY);
    r.advance(3 * CADENCE_MS).unwrap();
    assert!(named(&r, FILE).is_empty(), "{:?}", named(&r, FILE));
    let lines = inbox(&r);
    assert!(lines.iter().all(|l| !l.contains("P0")), "{lines:?}");
    let waits: Vec<&String> = lines.iter().filter(|l| l.contains("sp-j9ci07 waiting on sp-tec2pp (WORKING)")).collect();
    assert_eq!(waits.len(), 1, "one line per episode: {lines:?}");
    let carry: Carry = serde_json::from_str(&r.journaled().iter().rev().find(|e| e.kind.tag == STATE).unwrap().kind.body).unwrap();
    assert!(carry.waiting_on.contains_key(REPO), "{carry:?}");
}

#[test_id(tdw6u5r)]
#[test]
fn a_stall_whose_blocker_is_idle_files_and_pages_as_before() {
    let h = dependency_world(&["sp-j9ci07"], &[("sp-j9ci07", "CERTIFIED")]);
    let r = dependency_run(h, spira_runtime::tasks::a::batcher_local::STREAM, spira_runtime::tasks::a::batcher_local::STATE, CARRY);
    assert_eq!(named(&r, FILE).len(), 1);
    assert!(inbox(&r).iter().any(|l| l.contains("P0 remarkable-pipeline: landing stalled")), "{:?}", inbox(&r));
}

#[test_id(tsozuv2)]
#[test]
fn a_repo_with_one_blocked_and_one_unblocked_waiting_bead_still_files() {
    let h = dependency_world(&["sp-j9ci07", "sp-other"], &[("sp-tec2pp", "WORKING")]);
    let r = dependency_run(h, spira_runtime::tasks::a::batcher_local::STREAM, spira_runtime::tasks::a::batcher_local::STATE, CARRY);
    assert_eq!(named(&r, FILE).len(), 1);
}

#[test_id(tvkerv9)]
#[test]
fn the_forge_batchers_blocked_map_excuses_the_stall_too() {
    let h = dependency_world(&["sp-j9ci07"], &[("sp-tec2pp", "WORKING")]);
    let r = dependency_run(h, spira_runtime::tasks::a::batcher_forge::STREAM, spira_runtime::tasks::a::batcher_forge::STATE, FORGE_CARRY);
    assert!(named(&r, FILE).is_empty());
    assert!(inbox(&r).iter().any(|l| l.contains("sp-j9ci07 waiting on sp-tec2pp (WORKING)")), "{:?}", inbox(&r));
}

#[test_id(tx7f01m)]
#[test]
fn the_row_names_the_repo_a_dependency_wait_excuses() {
    let h = dependency_world(&["sp-j9ci07"], &[("sp-tec2pp", "WORKING")]);
    let r = dependency_run(h, spira_runtime::tasks::a::batcher_local::STREAM, spira_runtime::tasks::a::batcher_local::STATE, CARRY);
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&r, NAME);
    assert!(rows.contains(&format!("waiting_on/{REPO}")), "{rows:?}");
}

#[test_id(tcabs6n)]
#[test]
fn a_blocker_that_goes_idle_inside_one_episode_files_that_episode_then() {
    let now = 3 * DAY_MS;
    let mut snap = Snapshot { states: [("sp-tec2pp".to_string(), "WORKING".to_string())].into(), blocked: [("sp-x".to_string(), "sp-tec2pp".to_string())].into(), ..stalled_snapshot(now / 1000) };
    snap.landings[0].beads = vec!["sp-x".into()];
    let first = decide(&Carry::default(), &snap, now, &config());
    assert!(first.acts.iter().all(|a| a.name != FILE) && first.carry.waiting_on.contains_key("alpha"));
    snap.states.clear();
    let second = decide(&first.carry, &snap, now + CADENCE_MS, &config());
    assert_eq!(second.acts.iter().filter(|a| a.name == FILE).count(), 1);
    assert!(second.carry.waiting_on.is_empty());
}

#[test_id(tkfvg6g)]
#[test]
fn one_bead_held_by_a_live_blocker_and_one_by_an_idle_one_files() {
    let now = 3 * DAY_MS;
    let mut snap = Snapshot { states: [("sp-live".to_string(), "WORKING".to_string())].into(), blocked: [("sp-a".to_string(), "sp-live".to_string()), ("sp-b".to_string(), "sp-idle".to_string())].into(), ..stalled_snapshot(now / 1000) };
    snap.landings[0].beads = vec!["sp-a".into(), "sp-b".into()];
    assert_eq!(decide(&Carry::default(), &snap, now, &config()).acts.iter().filter(|a| a.name == FILE).count(), 1);
}

#[test_id(t07j7sf)]
#[test]
fn a_spent_observe_bound_skips_the_ref_reread_and_a_fresh_one_allows_it() {
    let now = 1_700_000_000u64;
    let (dir, w) = flaky_git_repo("throughput-bound", "rev-parse --verify -q local/main", true, now);
    let spent = std::time::Instant::now() - std::time::Duration::from_secs(60);
    let w_spent = spira_runtime::world::World { observe_bound: Some((spent, std::time::Duration::from_secs(10))), ..w.clone() };
    let err = w_spent.landed_on_refs(now, "sp").expect_err("no time left, so no re-read");
    assert!(err.contains("no landing ref resolves"), "{err}");
    let _ = std::fs::remove_file(dir.join("repo/.git/flaky-fired"));
    let fresh = spira_runtime::world::World { observe_bound: Some((std::time::Instant::now(), std::time::Duration::from_secs(60))), ..w };
    fresh.landed_on_refs(now, "sp").expect("a fresh bound allows the re-read");
}

fn unreadable_during(activation: Option<spira_runtime::watchers::Activation>) -> Snapshot {
    Snapshot { readable: false, why: "spira-lc show: cannot tell: deadline".into(), activation, ..Default::default() }
}

#[test_id(tx163bs)]
#[test]
fn an_unreadable_store_inside_an_activation_window_waits_and_does_not_page() {
    let a = spira_runtime::watchers::Activation { sha: "0123456789abcdef0123".into(), since: 1_000_000 };
    let v = decide(&Carry::default(), &unreadable_during(Some(a)), 1_000_030_000, &config());
    let said: Vec<&str> = v.acts.iter().map(|a| a.input.as_str()).collect();
    assert!(!said.iter().any(|t| t.contains("P0 BLIND")), "{said:?}");
    assert_eq!(said.iter().filter(|t| t.contains("WAIT BLIND") && t.contains("0123456789ab") && t.contains("bound 75s")).count(), 1, "{said:?}");
    assert!(!v.carry.blind, "the first unreadable tick after the window pages");
}

fn maintained(pid: u32) -> spira_world::freeze::Maintenance {
    spira_world::freeze::Maintenance { owner: "concierge".into(), why: "compact the store".into(), since: 1_000_000, pid }
}

#[test_id(t3snu7e)]
#[test]
fn an_unreadable_store_under_live_maintenance_waits_and_does_not_page() {
    let snap = Snapshot { maintenance: Some(maintained(std::process::id())), ..unreadable_during(None) };
    let v = decide(&Carry::default(), &snap, 1_000_030_000, &config());
    let said: Vec<&str> = v.acts.iter().map(|a| a.input.as_str()).collect();
    assert!(!said.iter().any(|t| t.contains("P0")), "{said:?}");
    assert_eq!(said.iter().filter(|t| t.contains("WAIT BLIND: store stopped for maintenance by concierge since") && t.contains("compact the store")).count(), 1, "{said:?}");
    assert!(!v.carry.blind);
}

#[test_id(ttd4dvz)]
#[test]
fn an_unreadable_store_under_orphaned_maintenance_pages_once_naming_thaw_store() {
    let snap = Snapshot { maintenance: Some(maintained(u32::MAX)), ..unreadable_during(None) };
    let v = decide(&Carry::default(), &snap, 1_000_030_000, &config());
    let pages = v.acts.iter().filter(|a| a.input.contains("P0 BLIND") && a.input.contains("orphaned") && a.input.contains("world thaw-store")).count();
    assert_eq!(pages, 1, "{:?}", v.acts);
    assert!(!v.carry.blind);
    let again = decide(&v.carry, &snap, 1_000_045_000, &config());
    assert!(again.acts.iter().all(|a| !a.input.contains("P0")), "paged once");
}

#[test_id(tviqb2b)]
#[test]
fn an_unreadable_store_outside_any_activation_window_pages_blind_past_the_stall_bound() {
    let carry = Carry { blind_since_ms: Some(1_000_000_000 - 601_000), ..Default::default() };
    let v = decide(&carry, &unreadable_during(None), 1_000_000_000, &config());
    assert!(v.acts.iter().any(|a| a.input.contains("P0 BLIND")));
    assert!(v.carry.blind);
}

fn unreadable_tick(carry: &Carry, at_s: u64) -> spira_runtime::tasks::e::watch_throughput::Verdict {
    decide(carry, &unreadable_during(None), at_s * 1000, &config())
}

#[test_id(t0q57mz)]
#[test]
fn blindness_pages_strictly_past_the_stall_bound_and_names_the_seconds() {
    let first = unreadable_tick(&Carry::default(), 1_000);
    assert!(first.acts.is_empty());
    let at_bound = unreadable_tick(&first.carry, 1_600);
    assert!(at_bound.acts.is_empty() && !at_bound.carry.blind, "{:?}", at_bound.acts);
    let past = unreadable_tick(&at_bound.carry, 1_660);
    assert!(past.acts.iter().any(|a| a.input.contains("P0 BLIND") && a.input.contains("for 660s")), "{:?}", past.acts);
}

#[test_id(toex7q2)]
#[test]
fn a_halt_restarts_the_blindness_clock_and_maintenance_does_not() {
    let first = unreadable_tick(&Carry::default(), 1_000);
    let halted = decide(&first.carry, &Snapshot { halt: Some(spira_runtime::watchers::halt::Halt::default()), ..Default::default() }, 1_500_000, &config());
    assert_eq!(halted.carry.blind_since_ms, None);
    let waiting = Snapshot { maintenance: Some(maintained(std::process::id())), ..unreadable_during(None) };
    let kept = decide(&first.carry, &waiting, 1_300_000, &config());
    assert_eq!(kept.carry.blind_since_ms, first.carry.blind_since_ms);
}
