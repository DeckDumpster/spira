#[macro_use]
extern crate spira_test_id;
use std::process::Command;

fn function(name: &str) -> String {
    let s = round_vm::run::REMOTE_SCRIPT;
    let f = s.find(&format!("{name}() {{")).unwrap();
    s[f..f + s[f..].find("\n}\n").unwrap() + 3].to_string()
}

fn files_of(deps: &std::path::Path, ext: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(deps).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| if ext.is_empty() { !n.contains('.') } else { n.ends_with(ext) }).collect();
    names.sort();
    names
}

#[test_id(tukzie1)]
#[test]
fn after_a_rebuild_under_new_metadata_and_a_prune_one_generation_of_each_target_remains_and_the_next_unit_build_compiles_nothing() {
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap()));
    let tmp = testkit::TempDir::new("unit-prune-real");
    let home = tmp.path().to_path_buf();
    let dir = home.join("round-work");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[features]\nintegration = []\n[profile.unittest]\ninherits = \"dev\"\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn n() -> u32 { 1 }\n#[cfg(test)]\nmod t { #[test] fn a() {} }\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() { println!(\"{}\", tiny::n()); }\n").unwrap();
    std::fs::write(dir.join("tests/it.rs"), "#[test] fn b() { assert!(tiny::n() > 0); }\n").unwrap();
    std::fs::write(dir.join("rust-toolchain.toml"), include_str!("../../rust-toolchain.toml")).unwrap();
    let rustup_home = std::env::var("RUSTUP_HOME").unwrap_or_else(|_| format!("{}/.rustup", std::env::var("HOME").unwrap()));
    std::os::unix::fs::symlink(&rustup_home, home.join(".rustup")).unwrap();
    let script = format!(
        "export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\nexport CARGO_TARGET_DIR=\"$HOME/round-work/target\"\n{}\n{}\n{}\ncd \"$HOME/round-work\"\nunit_cargo || exit 90\nsed -i 's/0.1.0/0.1.1/' Cargo.toml\nunit_cargo >/dev/null 2>&1 || exit 91\nls \"$HOME/round-work/target/unittest/deps\" | grep -vc \"[.]\" >&2\nprune_superseded_unit\nunit_cargo 2>&1 | grep -c Compiling || true\n",
        function("unit_env"),
        function("unit_cargo"),
        function("prune_superseded_unit"),
    );
    let deps = dir.join("target/unittest/deps");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&script).current_dir(&home).env_clear().env("HOME", &home).env("PATH", format!("{cargo_home}/bin:/usr/local/bin:/usr/bin:/bin"));
    for k in ["RUSTUP_TOOLCHAIN", "RUSTUP_HOME"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let out = cmd.output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("removed"), "{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0", "the next unit build compiled something: {err}");
    let rlibs = files_of(&deps, ".rlib");
    assert!(rlibs.len() == 1 && rlibs[0].starts_with("libtiny-"), "{rlibs:?}");
    let exes = files_of(&deps, "");
    assert_eq!(exes.len(), 4, "the lib's and bin's unit tests, the bin and the integration test, one generation each: {exes:?}");
    let before: usize = err.lines().find_map(|l| l.trim().parse().ok()).expect("the executable count before the prune");
    assert!(before > 4, "the edit left no second generation to prune ({before}): {err}");
}
