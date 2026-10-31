//! `queue` reads its keys against the registry of the release it runs in: a registry that
//! lacks one is refused at promotion (`check-config`).

use std::path::Path;
use std::process::{Command, Output};

const KEY: &str = "SPIRA_PUBLISH_SETTLE_DEFER_MAX";

fn run(args: &[&str], release: &Path, toml: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_queue"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("SPIRA_RELEASE", release)
        .env("SPIRA_TOML", toml)
        .output()
        .expect("the binary runs")
}

fn release_without(dir: &Path, key: &str) -> std::path::PathBuf {
    let release = spira_config::process::fixture_release(dir, false);
    let conf_d = release.join("spira/conf.d");
    std::fs::create_dir_all(&conf_d).unwrap();
    let tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira/conf.d");
    for e in std::fs::read_dir(tree).unwrap() {
        let e = e.unwrap();
        if e.file_name() != key {
            std::fs::copy(e.path(), conf_d.join(e.file_name())).unwrap();
        }
    }
    release
}

#[test]
fn check_config_refuses_a_registry_missing_a_key_the_binary_reads_and_passes_a_complete_one() {
    let dir = testkit::TempDir::new("queue-check-config");
    let toml = spira_config::process::fixture_toml(&dir, &[]);

    let full = spira_config::process::fixture_release(&dir, true);
    let out = run(&["check-config"], &full, &toml);
    assert!(out.status.success(), "positive control: {}", String::from_utf8_lossy(&out.stderr));

    let partial = release_without(&dir.join("partial"), KEY);
    let out = run(&["check-config"], &partial, &toml);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a registry without {KEY} passed: {stderr}");
    assert!(stderr.contains(KEY), "{stderr}");
}

#[test]
fn a_round_verb_does_not_refuse_for_a_registry_missing_the_defer_bound() {
    let dir = testkit::TempDir::new("queue-round-verb-config");
    let toml = spira_config::process::fixture_toml(&dir, &[]);
    let partial = release_without(&dir.join("partial"), KEY);
    let out = run(&["round", "abandon", "no-such-round"], &partial, &toml);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains(KEY), "abandon still resolves {KEY} eagerly: {stderr}");
}
