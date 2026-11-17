//! `World::sweep_snapshot` reads the live rows by state and the terminal rows it needs by id,
//! never a list of every terminal row: the content beads with no live row and the blockers of
//! the live rows are the ids it asks for.

use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

#[test]
fn the_sweep_asks_for_the_terminal_rows_of_the_ids_it_looks_up_and_no_others() {
    let dir = testkit::TempDir::new("world-sweep-snapshot");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            log.lock().unwrap().push(argv.join(" "));
            match (argv[0].as_str(), argv.get(1).map(String::as_str)) {
                ("list", Some("--state")) => Ok(serde_json::json!([{ "bead_id": "sp-live", "state": "READY", "blocked_by": ["sp-done", "sp-other"] }]).to_string()),
                ("list", Some("--ids")) => Ok(serde_json::json!([{ "bead_id": "sp-done", "state": "LANDED" }]).to_string()),
                ("content", _) => Ok(serde_json::json!([{ "id": "sp-live", "labels": [], "status": "open" }, { "id": "sp-old", "labels": [], "status": "open" }]).to_string()),
                _ => Err(RuntimeError::new(format!("the double has no {}", argv.join(" ")))),
            }
        })
    });
    let world = World { lc_socket: path, run_dir: dir.path().join("run"), ..World::default() };
    let snap: serde_json::Value = serde_json::from_str(&world.sweep_snapshot().unwrap()).unwrap();
    assert_eq!(snap["terminal"], serde_json::json!([["sp-done", "LANDED"]]));
    let sent = sent.lock().unwrap();
    assert!(sent.iter().any(|a| a == "list --ids sp-done,sp-old,sp-other"), "{sent:?}");
    assert!(sent.iter().all(|a| !a.contains("LANDED,DONE")), "no whole-terminal list: {sent:?}");
}
