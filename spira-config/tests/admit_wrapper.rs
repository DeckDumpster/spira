//! `spira-admit` as cargo's rustc wrapper: a compile it cannot admit fails the build, and an
//! admitted compile is a real one — a planted error still fails.

use std::path::Path;
use std::process::{Command, Output};

const VALID: &str = "pub fn answer() -> u32 { 42 }\n";
const PLANTED: &str = "pub fn answer() -> u32 { \"not a u32\" }\n";

fn fixture_crate(dir: &Path, lib: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"admit-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), lib).unwrap();
}

/// `cargo check` of a fresh fixture crate through the wrapper, in an explicit environment:
/// no user cargo config, no inherited admission, `SPIRA_TOML` only when `toml` names one.
fn cargo_check(tag: &str, lib: &str, toml: Option<&Path>) -> Output {
    cargo_check_as(tag, lib, toml, None)
}

/// [`cargo_check`] with `SPIRA_ADMISSION` set to `inherit` (a gate's token takes a lease at once).
fn cargo_check_as(tag: &str, lib: &str, toml: Option<&Path>, inherit: Option<&str>) -> Output {
    let tmp = testkit::TempDir::new(tag);
    let krate = tmp.join("crate");
    fixture_crate(&krate, lib);
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut c = Command::new(&cargo);
    c.env_clear()
        .current_dir(&krate)
        .args(["check", "--offline", "--quiet"])
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", tmp.path())
        .env("CARGO_HOME", tmp.join("cargo-home"))
        .env("CARGO_TARGET_DIR", tmp.join("target"))
        .env("RUSTC_WRAPPER", env!("CARGO_BIN_EXE_spira-admit"))
        .env("SPIRA_HOME", Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira"))
        .env("SPIRA_ADMIT_ROUND_HOLD_SECS", "0");
    for k in ["RUSTUP_HOME", "RUSTUP_TOOLCHAIN"] {
        if let Ok(v) = std::env::var(k) {
            c.env(k, v);
        }
    }
    if let Some(rustc) = Path::new(&cargo).parent().map(|d| d.join("rustc")).filter(|p| p.is_file()) {
        c.env("RUSTC", rustc);
    }
    if let Some(t) = toml {
        c.env("SPIRA_TOML", t);
    }
    if let Some(i) = inherit {
        c.env("SPIRA_ADMISSION", i);
    }
    c.output().expect("cargo runs")
}

fn admitted_config(tmp: &Path) -> std::path::PathBuf {
    let run = tmp.join("run");
    std::fs::create_dir_all(&run).unwrap();
    spira_config::process::fixture_toml(tmp, &[("SPIRA_RUN", &run.display().to_string())])
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_compile_the_wrapper_cannot_admit_fails_the_build() {
    let o = cargo_check("admit-refuse-valid", VALID, None);
    let err = stderr(&o);
    assert!(!o.status.success(), "cargo reported success for a compile spira-admit could not admit: {err}");
    assert!(err.contains("spira-admit: cannot admit this compile") && err.contains("SPIRA_TOML"), "{err}");
}

#[test]
fn a_planted_error_fails_the_build_when_the_wrapper_refuses() {
    let o = cargo_check("admit-refuse-planted", PLANTED, None);
    let err = stderr(&o);
    assert!(!o.status.success(), "a planted type error passed: {err}");
    assert!(err.contains("spira-admit: cannot admit this compile") && !err.contains("E0308"), "the wrapper refuses before rustc runs: {err}");
}

#[test]
fn a_gate_compile_the_wrapper_cannot_admit_fails_the_build() {
    let o = cargo_check_as("admit-refuse-gate", VALID, None, Some("gate"));
    let err = stderr(&o);
    assert!(!o.status.success() && err.contains("spira-admit: cannot admit this compile"), "{err}");
}

#[test]
fn an_admitted_compile_compiles() {
    let tmp = testkit::TempDir::new("admit-ok-cfg");
    let ok = cargo_check("admit-ok-valid", VALID, Some(&admitted_config(&tmp)));
    assert!(ok.status.success(), "positive control: a valid crate through the admitted wrapper: {}", stderr(&ok));
    let bad = cargo_check("admit-ok-planted", PLANTED, Some(&admitted_config(&tmp)));
    let err = stderr(&bad);
    assert!(!bad.status.success() && err.contains("E0308"), "an admitted compile with a planted type error: {err}");
}
