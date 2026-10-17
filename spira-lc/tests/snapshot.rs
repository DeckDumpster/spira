//! `list --snapshot` is answered from the shared snapshot file without touching the store while
//! the file is fresh, and goes to the store when it is stale. The database is a listener that
//! accepts and hangs up, so "reached" is observed.

use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn run(args: &[&str], dir: &std::path::Path, snapshot: &std::path::Path, accepted: &AtomicUsize) -> (i32, String, bool) {
    let before = accepted.load(Ordering::SeqCst);
    let toml = spira_config::process::fixture_toml(
        dir,
        &[
            ("SPIRA_RUN", dir.to_str().unwrap()),
            ("SPIRA_LC_SOCKET", ""),
            ("SPIRA_LC_PASSWORD_FILE", ""),
            ("SPIRA_LC_SNAPSHOT", snapshot.to_str().unwrap()),
            ("SPIRA_LC_SNAPSHOT_SECS", "60"),
        ],
    );
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
        .args(args)
        .env("SPIRA_LC_HOST", "127.0.0.1")
        .env("SPIRA_LC_PORT", PORT.load(Ordering::SeqCst).to_string())
        .env("SPIRA_LC_PASSWORD", "")
        .env("SPIRA_HOME", concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))
        .env("SPIRA_TOML", &toml)
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), accepted.load(Ordering::SeqCst) > before)
}

static PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

fn write_snapshot(path: &std::path::Path, at: i64) {
    let bead = |id: &str, state: &str| serde_json::json!({"bead_id": id, "state": state, "holds": "[]", "express": "0", "repo": "spira", "title": "t", "blocked_by": []});
    let body = serde_json::json!({"at": at, "beads": [bead("sp-a", "READY"), bead("sp-b", "CERTIFIED")]});
    std::fs::write(path, body.to_string()).unwrap();
}

#[test]
fn a_fresh_snapshot_answers_and_a_stale_one_reaches_the_store() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    PORT.store(listener.local_addr().unwrap().port(), Ordering::SeqCst);
    let accepted = Arc::new(AtomicUsize::new(0));
    let seen = accepted.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            seen.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    let t = testkit::TempDir::new("spira-lc-snapshot");
    let snap = t.path().join("bead-snapshot.json");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;

    write_snapshot(&snap, now);
    let (code, out, reached) = run(&["list", "--snapshot", "--state", "CERTIFIED"], t.path(), &snap, &accepted);
    assert_eq!((code, reached), (0, false), "{out}");
    let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert_eq!(rows.iter().map(|r| r["bead_id"].as_str().unwrap()).collect::<Vec<_>>(), ["sp-b"]);

    let (_, _, reached) = run(&["list", "--state", "CERTIFIED"], t.path(), &snap, &accepted);
    assert!(reached, "a list without --snapshot always asks the store");

    write_snapshot(&snap, now - 3600);
    let (code, _, reached) = run(&["list", "--snapshot", "--state", "CERTIFIED"], t.path(), &snap, &accepted);
    assert_eq!((code, reached), (2, true), "a stale snapshot is not believed");
}
