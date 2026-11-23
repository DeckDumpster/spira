//! `World::claim_count_snapshot` against a spira-lc double on a socket: the one read it sends, and
//! that a refused or unparsable list is an error, never an empty snapshot.

use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::watchers::claim_count::Snapshot;
use spira_runtime::world::World;

struct Double {
    _dir: testkit::TempDir,
    world: World,
    sent: Arc<Mutex<Vec<String>>>,
}

fn double(answer: Result<String, &'static str>) -> Double {
    let dir = testkit::TempDir::new("world-claim-count");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let sent = Arc::new(Mutex::new(vec![]));
    let log = sent.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            log.lock().unwrap().push(argv.join(" "));
            answer.clone().map_err(RuntimeError::refused)
        })
    });
    Double {
        _dir: dir,
        world: World {
            lc_socket: path,
            ..World::default()
        },
        sent,
    }
}

fn live() -> String {
    serde_json::json!([
        { "bead_id": "sp-a", "builder_claims": "5", "session_ends": [{ "claim": 1, "left": "Submit by d", "back": null, "at": 5 }] },
        { "bead_id": "sp-fresh", "builder_claims": "0", "session_ends": [] },
    ])
    .to_string()
}

#[test_id(tuixqmp)]
#[test]
fn the_snapshot_reads_the_live_list_alone_and_keeps_rows_with_a_builder_claim() {
    let d = double(Ok(live()));
    let snap: Snapshot = serde_json::from_str(&d.world.claim_count_snapshot().unwrap()).unwrap();
    assert_eq!(snap.rows.len(), 1, "{snap:?}");
    assert_eq!(
        (
            snap.rows[0].id.as_str(),
            snap.rows[0].n,
            snap.rows[0].ends.len()
        ),
        ("sp-a", 5, 1)
    );
    assert_eq!(*d.sent.lock().unwrap(), ["list --live"]);
}

#[test_id(t7gmdvd)]
#[test]
fn a_refused_or_unparsable_list_is_an_error_and_not_an_empty_snapshot() {
    assert!(double(Err("list refused"))
        .world
        .claim_count_snapshot()
        .is_err());
    assert!(double(Ok("not json".into()))
        .world
        .claim_count_snapshot()
        .is_err());
    let bad =
        serde_json::json!([{ "bead_id": "sp-a", "builder_claims": "many", "session_ends": [] }])
            .to_string();
    let err = double(Ok(bad)).world.claim_count_snapshot().unwrap_err();
    assert!(err.contains("sp-a"), "{err}");
}
