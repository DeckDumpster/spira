use std::path::Path;
use std::process::Command;

fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    testkit::write_exe(&p, &format!("#!/bin/sh\n{body}\n"));
    p.to_string_lossy().into_owned()
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
    lc_calls: String,
}

fn bdq(enforce: &str, bd_body: &str, lc_body: &str, args: &[&str]) -> Run {
    let t = testkit::TempDir::new("bdq-lcrow");
    let d = t.path();
    let log = d.join("lc.log");
    let bd = script(d, "bd", bd_body);
    let lc = script(d, "lc", &format!("echo \"$@\" >> {}\n{lc_body}", log.display()));
    let out = Command::new(env!("CARGO_BIN_EXE_bdq"))
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_DB", d)
        .env("SPIRA_BD", &bd)
        .env("SPIRA_LC_BIN", &lc)
        .env("SPIRA_LIFECYCLE_ENFORCE", enforce)
        .env("SPIRA_ASK_LABEL", "ask-pin")
        .env("SPIRA_BDQ_CONN_RETRIES", "1")
        .output()
        .unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        lc_calls: std::fs::read_to_string(&log).unwrap_or_default(),
    }
}

const CREATE: &[&str] = &["create", "a plain title", "--silent"];

#[test]
fn a_create_under_enforce_makes_the_row_and_still_prints_the_id() {
    let r = bdq("1", "echo sp-new1", "exit 0", CREATE);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout.trim(), "sp-new1");
    assert_eq!(r.lc_calls, "create-bead sp-new1\n");
}

#[test]
fn a_create_with_lifecycle_off_makes_no_spira_lc_call() {
    let r = bdq("0", "echo sp-new2", "exit 0", CREATE);
    assert_eq!(r.code, 0);
    assert_eq!(r.stdout.trim(), "sp-new2");
    assert_eq!(r.lc_calls, "", "off must never reach spira-lc");
}

#[test]
fn a_failed_row_is_loud_and_does_not_fail_the_create() {
    let r = bdq("1", "echo sp-new3", "echo refused >&2; exit 4", CREATE);
    assert_eq!(r.code, 0, "the bead exists; a nonzero rc would invite a duplicate");
    assert_eq!(r.stdout.trim(), "sp-new3");
    assert!(r.stderr.contains("LIFECYCLE") && r.stderr.contains("sp-new3") && r.stderr.contains("NO lifecycle row"), "{}", r.stderr);
}

#[test]
fn a_failed_create_makes_no_row_and_other_verbs_none_either() {
    let r = bdq("1", "exit 1", "exit 0", CREATE);
    assert_ne!(r.code, 0);
    assert_eq!(r.lc_calls, "");
    let r = bdq("1", "echo '[]'", "exit 0", &["list", "--json"]);
    assert_eq!(r.lc_calls, "");
}

#[test]
fn a_bead_created_closed_gets_no_row() {
    let r = bdq("1", "echo sp-ins", "exit 0", &["create", "an insight", "--status", "closed", "--silent"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.lc_calls, "");
}

#[test]
fn json_create_output_is_read() {
    let r = bdq("1", "echo '{\"id\":\"sp-js\"}'", "exit 0", &["create", "a plain title", "--json"]);
    assert_eq!(r.lc_calls, "create-bead sp-js\n");
    assert!(r.stdout.contains("sp-js"));
}
