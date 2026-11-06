//! `spira-runtime check-config` against the complete fixture: every key the service reads is
//! registered and declared, and a value the service refuses fails the check, named.

use std::path::Path;
use std::process::Command;

/// The fixture leaves the store's socket undeclared, which the service refuses on its own.
const SERVICE: &[(&str, &str)] = &[("SPIRA_LC_SOCKET", "/fixture/run/lc.sock")];

fn check_config(declare: &[(&str, &str)]) -> (bool, String) {
    let dir = testkit::TempDir::new("runtime-check-config");
    let toml = spira_config::process::fixture_toml(&dir, &[SERVICE, declare].concat());
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let out = Command::new(env!("CARGO_BIN_EXE_spira-runtime")).arg("check-config").env("SPIRA_TOML", &toml).env("SPIRA_HOME", &home).env("HOME", dir.path()).output().expect("spira-runtime runs");
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn the_complete_fixture_passes_check_config() {
    let (ok, out) = check_config(&[]);
    assert!(ok && out.starts_with("ok "), "{out}");
}

#[test]
fn a_pool_cap_the_service_refuses_fails_check_config_naming_the_key() {
    let (ok, out) = check_config(&[("SPIRA_POOL_CAP_STORE", "0")]);
    assert!(!ok && out.contains("SPIRA_POOL_CAP_STORE"), "{out}");
}
