//! `spira-config repo set` / `repo rm` through the binary a caller runs, against a throwaway repo-map.

use std::path::Path;
use std::process::{Command, Output};

fn run(map: &Path, args: &[&str]) -> Output {
    let dir = map.parent().unwrap();
    let toml = spira_config::process::fixture_toml(dir, &[("SPIRA_REPO_MAP", map.to_str().unwrap())]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spira");
    Command::new(env!("CARGO_BIN_EXE_spira-config"))
        .env_clear()
        .env("SPIRA_TOML", toml)
        .env("SPIRA_HOME", home)
        .args(args)
        .output()
        .unwrap()
}

fn field(map: &Path, name: &str, col: &str) -> String {
    String::from_utf8_lossy(&run(map, &["repo", "field", name, col]).stdout)
        .trim()
        .to_string()
}

fn fixture() -> (testkit::TempDir, std::path::PathBuf) {
    let d = testkit::TempDir::new("repo-set-cli");
    let map = d.path().join("repo-map");
    std::fs::write(
        &map,
        "fx | /srv/fx | queue.local | local/main | fmt | bash gate.sh | plan\n",
    )
    .unwrap();
    (d, map)
}

#[test]
fn set_gate_to_empty_reads_back_empty_through_repo_field() {
    let (_d, map) = fixture();
    assert_eq!(field(&map, "fx", "gate"), "bash gate.sh");
    let o = run(&map, &["repo", "set", "fx", "gate", ""]);
    assert!(o.status.success(), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stdout).contains("bash gate.sh"));
    assert_eq!(field(&map, "fx", "gate"), "");
    assert_eq!(field(&map, "fx", "format"), "fmt");
}

#[test]
fn set_with_an_unknown_field_refuses_and_changes_nothing() {
    let (_d, map) = fixture();
    let o = run(&map, &["repo", "set", "fx", "colour", "red"]);
    assert!(!o.status.success());
    assert_eq!(field(&map, "fx", "gate"), "bash gate.sh");
}

#[test]
fn rm_unregisters_the_repo() {
    let (_d, map) = fixture();
    assert!(run(&map, &["repo", "rm", "fx"]).status.success());
    assert_eq!(field(&map, "fx", "path"), "");
    assert!(!run(&map, &["repo", "rm", "fx"]).status.success());
}
