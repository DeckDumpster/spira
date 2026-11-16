use std::process::Command;

fn function(name: &str) -> String {
    let s = round_vm::run::REMOTE_SCRIPT;
    let f = s.find(&format!("{name}() {{")).unwrap();
    s[f..f + s[f..].find("\n}\n").unwrap() + 3].to_string()
}

fn run_tiny(list: &str) -> String {
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap()));
    if !std::path::Path::new(&cargo_home).join("bin/cargo-nextest").exists() {
        panic!("cargo-nextest is required (round-vm/ami-setup.sh installs it) and absent from {cargo_home}/bin");
    }
    let tmp = testkit::TempDir::new("deny-list-real");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[features]\nintegration = []\n[profile.unittest]\ninherits = \"dev\"\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "#[cfg(test)]\nmod t {\n#[test] fn keep() {}\n#[test] fn drop() {}\n}\n").unwrap();
    std::fs::write(dir.join("rust-toolchain.toml"), include_str!("../../rust-toolchain.toml")).unwrap();
    let deny = spira_config::unit_tier::DenyList::parse(list).unwrap();
    let script = format!(
        "export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\n{}\n{}\n{}\n{}\nwrite_unit_nextest_config 10 1 \"$PWD/round-nextest.toml\" \"$1\"\nunit_cargo || exit 90\nunit_nextest > run.log 2>&1\nrc=$?\ncat run.log >&2\nexit $rc\n",
        function("unit_env"),
        function("write_unit_nextest_config"),
        function("unit_cargo"),
        function("unit_nextest").replace("$HOME/round-nextest.toml", "$PWD/round-nextest.toml"),
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&script).arg("bash").arg(deny.config_filter_line()).current_dir(&dir).env_clear().env("HOME", std::env::var("HOME").unwrap()).env("PATH", format!("{cargo_home}/bin:/usr/local/bin:/usr/bin:/bin"));
    for k in ["RUSTUP_TOOLCHAIN", "RUSTUP_HOME"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let out = cmd.output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert_eq!(out.status.code(), Some(0), "{err}");
    err
}

#[test]
fn a_denied_unit_test_is_absent_from_the_nextest_run_and_the_others_pass() {
    let log = run_tiny("disabled\tunit:tiny t::drop\tsp-x\t-\n");
    assert!(log.contains("PASS") && log.contains("t::keep"), "{log}");
    assert!(!log.contains("t::drop"), "the denied test appears nowhere in the run: {log}");
}

#[test]
fn with_an_empty_deny_list_both_unit_tests_run() {
    let log = run_tiny("");
    assert!(log.contains("t::keep") && log.contains("t::drop"), "{log}");
}
