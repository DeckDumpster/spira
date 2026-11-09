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
            ["show-batch", "b-open"] => Ok(r#"{"state":"OPEN"}"#.into()),
            _ => Err(RuntimeError::new("the double has no such verb")),
        })
    });
    let queue = dir.path().join("queue");
    std::fs::create_dir_all(queue.join("alpha")).unwrap();
    std::fs::write(queue.join("alpha/round"), "batch_id=b-open\nopened=1700000000\n").unwrap();
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
