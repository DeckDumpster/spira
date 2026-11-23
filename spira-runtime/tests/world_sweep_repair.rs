//! `World::sweep_repair` completes a rowless bead in order: the row OPEN, each `blocks` edge the
//! content bead carries, then the promote, so the bead is never claimable ahead of its blockers.

#[macro_use]
extern crate spira_test_id;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

fn world(edges: Result<&'static str, &'static str>) -> (World, Arc<Mutex<Vec<String>>>, testkit::TempDir) {
    let dir = testkit::TempDir::new("world-sweep-repair");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            log.lock().unwrap().push(argv.join(" "));
            let v = |s: &str| argv.iter().any(|a| a == s);
            match (argv[0].as_str(), argv.get(1).map(String::as_str)) {
                ("skew-check", _) | ("create-bead", _) | ("promote-filed", _) | ("dep-add", _) => Ok(String::new()),
                ("list", Some("--state" | "--ids")) => Ok("[]".into()),
                ("content", Some("list")) => Ok(serde_json::json!([{ "id": "sp-new", "labels": ["repo:spira"], "status": "open" }]).to_string()),
                ("content", Some("dep")) if v("list") => edges.map(String::from).map_err(RuntimeError::new),
                _ => Err(RuntimeError::new(format!("the double has no {}", argv.join(" ")))),
            }
        })
    });
    (World { lc_socket: path, run_dir: dir.path().join("run"), ..World::default() }, sent, dir)
}

#[test_id(t0kk2m2)]
#[test]
fn a_rowless_bead_gets_an_open_row_then_its_blocks_edge_then_the_promote() {
    let (w, sent, _dir) = world(Ok(r#"[{"depends_on_id":"sp-blocker"}]"#));
    let done: serde_json::Value = serde_json::from_str(&w.sweep_repair(r#"{"ids":["sp-new"]}"#).unwrap()).unwrap();
    assert_eq!(done["ids"], serde_json::json!(["sp-new"]));
    assert_eq!(done["edges"]["sp-new"], serde_json::json!(["sp-blocker"]));
    let sent = sent.lock().unwrap();
    let writes: Vec<&str> = sent.iter().map(String::as_str).filter(|a| a.starts_with("create-bead") || a.starts_with("dep-add") || a.starts_with("promote-filed")).collect();
    assert_eq!(writes, ["create-bead sp-new --repo spira", "dep-add sp-new sp-blocker --type blocks", "promote-filed sp-new"]);
}

#[test_id(tajm99d)]
#[test]
fn a_bead_whose_edges_cannot_be_read_keeps_its_open_row_and_is_not_promoted() {
    let (w, sent, _dir) = world(Err("store unreadable"));
    let done: serde_json::Value = serde_json::from_str(&w.sweep_repair(r#"{"ids":["sp-new"]}"#).unwrap()).unwrap();
    assert_eq!(done["ids"], serde_json::json!([]));
    let sent = sent.lock().unwrap();
    assert!(sent.iter().any(|a| a == "create-bead sp-new --repo spira"), "{sent:?}");
    assert!(sent.iter().all(|a| !a.starts_with("promote-filed")), "{sent:?}");
}
