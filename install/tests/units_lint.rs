// covers: systemd/*.service systemd/*.timer install/src/values.rs round-controller/Cargo.toml round-controller/src/main.rs
use std::path::{Path, PathBuf};
fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd")
}

fn raw(n: &str) -> String {
    std::fs::read_to_string(dir().join(n)).unwrap()
}

/// The program and first argument of a unit's ExecStart, the program by its basename.
fn exec_verb(unit: &str) -> Option<(String, String)> {
    let line = unit.lines().find_map(|l| l.strip_prefix("ExecStart="))?;
    let mut words = line.split_whitespace();
    let program = words.next()?.rsplit('/').next()?.to_string();
    Some((program, words.next().unwrap_or("").to_string()))
}

#[test]
fn the_sift_unit_execs_the_round_controllers_sifts_verb_which_the_workspace_builds() {
    let pair = |a: &str, b: &str| Some((a.to_string(), b.to_string()));
    assert_eq!(exec_verb("[Service]\nExecStart=@SPIRA_PROD_ROOT@/bin/batcher sifts\n"), pair("batcher", "sifts"), "positive control");
    assert_eq!(exec_verb(&raw("spira-sift.service")), pair("round-controller", "sifts"));
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../round-controller");
    let manifest = std::fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    assert!(manifest.contains("[[bin]]\nname = \"round-controller\""), "the workspace builds no round-controller binary");
    let main = std::fs::read_to_string(crate_dir.join("src/main.rs")).unwrap();
    assert!(main.contains("[\"sifts\"] =>"), "round-controller dispatches no sifts verb");
}

fn lone_pct(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("Exec")).filter(|l| l.replace("%%", "").contains('%') && l.replace("%%", "").split('%').skip(1).any(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))).count()
}

#[test]
fn exec_lines_carry_no_unescaped_percent_specifier() {
    assert!(lone_pct("ExecCondition=/bin/bash -c 'printf \"%s\"'") > 0, "positive control");
    assert_eq!(lone_pct(&raw("spira-ops.service")), 0);
}
