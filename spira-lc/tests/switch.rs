//! The caller verbs read `lifecycle_enforce` before anything else (DESIGN.md §2): off, the
//! binary answers the retired shell library's off-answer and never opens the socket nor a
//! database connection; on, it does reach the machine. The database here is a listener
//! that accepts and hangs up, so "never reached" is observed, not assumed.

use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn run(enforce: &str, args: &[&str], dir: &std::path::Path, db: &Db) -> (i32, String, bool) {
    let before = db.accepted.load(Ordering::SeqCst);
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
        .args(args)
        .env("SPIRA_LIFECYCLE_ENFORCE", enforce)
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
fn off_never_reaches_the_machine_and_on_does() {
    let t = testkit::TempDir::new("spira-lc-switch");
    let d = t.path();
    let db = hanging_up_db();

    for (args, rc) in [
        (vec!["hold", "sp-a", "poison", "x"], 2),
        (vec!["unhold", "sp-a", "poison"], 2),
        (vec!["state", "sp-a"], 2),
        (vec!["held", "sp-a", "poison"], 1),
        (vec!["holds", "sp-a"], 0),
        (vec!["list-all"], 0),
        (vec!["certify", "sp-a", "t", "pass", "k"], 2),
        (vec!["deliver", "push-delivered", "sp-a", "abc"], 1),
    ] {
        let (code, _, reached) = run("0", &args, d, &db);
        assert_eq!((code, reached), (rc, false), "off: {args:?}");
    }
    let (_, out, _) = run("0", &["deliver", "push-delivered", "sp-a", "abc"], d, &db);
    assert!(out.contains(" spira: lc: no delivery row for sp-a — not recording landing.sh's event"), "{out}");
    assert!(!d.join("lifecycle-cert.log").exists(), "off, certify logs nothing (lifecycle-cert.sh returned before its log)");

    // On: the machine is asked (and, being a listener that hangs up, cannot tell).
    let (code, _, reached) = run("1", &["hold", "sp-a", "poison", "x"], d, &db);
    assert_eq!((code, reached), (2, true), "on: an unreachable machine is cannot-tell, and it was asked");
    let (code, _, reached) = run("true", &["certify", "sp-a", "t", "pass", "k"], d, &db);
    assert_eq!((code, reached), (2, true));
    let log = std::fs::read_to_string(d.join("lifecycle-cert.log")).unwrap();
    assert!(log.trim_end().ends_with("cannot-tell bead=sp-a no lifecycle row yet"), "{log}");
}
