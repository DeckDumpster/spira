//! `World::sweep_snapshot` reads the live rows by state and the terminal rows it needs by id,
//! never a list of every terminal row: the content beads with no live row and the blockers of
//! the live rows are the ids it asks for.

#[macro_use]
extern crate spira_test_id;
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::world::World;

#[test_id(t61np4p)]
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
    let world = World {
        lc_socket: path,
        run_dir: dir.path().join("run"),
        ..World::default()
    };
    let snap: serde_json::Value = serde_json::from_str(&world.sweep_snapshot().unwrap()).unwrap();
    assert_eq!(snap["terminal"], serde_json::json!([["sp-done", "LANDED"]]));
    let sent = sent.lock().unwrap();
    assert!(
        sent.iter()
            .any(|a| a == "list --ids sp-done,sp-old,sp-other"),
        "{sent:?}"
    );
    assert!(
        sent.iter().all(|a| !a.contains("LANDED,DONE")),
        "no whole-terminal list: {sent:?}"
    );
}

#[test_id(t6ryf8k)]
#[test]
fn the_sweep_reads_each_manual_holds_raise_from_its_row_and_sends_no_history() {
    let dir = testkit::TempDir::new("world-sweep-holds");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let review = |i: usize| {
        i.is_multiple_of(3)
            .then(|| format!("review-by:2031-01-0{}T00:00:00Z later", i % 9 + 1))
    };
    let rows: Vec<serde_json::Value> = (0..30)
        .map(|i| {
            let detail = review(i).unwrap_or_else(|| format!("parked {i}"));
            serde_json::json!({ "bead_id": format!("sp-h{i:02}"), "state": "READY", "holds": ["manual"], "reason": if i.is_multiple_of(6) { detail.clone() } else { "parked".to_string() },
                "manual_hold": { "at": 1000 + i, "cause": "manual-hold", "detail": detail } })
        })
        .collect();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            log.lock().unwrap().push(argv.join(" "));
            match (argv[0].as_str(), argv.get(1).map(String::as_str)) {
                ("list", Some("--state")) => Ok(serde_json::Value::Array(rows.clone()).to_string()),
                ("content", _) => Ok("[]".to_string()),
                _ => Err(RuntimeError::new(format!(
                    "the double has no {}",
                    argv.join(" ")
                ))),
            }
        })
    });
    let world = World {
        lc_socket: path,
        run_dir: dir.path().join("run"),
        ..World::default()
    };
    let snap: serde_json::Value = serde_json::from_str(&world.sweep_snapshot().unwrap()).unwrap();
    assert!(
        sent.lock()
            .unwrap()
            .iter()
            .all(|a| !a.starts_with("history")),
        "{:?}",
        sent.lock().unwrap()
    );
    let rows = snap["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 30);
    for (i, r) in rows.iter().enumerate() {
        if i.is_multiple_of(6) {
            assert!(
                r["held_at"].is_null() && r["hold_review"].is_null(),
                "a review-by reason is not read again: {r}"
            );
        } else {
            assert_eq!(r["held_at"], serde_json::json!((1000 + i) as f64), "{r}");
            assert_eq!(
                r["hold_review"],
                if i.is_multiple_of(3) {
                    serde_json::json!(review(i))
                } else {
                    serde_json::Value::Null
                },
                "{r}"
            );
        }
    }
}

#[test_id(taqgrno)]
#[test]
fn a_manual_held_row_without_a_manual_hold_fails_the_sweep_naming_the_migration() {
    let dir = testkit::TempDir::new("world-sweep-no-raise");
    let path = dir.path().join("lc.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        runtime::socket::answer(&listener, &|argv| {
            match argv[0].as_str() {
            "list" => Ok(serde_json::json!([{ "bead_id": "sp-h", "state": "READY", "holds": ["manual"], "reason": "parked" }]).to_string()),
            "content" => Ok("[]".to_string()),
            _ => Err(RuntimeError::new("the double has no history")),
        }
        })
    });
    let world = World {
        lc_socket: path,
        run_dir: dir.path().join("run"),
        ..World::default()
    };
    let err = world.sweep_snapshot().unwrap_err();
    assert!(
        err.contains("manual_hold") && err.contains("migration"),
        "{err}"
    );
}
