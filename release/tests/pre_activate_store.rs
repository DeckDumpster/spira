//! `spira/pre-activate.sh`'s store check, run under `bash` with a stubbed `spira-lc`. The release
//! it judges is empty, so every other check fails; only the `store` lines are read.

#[macro_use]
extern crate spira_test_id;

use std::process::Command;

/// `(the store check's line, the arguments of every spira-lc run)` for a stub answering `body`.
fn store_check(body: &str) -> (String, Vec<String>) {
    let dir = testkit::TempDir::new("pre-activate-store");
    let (bin, rel, log) = (dir.join("bin"), dir.join("rel"), dir.join("calls"));
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&rel).unwrap();
    testkit::write_exe(bin.join("spira-lc"), &format!("#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n", log.display()));
    let script = testkit::manifest_dir().parent().unwrap().join("spira/pre-activate.sh");
    let out = Command::new("bash")
        .env_clear()
        .arg(&script)
        .arg(&rel)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", dir.path())
        .env("SPIRA_STORE_CHECK_TIMEOUT", "7")
        .output()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let from = said.find("pre-activate: FAIL store:").or_else(|| said.find("pre-activate: ok   store")).unwrap_or_else(|| panic!("no store line: {said}"));
    let rest = &said[from..];
    let end = rest[1..].find("pre-activate: ").map_or(rest.len(), |i| i + 1);
    let calls = std::fs::read_to_string(&log).unwrap_or_default().lines().map(str::to_string).collect();
    (rest[..end].trim_end().to_string(), calls)
}

#[test_id(t1iol3d)]
#[test]
fn a_store_with_every_column_passes_after_one_check() {
    let (line, calls) = store_check("exit 0");
    assert_eq!(line, "pre-activate: ok   store");
    assert_eq!(calls, ["admin-store check"]);
}

// covers: UC-config-store-preflight-09
#[test_id(tjwhk76)]
#[test]
fn a_gap_is_a_schema_mismatch_naming_the_column_and_is_not_asked_twice() {
    let (line, calls) = store_check("echo issues.row_lock; exit 1");
    assert_eq!(line, "pre-activate: FAIL store: schema mismatch: issues.row_lock");
    assert_eq!(calls, ["admin-store check"]);
}

// 124 is what the script's own `timeout` exits with when the check outlasts it.
#[test_id(tw3r7x8)]
#[test]
fn a_check_that_outlasts_its_limit_is_a_slow_store_after_two_attempts() {
    let (line, calls) = store_check("exit 124");
    assert_eq!(line, "pre-activate: FAIL store: store slow: no answer in 7 s (2 attempts)");
    assert_eq!(calls.len(), 2);
}

#[test_id(tbq6ad9)]
#[test]
fn an_unreachable_store_is_reported_as_such_after_two_attempts() {
    let (line, calls) = store_check("echo 'admin-store: the beads store is unreachable at 127.0.0.1:1: refused' >&2; exit 2");
    assert_eq!(line, "pre-activate: FAIL store: store slow/unreachable: admin-store: the beads store is unreachable at 127.0.0.1:1: refused");
    assert_eq!(calls.len(), 2);
}
