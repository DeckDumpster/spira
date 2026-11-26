#[macro_use]
extern crate spira_test_id;
use std::process::Command;

fn function(name: &str) -> String {
    let s = round_vm::run::REMOTE_SCRIPT;
    let f = s.find(&format!("{name}() {{")).unwrap();
    s[f..f + s[f..].find("\n}\n").unwrap() + 3].to_string()
}

const LIB: &str = "#[cfg(test)]\n#[macro_use]\nextern crate spira_test_id;\n#[cfg(test)]\nmod t {\n#[test_id(t0bbbbb)]\n#[test]\nfn keep() {}\n#[test_id(t0aaaaa)]\n#[test]\nfn drop() {}\n}\n";

const RENAMED_LIB: &str = "#[cfg(test)]\n#[macro_use]\nextern crate spira_test_id;\n#[cfg(test)]\nmod t {\n#[test_id(t0bbbbb)]\n#[test]\nfn keep() {}\n}\n#[cfg(test)]\nmod other {\n#[test_id(t0aaaaa)]\n#[test]\nfn dropped() {}\n}\n";

fn test_id_dep() -> String {
    let macro_crate = std::env::current_dir().unwrap().join("../spira-test-id").canonicalize().unwrap();
    format!("[dev-dependencies]\nspira-test-id = {{ path = {:?} }}\n", macro_crate.display().to_string())
}

fn tiny_crate() -> (testkit::TempDir, String) {
    tiny_crate_with(LIB)
}

fn tiny_crate_with(lib: &str) -> (testkit::TempDir, String) {
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap()));
    if !std::path::Path::new(&cargo_home).join("bin/cargo-nextest").exists() {
        panic!("cargo-nextest is required (round-vm/ami-setup.sh installs it) and absent from {cargo_home}/bin");
    }
    let tmp = testkit::TempDir::new("deny-list-real");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), format!("[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[features]\nintegration = []\n[profile.unittest]\ninherits = \"dev\"\n{}", test_id_dep())).unwrap();
    std::fs::write(dir.join("src/lib.rs"), lib).unwrap();
    std::fs::write(dir.join("rust-toolchain.toml"), include_str!("../../rust-toolchain.toml")).unwrap();
    (tmp, cargo_home)
}

fn bash_in(dir: &std::path::Path, cargo_home: &str, script: &str, arg: &str) -> String {
    let (code, err) = bash_in_code(dir, cargo_home, script, arg);
    assert_eq!(code, Some(0), "{err}");
    err
}

fn bash_in_code(dir: &std::path::Path, cargo_home: &str, script: &str, arg: &str) -> (Option<i32>, String) {
    bash_in_code2(dir, cargo_home, script, arg, "")
}

fn bash_in_code2(dir: &std::path::Path, cargo_home: &str, script: &str, arg: &str, arg2: &str) -> (Option<i32>, String) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script).arg("bash").arg(arg).arg(arg2).current_dir(dir).env_clear().env("HOME", std::env::var("HOME").unwrap()).env("PATH", format!("{cargo_home}/bin:/usr/local/bin:/usr/bin:/bin"));
    for k in ["RUSTUP_TOOLCHAIN", "RUSTUP_HOME"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let out = cmd.output().unwrap();
    (out.status.code(), String::from_utf8_lossy(&out.stderr).to_string())
}

fn run_tiny(list: &str) -> String {
    let (tmp, cargo_home) = tiny_crate();
    let (code, log) = run_in(&tmp, &cargo_home, list);
    assert_eq!(code, Some(0), "{log}");
    log
}

fn run_in(tmp: &testkit::TempDir, cargo_home: &str, list: &str) -> (Option<i32>, String) {
    let dir = tmp.path().to_path_buf();
    let deny = spira_config::unit_tier::DenyList::parse(list).unwrap();
    let script = format!(
        "export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\n{}\n{}\n{}\n{}\n{}\nwrite_unit_nextest_config 10 1 60 2 \"$PWD/round-nextest.toml\" \"$1\"\ntest_env() {{ : > entered-test-env; unit_env \"$@\"; }}\nunit_cargo || exit 90\nunit_nextest > run.log 2>&1\nrc=$?\ncat run.log >&2\nprintf '%s' \"$2\" > round-deny-list\nunit_registry_skips round-deny-list run.log\nexit $rc\n",
        function("unit_registry_skips"),
        function("unit_env"),
        function("write_unit_nextest_config"),
        function("unit_cargo"),
        function("unit_nextest").replace("$HOME/round-nextest.toml", "$PWD/round-nextest.toml"),
    );
    let out = bash_in_code2(&dir, cargo_home, &script, &deny.config_filter_line(), list);
    assert!(dir.join("entered-test-env").exists(), "the round's unit run goes through test_env: {}", out.1);
    out
}

#[test_id(tr9w5nw)]
#[test]
fn a_denied_unit_test_is_absent_from_the_nextest_run_and_the_others_pass() {
    let log = run_tiny("disabled\ttest:t0aaaaa\tsp-x\t-\n");
    assert!(log.contains("PASS") && log.contains("t::keep"), "{log}");
    assert!(log.contains("SKIPPED (registry disabled) test:t0aaaaa"), "{log}");
    assert!(!names_in_pass_or_fail(&log, "t::drop"), "{log}");
    assert_eq!(unit_stream_reds(&log), "", "a SKIP line is no red: {log}");
}

#[test_id(tqrnsiv)]
#[test]
fn a_deny_row_naming_no_test_says_it_matched_none() {
    let log = run_tiny("disabled\ttest:t0zzzzz\tsp-x\t-\n");
    assert!(log.contains("registry row test:t0zzzzz matched no test in the run"), "{log}");
    assert!(!log.contains("SKIPPED"), "{log}");
}

fn names_in_pass_or_fail(log: &str, test: &str) -> bool {
    log.lines().any(|l| l.contains(test) && (l.trim_start().starts_with("PASS") || l.contains("FAIL")))
}

fn unit_stream_reds(log: &str) -> String {
    let (tmp, _) = tiny_crate();
    std::fs::write(tmp.path().join("run.log"), log).unwrap();
    let script = format!("{}\nunit_stream 9 prog < run.log 2>&1 >/dev/null | grep UNIT-TEST\n", function("unit_stream"));
    let out = Command::new("bash").arg("-c").arg(script).current_dir(tmp.path()).output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test_id(tvffwxx)]
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
    test_registry::cli::run(&argv, now, &cfg, &|p| test_registry::open(p), &mut runtime::ports::SeededRng::new(0))
}

fn resolve(path: &str, at: i64) -> String {
    test_registry::deny_list(&test_registry::open(path).unwrap(), at)
}

/// The gate's composed test phase: the unit command `compose` builds and the nextest config the
/// engine writes for the same list, run in the tiny crate.
fn gate_unit_run(list: &str) -> String {
    let (tmp, cargo_home) = tiny_crate();
    gate_unit_run_in(&tmp, &cargo_home, list)
}

fn gate_unit_run_in(tmp: &testkit::TempDir, cargo_home: &str, list: &str) -> String {
    let dir = tmp.path().to_path_buf();
    let deny = spira_config::unit_tier::DenyList::parse(list).unwrap();
    let cmds = gate::compose::unit_test_commands(&["tiny".to_string()], gate::compose::Targets { lib: true, bin: false, test: false }, &deny);
    let unit = cmds.iter().find_map(|c| c.find("cargo nextest run").map(|i| &c[i..])).unwrap_or_else(|| panic!("no nextest command: {cmds:?}"));
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join(gate::compose::NEXTEST_CONFIG), spira_config::unit_tier::nextest_config((10, 1), (60, 2), &deny)).unwrap();
    let script = format!("export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\n{unit}\n");
    bash_in(&dir, cargo_home, &script, "")
}

#[test_id(t9l2a8v)]
#[test]
fn a_disabled_row_excludes_exactly_that_test_from_a_gate_run_and_a_round_run() {
    let reg = testkit::TempDir::new("deny-list-registry");
    let path = reg.path().join("registry.sqlite").display().to_string();
    let o = registry(&path, &["disable", "--test", "test:t0aaaaa", "--bead", "sp-x", "--reason", "r", "--actor", "a"], 1000);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let list = resolve(&path, 1100);
    assert!(list.contains("disabled\ttest:t0aaaaa"), "{list}");
    for (who, log) in [("gate", gate_unit_run(&list)), ("round", run_tiny(&list))] {
        assert!(log.contains("PASS") && log.contains("t::keep"), "{who}: {log}");
        assert!(!names_in_pass_or_fail(&log, "t::drop"), "{who}: the disabled test does not run: {log}");
    }

    let o = registry(&path, &["activate", "--test", "test:t0aaaaa", "--reason", "fixed", "--actor", "a"], 1200);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let list = resolve(&path, 1700);
    for (who, log) in [("gate", gate_unit_run(&list)), ("round", run_tiny(&list))] {
        assert!(log.contains("t::keep") && log.contains("t::drop"), "{who}: an activated row runs both: {log}");
    }
}

#[test_id(tjla9pj)]
#[test]
fn the_digest_the_registry_prints_is_the_one_the_gate_and_the_round_record() {
    assert_eq!(test_registry::digest("abc"), "ba7816bf8f01");
    assert_eq!(&gate::key::sha256_hex(b"abc")[..12], "ba7816bf8f01");
}

#[test_id(td7bmaa)]
#[test]
fn a_test_red_once_on_the_round_tree_is_disabled_through_the_rounds_own_path_and_the_next_pass_does_not_run_it() {
    let flaky = "#[cfg(test)]\n#[macro_use]\nextern crate spira_test_id;\n#[cfg(test)]\nmod t {\n#[test_id(t0bbbbb)]\n#[test] fn keep() {}\n#[test_id(t0aaaaa)]\n#[test] fn drop() {\nlet m = std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"drop.mark\");\nif !m.exists() { std::fs::write(&m, \"\").unwrap(); panic!(\"flaked\"); }\n}\n}\n";
    let (tmp, cargo_home) = tiny_crate_with(flaky);
    let (code, log) = run_in(&tmp, &cargo_home, "");
    assert_ne!(code, Some(0), "the first pass is red: {log}");
    std::fs::write(tmp.path().join("red.log"), &log).unwrap();
    let script = format!("{}\nnextest_reds red.log | awk '{{ print $1, $3, $2 }}'\n", function("nextest_reds"));
    let out = Command::new("bash").arg("-c").arg(script).current_dir(tmp.path()).output().unwrap();
    let failed = batcher::unitattr::failing_lines(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(failed, ["FAIL tiny t::drop::t0aaaaa"], "{log}");
    let (code, log) = run_in(&tmp, &cargo_home, "");
    assert_eq!(code, Some(0), "the unchanged tree reruns green: {log}");

    let ids = batcher::drive::flip_ids(&failed).unwrap();
    assert_eq!(ids, ["test:t0aaaaa"]);
    let reg = testkit::TempDir::new("flip-registry");
    let path = reg.path().join("registry.sqlite").display().to_string();
    let results = reg.path().join("results");
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("deny-list"), resolve(&path, 1000)).unwrap();
    let rows = batcher_cut::io::disable_flips_with(
        &mut |_| Ok("sp-flip1".to_string()),
        &mut |test, bead, reason| {
            let o = registry(&path, &["disable", "--json", "--test", test, "--bead", bead, "--reason", reason, "--actor", "batcher"], 1000);
            assert_eq!(o.code, 0, "{}", o.stderr);
            batcher_cut::io::parse_disabled_row(&o.stdout)
        },
        &results,
        "spira/batcher-attr/tiny-1",
        &ids,
    )
    .unwrap();
    assert_eq!((rows[0].test.as_str(), rows[0].state.as_str(), rows[0].bead.as_str()), ("test:t0aaaaa", "disabled", "sp-flip1"));
    let held = resolve(&path, 1100);
    assert!(held.contains("disabled\ttest:t0aaaaa\tsp-flip1"), "{held}");
    let round_list = batcher_cut::vm::DenySnapshot::from_pass(&results).unwrap().text;
    assert_eq!(round_list.lines().skip(1).collect::<Vec<_>>(), held.lines().skip(1).collect::<Vec<_>>());

    std::fs::remove_file(tmp.path().join("drop.mark")).unwrap();
    let (code, log) = run_in(&tmp, &cargo_home, &round_list);
    assert_eq!(code, Some(0), "{log}");
    assert!(log.contains("t::keep") && !names_in_pass_or_fail(&log, "t::drop"), "the re-certifying pass does not run the disabled test: {log}");
}

const FLAKY: &str = "#[macro_use]\nextern crate spira_test_id;\n#[test_id(t0ddddd)]\n#[test]\nfn steady() {}\n#[test_id(t0ccccc)]\n#[test]\nfn flaked() { panic!(\"flaked\"); }\n";

const FLAKY_RENAMED: &str = "#[macro_use]\nextern crate spira_test_id;\n#[test_id(t0ddddd)]\n#[test]\nfn steady() {}\nmod moved {\n#[test_id(t0ccccc)]\n#[test]\nfn flaked_again() { panic!(\"flaked\"); }\n}\n";

fn member_workspace(flaky: &str) -> (testkit::TempDir, String) {
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap()));
    let tmp = testkit::TempDir::new("integration-flip-real");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("member/src")).unwrap();
    std::fs::create_dir_all(dir.join("member/tests")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\nmembers = [\"member\"]\n").unwrap();
    std::fs::write(dir.join("member/Cargo.toml"), format!("[package]\nname = \"memb\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{}", test_id_dep())).unwrap();
    std::fs::write(dir.join("member/src/lib.rs"), "").unwrap();
    std::fs::write(dir.join("member/tests/flaky.rs"), flaky).unwrap();
    std::fs::write(dir.join("rust-toolchain.toml"), include_str!("../../rust-toolchain.toml")).unwrap();
    (tmp, cargo_home)
}

fn integration_run(dir: &std::path::Path, cargo_home: &str, skips: &str) -> (Option<i32>, String) {
    let script = format!("export CARGO_HOME=\"$2\" RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\ncargo test --offline --test '*'{skips} > test.log 2>&1\nrc=$?\ncargo metadata --no-deps --format-version 1 > meta.json\nexit $rc\n");
    let (code, err) = bash_in_code2(dir, cargo_home, &script, "", cargo_home);
    (code, format!("{err}{}", std::fs::read_to_string(dir.join("test.log")).unwrap_or_default()))
}

#[test_id(tbfle4a)]
#[test]
fn an_integration_test_red_in_a_member_crate_is_named_by_its_id_through_real_cargo() {
    let (tmp, cargo_home) = member_workspace(FLAKY);
    let dir = tmp.path();
    let (code, log) = integration_run(dir, &cargo_home, "");
    assert_ne!(code, Some(0), "control: the test failed: {log}");
    assert!(log.contains("flaked::t0ccccc ... FAILED"), "{log}");
    let reds = Command::new("bash").arg("-c").arg(format!("{}\ncargo_test_reds test.log\n", function("cargo_test_reds"))).current_dir(dir).output().unwrap();
    let failed = batcher::unitattr::failing_lines(&String::from_utf8_lossy(&reds.stdout));
    assert_eq!(batcher::drive::flip_ids(&failed).unwrap(), ["test:t0ccccc"], "{failed:?}\n{log}");
}

fn deny_by_id(id: &str, bead: &str) -> (testkit::TempDir, String, String) {
    let reg = testkit::TempDir::new("deny-by-id");
    let path = reg.path().join("registry.sqlite").display().to_string();
    let test = format!("test:{id}");
    let o = registry(&path, &["disable", "--test", &test, "--bead", bead, "--reason", "r", "--actor", "a"], 1000);
    assert_eq!(o.code, 0, "{}", o.stderr);
    let list = resolve(&path, 1100);
    (reg, path, list)
}

#[test_id(t0r1n2m)]
#[test]
fn a_unit_test_disabled_by_id_stays_out_of_the_gate_and_round_runs_after_a_rename_and_a_move() {
    let (_reg, path, list) = deny_by_id("t0aaaaa", "sp-x");
    let (tmp, cargo_home) = tiny_crate();
    for (who, log) in [("gate", gate_unit_run_in(&tmp, &cargo_home, &list)), ("round", run_in(&tmp, &cargo_home, &list).1)] {
        assert!(log.contains("PASS") && log.contains("t::keep"), "{who}: {log}");
        assert!(!names_in_pass_or_fail(&log, "drop"), "{who}: drop is absent: {log}");
    }

    std::fs::write(tmp.path().join("src/lib.rs"), RENAMED_LIB).unwrap();
    let o = registry(&path, &["clear-landed", "--bead", "sp-renamer", "--actor", "batcher"], 1200);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(resolve(&path, 1300), list.replace("at=1100", "at=1300"), "no row cleared");
    for (who, log) in [("gate", gate_unit_run_in(&tmp, &cargo_home, &list)), ("round", run_in(&tmp, &cargo_home, &list).1)] {
        assert!(log.contains("PASS") && log.contains("t::keep"), "{who}: {log}");
        assert!(!names_in_pass_or_fail(&log, "dropped"), "{who}: the renamed, moved test is still absent: {log}");
    }
    let (code, log) = run_in(&tmp, &cargo_home, "");
    assert_eq!(code, Some(0), "{log}");
    assert!(names_in_pass_or_fail(&log, "other::dropped::t0aaaaa"), "control: without the row the renamed test runs: {log}");
}

fn integration_nextest_run(dir: &std::path::Path, cargo_home: &str, list: &str) -> (Option<i32>, String) {
    let deny = spira_config::unit_tier::DenyList::parse(list).unwrap();
    std::fs::write(dir.join("nx.toml"), spira_config::unit_tier::nextest_config((10, 1), (60, 2), &deny)).unwrap();
    let script = "export CARGO_HOME=\"$2\" RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\ncargo nextest run --config-file nx.toml --no-fail-fast --offline --test '*' > test.log 2>&1\nrc=$?\nexit $rc\n";
    let (code, err) = bash_in_code2(dir, cargo_home, script, "", cargo_home);
    (code, format!("{err}{}", std::fs::read_to_string(dir.join("test.log")).unwrap_or_default()))
}

#[test_id(t0i2n3t)]
#[test]
fn an_integration_test_disabled_by_id_is_skipped_by_nextest_after_a_rename() {
    let (_reg, _path, list) = deny_by_id("t0ccccc", "sp-x");
    let (tmp, cargo_home) = member_workspace(FLAKY);
    let (code, log) = integration_nextest_run(tmp.path(), &cargo_home, "");
    assert_ne!(code, Some(0), "control: red with no deny row: {log}");
    assert!(log.contains("FAIL") && log.contains("flaked"), "{log}");
    let (code, log) = integration_nextest_run(tmp.path(), &cargo_home, &list);
    assert_eq!(code, Some(0), "{log}");
    assert!(log.lines().any(|l| l.trim_start().starts_with("SKIP") && l.ends_with("::t0ccccc")), "nextest reports the denied test skipped: {log}");

    std::fs::write(tmp.path().join("member/tests/flaky.rs"), FLAKY_RENAMED).unwrap();
    let (code, log) = integration_nextest_run(tmp.path(), &cargo_home, &list);
    assert_eq!(code, Some(0), "{log}");
    assert!(log.contains("steady") && !log.contains("FAIL"), "{log}");
}
