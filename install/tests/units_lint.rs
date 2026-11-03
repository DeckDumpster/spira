// covers: systemd/*.service systemd/*.timer install/src/values.rs round-controller/Cargo.toml round-controller/src/main.rs
use std::path::{Path, PathBuf};
fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd")
}

fn raw(n: &str) -> String {
    std::fs::read_to_string(dir().join(n)).unwrap()
}

fn lone_pct(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with("Exec")).filter(|l| l.replace("%%", "").contains('%') && l.replace("%%", "").split('%').skip(1).any(|r| r.starts_with(|c: char| c.is_ascii_alphabetic()))).count()
}

#[test]
fn exec_lines_carry_no_unescaped_percent_specifier() {
    assert!(lone_pct("ExecCondition=/bin/bash -c 'printf \"%s\"'") > 0, "positive control");
    assert_eq!(lone_pct(&raw("spira-ops.service")), 0);
}
