//! The lifecycle switch at the binary's edge (DESIGN.md §3): off never touches the socket;
//! on reaches it, and an unreachable socket is "cannot tell".

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("work-switch-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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

fn work(enforce: &str, sock: &PathBuf, home: &PathBuf, verb: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_work"))
        .arg(verb)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_WORK_BEAD_ID", "sp-abc12")
        .env("SPIRA_LC_SOCKET", sock)
        .env("SPIRA_LIFECYCLE_ENFORCE", enforce)
        .output()
        .unwrap()
}

#[test]
fn off_refuses_every_verb_without_touching_the_socket() {
    let d = scratch("off");
    let sock = d.join("sock");
    let hit = machine(&sock);
    for (enforce, verb) in [("0", "show"), ("", "note"), ("yes", "done"), ("0", "submit")] {
        let o = work(enforce, &sock, &d, verb);
        assert_eq!(o.status.code(), Some(3), "{enforce:?} {verb}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("lifecycle_enforce is off"), "{err}");
        assert!(o.stdout.is_empty());
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(!hit.load(Ordering::SeqCst), "the socket must never be touched with lifecycle_enforce off");
}

#[test]
fn on_reaches_the_socket() {
    let d = scratch("on");
    let sock = d.join("sock");
    let hit = machine(&sock);
    let o = work("1", &sock, &d, "show");
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "ok");
    assert!(hit.load(Ordering::SeqCst));
}

#[test]
fn on_unreachable_socket_is_cannot_tell() {
    let d = scratch("on-gone");
    let o = work("true", &d.join("absent-sock"), &d, "show");
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("cannot tell"));
}
