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

/// A listening socket that answers a header and `ok` and records a connection.
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
            let _ = write!(&s, "{{\"exit_code\":0,\"len\":2}}\nok");
        }
    });
    hit
}

fn work(sock: &Path, home: &Path, verb: &str) -> std::process::Output {
    // SPIRA_LC_SOCKET is a registered config key (spira/conf.d), resolved only through
    // $SPIRA_TOML now — this is the one test that exercises that top-level read, so it
    // writes a complete fixture config file rather than setting the key directly. An empty
    // conf.d is enough: resolving SPIRA_LC_SOCKET itself needs no registry file, only a
    // conf.d directory that exists (spira-config's own `fixture_home_repo` test helper).
    std::fs::create_dir_all(home.join("conf.d")).unwrap();
    let toml = spira_config::process::fixture_toml(home, &[("SPIRA_LC_SOCKET", sock.to_str().unwrap())]);
    Command::new(env!("CARGO_BIN_EXE_work"))
        .arg(verb)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_WORK_BEAD_ID", "sp-abc12")
        .env("SPIRA_HOME", home)
        .env("SPIRA_TOML", &toml)
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

#[test]
fn a_reply_slower_than_one_read_slice_is_waited_for() {
    let d = scratch("slow");
    let sock = d.join("sock");
    let l = UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for s in l.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(s.try_clone().unwrap()).read_line(&mut line);
            std::thread::sleep(std::time::Duration::from_millis(3000));
            let _ = write!(&s, "{{\"exit_code\":0,\"len\":4}}\nlate");
        }
    });
    let o = work(&sock, &d, "show");
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "late");
}
