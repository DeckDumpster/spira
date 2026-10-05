//! The caller verbs always reach the machine (DESIGN.md §2; sp-v62vn retired the off
//! mode). The database here is a listener that accepts and hangs up, so "reached" is
//! observed, not assumed.

use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn run(args: &[&str], dir: &std::path::Path, db: &Db) -> (i32, String, bool) {
    let before = db.accepted.load(Ordering::SeqCst);
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
        .args(args)
        .env_remove("SPIRA_LIFECYCLE_ENFORCE")
        .env("SPIRA_LC_HOST", "127.0.0.1")
        .env("SPIRA_LC_PORT", db.port.to_string())
        .env("SPIRA_LC_SOCKET", dir.join("no-such-socket"))
        .env("SPIRA_LC_PASSWORD", "")
        .env("SPIRA_RUN", dir)
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), db.accepted.load(Ordering::SeqCst) > before)
}

struct Db {
    port: u16,
    accepted: Arc<AtomicUsize>,
}

fn hanging_up_db() -> Db {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let seen = accepted.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            seen.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    Db { port, accepted }
}

#[test]
fn caller_verbs_always_reach_the_machine() {
    let t = testkit::TempDir::new("spira-lc-switch");
    let d = t.path();
    let db = hanging_up_db();
    // An unreachable machine is cannot-tell, and it was asked.
    let (code, _, reached) = run(&["hold", "sp-a", "poison", "x"], d, &db);
    assert_eq!((code, reached), (2, true));
    let (code, _, reached) = run(&["certify", "sp-a", "t", "pass", "k"], d, &db);
    assert_eq!((code, reached), (2, true));
    let log = std::fs::read_to_string(d.join("lifecycle-cert.log")).unwrap();
    assert!(log.trim_end().ends_with("cannot-tell bead=sp-a no lifecycle row yet"), "{log}");
}
