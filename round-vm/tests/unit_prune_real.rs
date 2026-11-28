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

/// A one-package workspace under a fresh HOME where the round keeps it: a library, a binary,
/// and an integration test, which is what makes the unit build link the plain binary.
fn tiny_round_work() -> (testkit::TempDir, std::path::PathBuf) {
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
    (tmp, dir)
}

/// Runs `steps` in `$HOME/round-work` with the round's own unit-build functions and `functions`.
fn in_round(home: &std::path::Path, functions: &[&str], steps: &str) -> std::process::Output {
    let cargo_home = std::env::var("CARGO_HOME").unwrap_or_else(|_| format!("{}/.cargo", std::env::var("HOME").unwrap()));
    let defs: String = ["unit_env", "unit_cargo"].iter().chain(functions).map(|n| function(n) + "\n").collect();
    let script = format!(
        "export CARGO_HOME={cargo_home:?} RUSTC_WRAPPER= SCCACHE_WEBDAV_ENDPOINT= SCCACHE_WEBDAV_KEY_PREFIX=\nexport CARGO_TARGET_DIR=\"$HOME/round-work/target\"\n{defs}cd \"$HOME/round-work\"\n{steps}"
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&script).current_dir(home).env_clear().env("HOME", home).env("PATH", format!("{cargo_home}/bin:/usr/local/bin:/usr/bin:/bin"));
    for k in ["RUSTUP_TOOLCHAIN", "RUSTUP_HOME"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    cmd.output().unwrap()
}

/// The dep-info under `deps/` of the unit the uplifted plain binary is a hard link to, found by
/// the shell so it is empty when there is none.
const PLAIN_BIN_DEP_INFO: &str = "for f in target/unittest/deps/tiny-*; do if [ \"$f\" -ef target/unittest/tiny ] && [ -e \"$f.d\" ]; then echo \"$f.d\"; fi; done";

#[test_id(tukzie1)]
#[test]
fn after_a_rebuild_under_new_metadata_and_a_prune_one_generation_of_each_target_remains_and_the_next_unit_build_compiles_nothing() {
    let (tmp, dir) = tiny_round_work();
    let out = in_round(
        tmp.path(),
        &["prune_superseded_unit"],
        "unit_cargo || exit 90\nsed -i 's/0.1.0/0.1.1/' Cargo.toml\nunit_cargo >/dev/null 2>&1 || exit 91\nls target/unittest/deps | grep -vc \"[.]\" >&2\nprune_superseded_unit\nunit_cargo 2>&1 | grep -c Compiling || true\n",
    );
    let deps = dir.join("target/unittest/deps");
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

#[test_id(tk33y4s)]
#[test]
fn the_plain_binary_keeps_its_dep_info_through_a_prune() {
    let (tmp, _dir) = tiny_round_work();
    let out = in_round(tmp.path(), &["prune_superseded_unit"], &format!("unit_cargo >&2 || exit 90\n[ ! -e target/unittest/tiny.d ] || exit 91\nprune_superseded_unit\n{PLAIN_BIN_DEP_INFO}\n"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).trim().ends_with(".d"), "the plain binary's dep-info is gone: {err}");
}

#[test_id(ttc409n)]
#[test]
fn a_binary_that_lost_its_dep_info_is_relinked_so_cargo_writes_it_again() {
    let (tmp, _dir) = tiny_round_work();
    let steps = format!(
        "unit_cargo >&2 || exit 90\nlost=$({PLAIN_BIN_DEP_INFO})\n[ -n \"$lost\" ] || exit 91\nrm \"$lost\"\nunit_cargo >&2 || exit 92\n[ ! -e \"$lost\" ] || exit 93\ndrop_unit_binaries_without_dep_info\nunit_cargo >&2 || exit 94\n{PLAIN_BIN_DEP_INFO}\n"
    );
    let out = in_round(tmp.path(), &["drop_unit_binaries_without_dep_info"], &steps);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).trim().ends_with(".d"), "the dep-info was not rewritten: {err}");
    assert!(err.contains("without dep-info"), "{err}");
}
