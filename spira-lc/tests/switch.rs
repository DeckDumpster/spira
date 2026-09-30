//! The caller verbs read `lifecycle_enforce` before anything else (DESIGN.md §2): off, the
//! binary answers the retired shell library's off-answer and never starts `dolt` nor opens
//! the socket; on, it does reach the machine. The `dolt` here is a recorder, so "never
//! reached" is observed, not assumed.

use std::process::Command;

fn run(enforce: &str, args: &[&str], dir: &std::path::Path) -> (i32, String, bool) {
    let log = dir.join("dolt.log");
    let _ = std::fs::remove_file(&log);
    let dolt = dir.join("dolt");
    let o = Command::new(env!("CARGO_BIN_EXE_spira-lc"))
        .args(args)
        .env("SPIRA_LIFECYCLE_ENFORCE", enforce)
        .env("SPIRA_LC_DOLT_BIN", &dolt)
        .env("SPIRA_LC_SOCKET", dir.join("no-such-socket"))
        .env("SPIRA_LC_PASSWORD", "")
        .env("SPIRA_RUN", dir)
        .output()
        .unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), log.exists())
}

#[test]
fn off_never_reaches_the_machine_and_on_does() {
    let t = testkit::TempDir::new("spira-lc-switch");
    let d = t.path();
    testkit::write_exe(d.join("dolt"), &format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", d.join("dolt.log").display()));

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
        let (code, _, reached) = run("0", &args, d);
        assert_eq!((code, reached), (rc, false), "off: {args:?}");
    }
    let (_, out, _) = run("0", &["deliver", "push-delivered", "sp-a", "abc"], d);
    assert!(out.contains(" spira: lc: no delivery row for sp-a — not recording landing.sh's event"), "{out}");
    assert!(!d.join("lifecycle-cert.log").exists(), "off, certify logs nothing (lifecycle-cert.sh returned before its log)");

    // On: the machine is asked (and, being a recorder that fails, cannot tell).
    let (code, _, reached) = run("1", &["hold", "sp-a", "poison", "x"], d);
    assert_eq!((code, reached), (2, true), "on: an unreachable machine is cannot-tell, and it was asked");
    let (code, _, reached) = run("true", &["certify", "sp-a", "t", "pass", "k"], d);
    assert_eq!((code, reached), (2, true));
    let log = std::fs::read_to_string(d.join("lifecycle-cert.log")).unwrap();
    assert!(log.trim_end().ends_with("cannot-tell bead=sp-a no lifecycle row yet"), "{log}");
}
