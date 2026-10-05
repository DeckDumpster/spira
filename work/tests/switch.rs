//! The binary's edge (DESIGN.md §3): every verb reaches the socket, and an unreachable
//! socket is "cannot tell". There is no off mode (sp-v62vn).

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn scratch(tag: &str) -> testkit::TempDir {
    testkit::TempDir::new(&format!("work-switch-{tag}"))
}

/// A listening socket that answers `{"exit_code":0,"stdout":"ok"}` and records a connection.
fn machine(sock: &PathBuf) -> Arc<AtomicBool> {
    let hit = Arc::new(AtomicBool::new(false));
    let l = UnixListener::bind(sock).unwrap();
    let h = hit.clone();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for s in l.incoming().flatten() {
            h.store(true, Ordering::SeqCst);
            let mut line = String::new();
            let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut line);
            let _ = writeln!(&s, r#"{{"exit_code":0,"stdout":"ok"}}"#);
        }
    });
    hit
}

fn work(sock: &Path, home: &Path, verb: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_work"))
        .arg(verb)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_WORK_BEAD_ID", "sp-abc12")
        .env("SPIRA_LC_SOCKET", sock)
        .output()
        .unwrap()
}

#[test]
fn a_verb_reaches_the_socket() {
    let d = scratch("reach");
    let sock = d.join("sock");
    let hit = machine(&sock);
    let o = work(&sock, &d, "show");
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "ok");
    assert!(hit.load(Ordering::SeqCst));
}

#[test]
fn an_unreachable_socket_is_cannot_tell() {
    let d = scratch("gone");
    let o = work(&d.join("absent-sock"), &d, "show");
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot tell"));
}
