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
    let (_dir, w) = world(true);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!((snap.submitted, snap.submitted_by_repo.as_str()), (2, "alpha=2"));
    assert_eq!((snap.state, snap.open), (RoundState::Open, 1));
    assert_eq!(snap.unadvanced, [("b-open".to_string(), 1_700_000_000)]);
}

#[test]
fn a_round_the_store_holds_open_is_open_though_the_queue_directory_has_no_record_of_it() {
    let (dir, w) = world(true);
    assert!(!dir.path().join("queue/alpha").exists());
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!((snap.state, snap.open), (RoundState::Open, 1), "only the home repo's unended round counts");
}

#[test]
fn an_unreadable_round_source_is_unknown_never_none() {
    let (_dir, w) = world(false);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!(snap.state, RoundState::Unknown);
}

fn cfg() -> RoundDuty {
    RoundDuty { interval_ms: 15_000, min: 1, max_open: 1, remind_secs: 600, idle_cut_secs: 900, lease_ttl_secs: 300 }
}

#[test]
fn a_certifying_round_with_a_certified_bead_set_aside_pages_nothing_and_no_round_pages_once() {
    let (_dir, w) = world(true);
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!(snap.certified, 1);
    let (c, say) = decide(&cfg(), &Carry::default(), &snap, 1000);
    assert!(say.is_empty(), "{say:?}");
    assert!(decide(&cfg(), &c, &snap, 5000).1.is_empty(), "a long wait behind an open round is sequencing");

    let (_dir, w) = world(true);
    let w = World { home_repo: "gamma".into(), ..w };
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!(snap.state, RoundState::None);
    let (c, first) = decide(&cfg(), &Carry::default(), &snap, 1000);
    assert_eq!(first.len(), 1, "{first:?}");
    let (_, later) = decide(&cfg(), &c, &snap, 1100);
    assert!(later.is_empty(), "{later:?}");
}
