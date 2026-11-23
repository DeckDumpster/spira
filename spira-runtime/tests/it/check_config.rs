//! `spira-runtime check-config` against the complete fixture: every key the service reads is
//! registered and declared, and a value the service refuses fails the check, named.

use std::process::Command;

/// The fixture leaves the store's socket undeclared, which the service refuses on its own.
const SERVICE: &[(&str, &str)] = &[("SPIRA_LC_SOCKET", "fixture/run/lc.sock")];

fn check_config(declare: &[(&str, &str)]) -> (bool, String) {
    check_config_without(declare, &[])
}

fn check_config_without(declare: &[(&str, &str)], unset: &[&str]) -> (bool, String) {
    let dir = testkit::TempDir::new("runtime-check-config");
    let toml = spira_config::process::fixture_toml(&dir, &[SERVICE, declare].concat());
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let mut cmd = Command::new(testkit::bin_exe("spira-runtime"));
    cmd.arg("check-config")
        .env("SPIRA_TOML", &toml)
        .env("SPIRA_HOME", &home)
        .env("HOME", dir.path());
    for k in unset {
        cmd.env_remove(k);
    }
    let out = cmd.output().expect("spira-runtime runs");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test_id(tif6ezg)]
#[test]
fn the_complete_fixture_passes_check_config() {
    let (ok, out) = check_config(&[]);
    assert!(ok && out.starts_with("ok "), "{out}");
}

#[test_id(t6dl1ht)]
#[test]
fn a_pool_cap_the_service_refuses_fails_check_config_naming_the_key() {
    let (ok, out) = check_config(&[("SPIRA_POOL_CAP_STORE", "0")]);
    assert!(!ok && out.contains("SPIRA_POOL_CAP_STORE"), "{out}");
}

#[test_id(txfwvgu)]
#[test]
fn an_s3_backup_destination_without_the_key_in_the_environment_fails_check_config_naming_it() {
    let aws = [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_DEFAULT_REGION",
        "AWS_REGION",
    ];
    let (ok, out) = check_config_without(&[("SPIRA_LC_BACKUP_URL", "s3://b/p/")], &aws);
    assert!(
        !ok && out.contains("AWS_ACCESS_KEY_ID") && !out.starts_with("ok "),
        "{out}"
    );
}
