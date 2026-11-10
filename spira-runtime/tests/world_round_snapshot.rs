//! `World::round_snapshot` over a real repo map and queue record and a spira-lc double: the
//! SUBMITTED pool is counted unheld, and a round whose batch the store still has OPEN is
//! reported with the second its record says it opened.

use std::os::unix::net::UnixListener;

use runtime::RuntimeError;
use spira_runtime::watchers::round_duty::{RoundState, Snapshot};
use spira_runtime::world::World;

fn world() -> (testkit::TempDir, World) {
    let dir = testkit::TempDir::new("world-round-snapshot");
    let sock = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| match argv.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["list", "--state", "SUBMITTED", "--repo", "alpha"] => Ok(r#"[{"bead_id":"a-1","holds":[]},{"bead_id":"a-2","holds":[]},{"bead_id":"a-3","holds":["ask"]}]"#.into()),
            ["list", "--state", "CERTIFIED", "--repo", "alpha"] => Ok("[]".into()),
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
    let (_dir, w) = world();
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!((snap.submitted, snap.submitted_by_repo.as_str()), (2, "alpha=2"));
    assert_eq!((snap.state, snap.open), (RoundState::Open, 1));
    assert_eq!(snap.unadvanced, [("b-open".to_string(), 1_700_000_000)]);
}

#[test]
fn a_round_the_store_holds_open_is_open_though_the_queue_directory_has_no_record_of_it() {
    let (dir, w) = world();
    assert!(!dir.path().join("queue/alpha").exists());
    let snap: Snapshot = serde_json::from_str(&w.round_snapshot("{}").unwrap()).unwrap();
    assert_eq!((snap.state, snap.open), (RoundState::Open, 1), "only the home repo's unended round counts");
}

#[test]
fn an_unreadable_round_source_is_unknown_never_none() {
    let (_dir, w) = world();
    let w = World { lc_socket: w.run_dir.join("no-such.sock"), ..w };
    let snap: Result<String, String> = w.round_snapshot("{}");
    match snap {
        Err(_) => {}
        Ok(s) => assert_eq!(serde_json::from_str::<Snapshot>(&s).unwrap().state, RoundState::Unknown),
    }
}
