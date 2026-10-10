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

fn bdq(bd_body: &str, lc_body: &str, args: &[&str]) -> Run {
    let t = testkit::TempDir::new("bdq-lcrow");
    let d = t.path();
    let log = d.join("lc.log");
    let bd = script(d, "bd", bd_body);
    let lc = script(d, "lc", &format!("echo \"$@\" >> {}\n{lc_body}", log.display()));
    // SPIRA_DB/SPIRA_BD/SPIRA_ASK_LABEL are registered config keys — `bdq` now reads them
    // through `cfg`/`$SPIRA_TOML`, not the process environment, so this fixture drives them
    // through a config file instead of setting them directly (per Ryan 2026-10-05: one
    // source of config). Each call here execs a fresh `bdq` process, so there is no shared
    // `cfg` cache across these tests to worry about. SPIRA_LC_BIN/SPIRA_BDQ_CONN_RETRIES
    // are NOT registered keys (bare env knobs, same as in production), so they still go
    // straight on the child's environment.
    let db_dir = d.to_string_lossy().into_owned();
    let toml = spira_config::process::fixture_toml(d, &[("SPIRA_DB", &db_dir), ("SPIRA_BD", &bd), ("SPIRA_ASK_LABEL", "ask-pin")]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
    let out = Command::new(env!("CARGO_BIN_EXE_bdq"))
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
        .env("SPIRA_LC_BIN", &lc)
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
fn a_create_makes_the_row_and_still_prints_the_id() {
    let r = bdq("echo sp-new1", "exit 0", CREATE);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.stdout.trim(), "sp-new1");
    assert_eq!(r.lc_calls, "create-bead sp-new1\n");
}

#[test]
fn a_failed_row_is_loud_and_does_not_fail_the_create() {
    let r = bdq("echo sp-new3", "echo refused >&2; exit 4", CREATE);
    assert_eq!(r.code, 0, "the bead exists; a nonzero rc would invite a duplicate");
    assert_eq!(r.stdout.trim(), "sp-new3");
    assert!(r.stderr.contains("LIFECYCLE") && r.stderr.contains("sp-new3") && r.stderr.contains("NO lifecycle row"), "{}", r.stderr);
}

#[test]
fn a_failed_create_makes_no_row_and_other_verbs_none_either() {
    let r = bdq("exit 1", "exit 0", CREATE);
    assert_ne!(r.code, 0);
    assert_eq!(r.lc_calls, "");
    let r = bdq("echo '[]'", "exit 0", &["list", "--json"]);
    assert_eq!(r.lc_calls, "");
}

#[test]
fn a_bead_created_closed_gets_no_row() {
    let r = bdq("echo sp-ins", "exit 0", &["create", "an insight", "--status", "closed", "--silent"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.lc_calls, "");
}

#[test]
fn json_create_output_is_read() {
    let r = bdq("echo '{\"id\":\"sp-js\"}'", "exit 0", &["create", "a plain title", "--json"]);
    assert_eq!(r.lc_calls, "create-bead sp-js\n");
    assert!(r.stdout.contains("sp-js"));
}

/// sp-mve9i: a description edit is refused while the lifecycle row has the bead WORKING with
/// a live lease — bd's own status (here `open`, no assignee) plays no part.
#[test]
fn a_description_edit_is_refused_by_the_lifecycle_claim_not_bd_status() {
    let bd = "case \"$3\" in show) echo '[{\"id\":\"sp-c\",\"status\":\"open\",\"assignee\":\"\"}]';; *) exit 0;; esac";
    let lc = "echo '{\"bead\":{\"bead_id\":\"sp-c\",\"state\":\"WORKING\",\"holder\":\"aeon-7\",\"lease_until\":\"4102444800\",\"holds\":[]},\"delivery\":null}'";
    let r = bdq(bd, lc, &["update", "sp-c", "--description", "new words"]);
    assert_eq!(r.code, 1, "stdout={} stderr={}", r.stdout, r.stderr);
    assert!(r.stderr.contains("claimed by aeon-7"), "{}", r.stderr);
    assert!(r.lc_calls.contains("show sp-c"), "{}", r.lc_calls);
}
