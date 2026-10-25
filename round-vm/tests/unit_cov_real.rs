use std::process::Command;

fn function(name: &str) -> String {
    let s = round_vm::run::REMOTE_SCRIPT;
    let f = s.find(&format!("{name}() {{")).unwrap();
    s[f..f + s[f..].find("\n}\n").unwrap() + 3].to_string()
}

#[test]
fn the_unit_command_runs_under_a_real_cargo_llvm_cov_and_yields_line_coverage() {
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap()));
    if !std::path::Path::new(&cargo_home).join("bin/cargo-llvm-cov").exists() {
        assert!(std::env::var_os("SPIRA_REQUIRE_LLVM_COV").is_none(), "cargo-llvm-cov is required and absent");
        eprintln!("skipped: no cargo-llvm-cov in {cargo_home}/bin");
        return;
    }
    let tmp = testkit::TempDir::new("unit-cov-real");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[profile.unittest]\ninherits = \"dev\"\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn f(x: u32) -> u32 { x + 1 }\n#[cfg(test)]\nmod t { #[test] fn a() { assert_eq!(super::f(1), 2); } }\n").unwrap();
    let script = format!(
        "set -euo pipefail\nexport CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\n{}\n{}\nunit_cov_init\ntest -n \"$unit_cov_env\"\nunit_cargo --no-run\nunit_cargo --no-fail-fast\neval \"$unit_cov_env\"\ncargo llvm-cov report --profile unittest --json --summary-only --output-path out.json\n",
        function("unit_cargo"),
        function("unit_cov_init"),
    );
    // A clean environment: when this runs inside a coverage-instrumented unit run, the outer
    // run's CARGO_TARGET_DIR / LLVM_PROFILE_FILE / CARGO_LLVM_COV_* must not leak in.
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .current_dir(&dir)
        .env_clear()
        .env("HOME", std::env::var("HOME").unwrap())
        .env("PATH", format!("{cargo_home}/bin:/usr/local/bin:/usr/bin:/bin"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let j: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("out.json")).unwrap()).unwrap();
    assert!(j["data"][0]["totals"]["lines"]["count"].as_u64().unwrap() > 0, "{j}");
}
