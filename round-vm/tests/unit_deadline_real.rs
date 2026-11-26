#[macro_use]
extern crate spira_test_id;
use std::process::Command;

fn function(name: &str) -> String {
    let s = round_vm::run::REMOTE_SCRIPT;
    let f = s.find(&format!("{name}() {{")).unwrap();
    s[f..f + s[f..].find("\n}\n").unwrap() + 3].to_string()
}

// These build a tiny crate with real cargo: only real nextest shows which deadline each target gets.
fn tiny_crate(name: &str, files: &[(&str, &str)]) -> (testkit::TempDir, std::path::PathBuf) {
    let tmp = testkit::TempDir::new(name);
    let dir = tmp.path().to_path_buf();
    let manifest = "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[features]\nintegration = []\n[profile.unittest]\ninherits = \"dev\"\n";
    let fixed = [("Cargo.toml", manifest), ("rust-toolchain.toml", include_str!("../../rust-toolchain.toml"))];
    for (path, text) in fixed.iter().chain(files) {
        std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
        std::fs::write(dir.join(path), text).unwrap();
    }
    (tmp, dir)
}

/// `body` in a cleared environment in `dir`, after the round script's `unit_env` and config writer.
fn round_bash(dir: &std::path::Path, body: &str) -> std::process::Output {
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap()));
    if !std::path::Path::new(&cargo_home).join("bin/cargo-nextest").exists() {
        panic!("cargo-nextest is required (round-vm/ami-setup.sh installs it) and absent from {cargo_home}/bin");
    }
    let script = format!(
        "export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\n{}\n{}\n{body}",
        function("unit_env"),
        function("write_unit_nextest_config"),
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&script).current_dir(dir).env_clear().env("HOME", std::env::var("HOME").unwrap()).env("PATH", format!("{cargo_home}/bin:/usr/local/bin:/usr/bin:/bin"));
    for k in ["RUSTUP_TOOLCHAIN", "RUSTUP_HOME"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    cmd.output().unwrap()
}

#[test_id(t9m8913)]
#[test]
fn a_hung_unit_test_is_terminated_at_its_deadline_and_named_while_the_other_tests_still_run() {
    let (_tmp, dir) = tiny_crate(
        "unit-deadline-real",
        &[("src/lib.rs", "#[cfg(test)]\nmod t {\n#[test] fn hangs() { std::thread::sleep(std::time::Duration::from_secs(300)); }\n#[test] fn quick() {}\n#[test] fn quick_too() {}\n}\n")],
    );
    let body = format!(
        "{}\n{}\n{}\nwrite_unit_nextest_config 1 2 \"$PWD/round-nextest.toml\"\nunit_cargo || exit 90\nstart=$(date +%s)\nunit_nextest > run.log 2>&1\nrc=$?\necho rc=$rc secs=$(( $(date +%s) - start )) >&2\nunit_stream 3 progress < run.log\ncat run.log >&2\n",
        function("unit_cargo"),
        function("unit_nextest").replace("$HOME/round-nextest.toml", "$PWD/round-nextest.toml"),
        function("unit_stream"),
    );
    let out = round_bash(&dir, &body);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    let secs: u64 = err.split("secs=").nth(1).and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok()).expect(&err);
    assert!(!err.contains("rc=0 "), "a terminated test is a red run: {err}");
    assert!(secs < 120, "the run ended at the deadline, not after the test's own sleep: {err}");
    assert!(err.contains("round-vm: UNIT-TEST t::hangs TIMEOUT"), "{err}");
    assert!(err.contains("PASS") && err.matches("quick").count() >= 2, "the other tests ran to a result: {err}");
    assert_eq!(std::fs::read_to_string(dir.join("progress")).unwrap().trim_end().rsplit_once(' ').map(|x| x.1), Some("t::hangs"), "named in the progress record");
}

#[test_id(t2hscjd)]
#[test]
fn a_tests_target_outlives_the_unit_deadline_under_the_rounds_nextest_config() {
    let (_tmp, dir) = tiny_crate(
        "integration-deadline-real",
        &[("src/lib.rs", ""), ("tests/long.rs", "#[test] fn outlives_the_unit_deadline() { std::thread::sleep(std::time::Duration::from_secs(3)); }\n")],
    );
    let body = format!(
        "write_unit_nextest_config 1 2 \"$PWD/round-nextest.toml\"\nunit_env {}\n",
        spira_config::unit_tier::integration_words("\"$PWD/round-nextest.toml\"", Some("unittest"), "--workspace"),
    );
    let out = round_bash(&dir, &body);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("1 passed"), "the tests/ target ran to a pass: {err}");
}
