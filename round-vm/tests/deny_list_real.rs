use std::process::Command;

fn function(name: &str) -> String {
    let s = round_vm::run::REMOTE_SCRIPT;
    let f = s.find(&format!("{name}() {{")).unwrap();
    s[f..f + s[f..].find("\n}\n").unwrap() + 3].to_string()
}

fn tiny_crate() -> (testkit::TempDir, String) {
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
    (tmp, cargo_home)
}

fn bash_in(dir: &std::path::Path, cargo_home: &str, script: &str, arg: &str) -> String {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script).arg("bash").arg(arg).current_dir(dir).env_clear().env("HOME", std::env::var("HOME").unwrap()).env("PATH", format!("{cargo_home}/bin:/usr/local/bin:/usr/bin:/bin"));
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

fn run_tiny(list: &str) -> String {
    let (tmp, cargo_home) = tiny_crate();
    let dir = tmp.path().to_path_buf();
    let deny = spira_config::unit_tier::DenyList::parse(list).unwrap();
    let script = format!(
        "export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\n{}\n{}\n{}\n{}\nwrite_unit_nextest_config 10 1 \"$PWD/round-nextest.toml\" \"$1\"\nunit_cargo || exit 90\nunit_nextest > run.log 2>&1\nrc=$?\ncat run.log >&2\nexit $rc\n",
        function("unit_env"),
        function("write_unit_nextest_config"),
        function("unit_cargo"),
        function("unit_nextest").replace("$HOME/round-nextest.toml", "$PWD/round-nextest.toml"),
    );
    bash_in(&dir, &cargo_home, &script, &deny.config_filter_line())
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

fn registry(path: &str, args: &[&str], now: i64) -> test_registry::cli::Outcome {
    let cfg = |k: &str| match k {
        test_registry::cli::PATH_KEY => Ok(path.to_string()),
        test_registry::cli::HOLD_KEY => Ok("600".to_string()),
        _ => Err(format!("{k} unknown")),
    };
    let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    test_registry::cli::run(&argv, now, &cfg, &|p| test_registry::open(p))
}

fn resolve(path: &str, at: i64) -> String {
    test_registry::deny_list(&test_registry::open(path).unwrap(), at)
}

/// The gate's composed test phase: the unit command `compose` builds and the nextest config the
/// engine writes for the same list, run in the tiny crate.
fn gate_unit_run(list: &str) -> String {
    let (tmp, cargo_home) = tiny_crate();
    let dir = tmp.path().to_path_buf();
    let deny = spira_config::unit_tier::DenyList::parse(list).unwrap();
    let cmds = gate::compose::unit_test_commands(&["tiny".to_string()], gate::compose::Targets { lib: true, bin: false, test: false }, &deny);
    let unit = cmds.iter().find_map(|c| c.find("cargo nextest run").map(|i| &c[i..])).unwrap_or_else(|| panic!("no nextest command: {cmds:?}"));
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join(gate::compose::NEXTEST_CONFIG), spira_config::unit_tier::nextest_config(10, 1, &deny)).unwrap();
    let script = format!("export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\n{unit}\n");
    bash_in(&dir, &cargo_home, &script, "")
}

#[test]
fn a_disabled_row_excludes_exactly_that_test_from_a_gate_run_and_a_round_run() {
    let reg = testkit::TempDir::new("deny-list-registry");
    let path = reg.path().join("registry.sqlite").display().to_string();
    let o = registry(&path, &["disable", "--test", "unit:tiny t::drop", "--bead", "sp-x", "--reason", "r", "--actor", "a"], 1000);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let list = resolve(&path, 1100);
    assert!(list.contains("disabled\tunit:tiny t::drop"), "{list}");
    for (who, log) in [("gate", gate_unit_run(&list)), ("round", run_tiny(&list))] {
        assert!(log.contains("PASS") && log.contains("t::keep"), "{who}: {log}");
        assert!(!log.contains("t::drop"), "{who}: the disabled test does not run: {log}");
    }

    let o = registry(&path, &["activate", "--test", "unit:tiny t::drop", "--reason", "fixed", "--actor", "a"], 1200);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let list = resolve(&path, 1700);
    for (who, log) in [("gate", gate_unit_run(&list)), ("round", run_tiny(&list))] {
        assert!(log.contains("t::keep") && log.contains("t::drop"), "{who}: an activated row runs both: {log}");
    }
}
