//! `World::round_snapshot` over a real repo map and queue record and a spira-lc double: the
//! SUBMITTED pool is counted unheld, and a round whose batch the store still has OPEN is
//! reported with the second its record says it opened.

use std::os::unix::net::UnixListener;

use runtime::RuntimeError;
use spira_runtime::watchers::round_duty::{decide, Carry, RoundDuty, RoundState, Snapshot};
use spira_runtime::world::World;

fn world(batches_answer: bool) -> (testkit::TempDir, World) {
    let dir = testkit::TempDir::new("world-round-snapshot");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| match argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["list", "--state", "SUBMITTED", "--repo", "alpha"] => Ok(r#"[{"bead_id":"a-1","holds":[]},{"bead_id":"a-2","holds":[]},{"bead_id":"a-3","holds":["ask"]}]"#.into()),
            ["list", "--state", "CERTIFIED", "--repo", "alpha"] => Ok(r#"[{"bead_id":"a-9","holds":[]}]"#.into()),
            ["list", "--batches", "--window", "1"] if !batches_answer => Err(RuntimeError::new("store unreadable")),
            ["list", "--batches", "--window", "1"] => Ok(r#"[{"batch_id":"b-open","repo":"alpha","state":"OPEN","opened_at":"1700000000"},{"batch_id":"b-old","repo":"alpha","state":"ABANDONED","opened_at":"1600000000"},{"batch_id":"b-elsewhere","repo":"beta","state":"CI_RUNNING","opened_at":"1700000000"}]"#.into()),
            _ => Err(RuntimeError::new("the double has no such verb")),
        })
    });
    let queue = dir.path().join("queue");
    let map = dir.path().join("repos");
    std::fs::write(&map, "alpha|/x|queue.local|main\n").unwrap();
    let w = World { lc_socket: sock, queue_dir: queue, repo_map: map, run_dir: dir.path().join("run"), home_repo: "alpha".into(), ..World::default() };
    (dir, w)
}

#[test]
fn unheld_submitted_beads_are_counted_and_an_open_state_round_is_reported_unadvanced() {
    let _env = testkit::env_read();
    let (_dir, w) = world(true);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!((snap.submitted, snap.submitted_by_repo.as_str()), (2, "alpha=2"));
    assert_eq!((snap.state, snap.open), (RoundState::Open, 1));
    assert_eq!(snap.unadvanced, [("b-open".to_string(), 1_700_000_000)]);
}

#[test]
fn a_round_the_store_holds_open_is_open_though_the_queue_directory_has_no_record_of_it() {
    let _env = testkit::env_read();
    let (dir, w) = world(true);
    assert!(!dir.path().join("queue/alpha").exists());
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!((snap.state, snap.open), (RoundState::Open, 1), "only the home repo's unended round counts");
}

#[test]
fn an_unreadable_round_source_is_unknown_never_none() {
    let _env = testkit::env_read();
    let (_dir, w) = world(false);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!(snap.state, RoundState::Unknown);
}

#[test]
fn a_store_that_refuses_the_lifecycle_read_is_named_in_the_error_with_its_reason() {
    let _env = testkit::env_read();
    let dir = testkit::TempDir::new("world-round-refused");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || runtime::socket::answer(&listener, &|_| Err(RuntimeError::new("stale past 1500ms: the watch is lost: cannot tell: deadline"))));
    let map = dir.path().join("repos");
    std::fs::write(&map, "alpha|/x|queue.local|main\n").unwrap();
    let w = World { lc_socket: sock, queue_dir: dir.path().join("queue"), repo_map: map, run_dir: dir.path().join("run"), home_repo: "alpha".into(), ..World::default() };
    let why = w.round_snapshot("{}").unwrap_err();
    assert!(why.starts_with("lifecycle did not answer for alpha: ") && why.contains("the watch is lost: cannot tell: deadline"), "{why}");
}

#[test]
fn a_refused_round_view_is_unknown_with_the_refusal_in_why() {
    let _env = testkit::env_read();
    let (_dir, w) = world(false);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert!(snap.why.contains("store unreadable"), "{}", snap.why);
}

fn cfg() -> RoundDuty {
    RoundDuty { interval_ms: 15_000, min: 1, max_open: 1, remind_secs: 600, idle_cut_secs: 900, lease_ttl_secs: 300, halt_page_secs: 3_600, activation_bound_s: 90, run_dir: Default::default() }
}

#[test]
fn a_certifying_round_with_a_certified_bead_set_aside_pages_nothing_and_no_round_pages_once() {
    let _env = testkit::env_read();
    let (_dir, w) = world(true);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!(snap.certified, 1);
    let (c, say) = decide(&cfg(), &Carry::default(), &snap, 1000);
    assert!(say.is_empty(), "{say:?}");
    assert!(decide(&cfg(), &c, &snap, 5000).1.is_empty(), "a long wait behind an open round is sequencing");

    let (_dir, w) = world(true);
    let w = World { home_repo: "gamma".into(), ..w };
    let mut snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!(snap.state, RoundState::None);
    (snap.admissible, snap.admissible_by_repo) = (snap.certified, snap.by_repo.clone());
    let (c, first) = decide(&cfg(), &Carry::default(), &snap, 1000);
    assert_eq!(first.len(), 1, "{first:?}");
    let (_, later) = decide(&cfg(), &c, &snap, 1100);
    assert!(later.is_empty(), "{later:?}");
}

#[test]
fn a_work_halt_stamp_reaches_the_snapshot_with_its_reason_and_no_stamp_is_no_halt() {
    let _env = testkit::env_read();
    let (dir, w) = world(true);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!(snap.halt, None);
    std::fs::create_dir_all(dir.path().join("run")).unwrap();
    std::fs::write(dir.path().join("run/world.halted"), "2026-11-11T05:10:52Z\nwhy: operator order: freeze\n").unwrap();
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    let h = snap.halt.expect("halted");
    assert_eq!(h.why, "operator order: freeze");
    assert!(h.since > 1_000_000_000);
}

fn stamp(dir: &testkit::TempDir) {
    std::fs::create_dir_all(dir.path().join("run")).unwrap();
    std::fs::write(dir.path().join("run/world.halted"), "2026-11-11T05:10:52Z\nwhy: operator order: freeze\n").unwrap();
}

#[test]
fn the_throughput_and_transitions_snapshots_carry_the_halt_even_when_unreadable() {
    let _env = testkit::env_read();
    let (dir, w) = world(true);
    stamp(&dir);
    let t: spira_runtime::tasks::e::watch_throughput::Snapshot = serde_json::from_str(&w.throughput_snapshot(r#"{"now":1700000000,"abandon_window_s":3600}"#).unwrap()).unwrap();
    assert_eq!(t.halt.map(|h| h.why), Some("operator order: freeze".to_string()));
    let input = format!(r#"{{"now":1700000000,"window_s":3600,"releases":"{}","release_repo":"alpha"}}"#, dir.path().join("rel").display());
    let x: spira_runtime::tasks::e::watch_transitions::Snapshot = serde_json::from_str(&w.transitions_snapshot(&input).unwrap()).unwrap();
    assert_eq!(x.halt.map(|h| h.why), Some("operator order: freeze".to_string()));
}

#[test]
fn the_throughput_snapshot_names_the_oldest_waiting_bead_certified_ones_included() {
    let dir = testkit::TempDir::new("world-throughput-oldest");
    let repo = dir.path().join("alpha");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base"]] {
        assert!(std::process::Command::new("git").current_dir(&repo).args(args).status().unwrap().success());
    }
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_ID_PREFIX", "a")]);
    let _env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| match argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["list", "--state", "WORKING,SUBMITTED,CERTIFIED,IN_DELIVERY"] => Ok(r#"[{"bead_id":"a-dep","repo":"alpha","state":"WORKING","since":1699999500},{"bead_id":"a-new","repo":"alpha","state":"SUBMITTED","since":1699999000},{"bead_id":"a-old","repo":"alpha","state":"CERTIFIED","since":1699990000}]"#.into()),
            ["list", "--batches", "--window", "86400"] => Ok("[]".into()),
            _ => Err(RuntimeError::new("the double has no such verb")),
        })
    });
    let map = dir.path().join("repos");
    std::fs::write(&map, format!("alpha|{}|queue.local|main\n", repo.display())).unwrap();
    let w = World { lc_socket: sock, queue_dir: dir.path().join("queue"), repo_map: map, run_dir: dir.path().join("run"), home_repo: "alpha".into(), ..World::default() };
    let t: spira_runtime::tasks::e::watch_throughput::Snapshot = serde_json::from_str(&w.throughput_snapshot(r#"{"now":1700000000,"abandon_window_s":3600}"#).unwrap()).unwrap();
    assert!(t.readable, "{}", t.why);
    let a = &t.landings[0];
    assert_eq!(a.waiting, 2, "a WORKING row is not a waiting bead");
    assert_eq!(a.beads, ["a-new", "a-old"]);
    assert_eq!(t.states.get("a-dep").map(String::as_str), Some("WORKING"));
    assert_eq!(t.states.get("a-old").map(String::as_str), Some("CERTIFIED"));
    let oldest = a.oldest.as_ref().expect("the snapshot stamps the oldest waiting bead");
    assert_eq!((oldest.bead.as_str(), oldest.since), ("a-old", Some(1699990000)));
}
