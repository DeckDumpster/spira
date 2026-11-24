//! `seed_prod_instance` through the real `spira-config` binary it shells to, on a throwaway root.

#[macro_use]
extern crate spira_test_id;
use install::seed_instance::seed_prod_instance;
use std::path::Path;
use std::process::Command;

fn instance_in(toml: &Path) -> String {
    let o = Command::new(testkit::bin_exe("spira-config")).args(["get", "spira.instance"]).arg(toml).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

// covers: UC-instance-lifecycle-29
#[test_id(tfcu7w7)]
#[test]
fn a_non_prod_install_seeds_the_instance_once_into_the_prod_root_and_keeps_what_was_there() {
    let release = testkit::TempDir::new("seed-release");
    let bin = release.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(testkit::bin_exe("spira-config"), bin.join("spira-config")).unwrap();
    let scratch = testkit::TempDir::new("seed-own");
    let own = spira_config::process::fixture_toml(scratch.path(), &[]);
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let _env = testkit::env(&[("PATH", Some(bin.to_str().unwrap())), ("SPIRA_RELEASE", Some(release.path().to_str().unwrap())), ("SPIRA_TOML", Some(own.to_str().unwrap())), ("SPIRA_HOME", Some(home.to_str().unwrap()))]);

    let fresh = testkit::TempDir::new("seed-fresh");
    let (conf, toml) = (fresh.path().join("spira.conf"), spira_config::toml_path_at(fresh.path()));
    assert!(!conf.exists() && !toml.exists());
    let said = seed_prod_instance(&conf, &toml, "dev-a", &home).expect("the first seed reports itself");
    assert!(said.contains("seeded") && said.contains("dev-a"), "{said}");
    assert_eq!(instance_in(&toml), "dev-a");
    assert!(!conf.exists(), "no legacy file appears beside the typed one");
    assert_eq!(seed_prod_instance(&conf, &toml, "dev-a", &home), None, "the second seed is silent");
    let again = std::fs::read_to_string(&toml).unwrap();
    assert_eq!(again.matches("dev-a").count(), 1, "{again}");

    let kept = testkit::TempDir::new("seed-kept");
    let (conf, toml) = (kept.path().join("spira.conf"), spira_config::toml_path_at(kept.path()));
    std::fs::write(&toml, "[spira]\nid_prefix = \"sp\"\nmax_dwarves = 4\n").unwrap();
    seed_prod_instance(&conf, &toml, "dev-b", &home).unwrap();
    let text = std::fs::read_to_string(&toml).unwrap();
    assert!(text.contains("max_dwarves = 4") && text.contains("id_prefix"), "{text}");
    assert_eq!(instance_in(&toml), "dev-b");

    let legacy = testkit::TempDir::new("seed-legacy");
    let (conf, toml) = (legacy.path().join("spira.conf"), spira_config::toml_path_at(legacy.path()));
    std::fs::write(&conf, "SPIRA_DB = /some/legacy/db\n").unwrap();
    seed_prod_instance(&conf, &toml, "dev-c", &home).unwrap();
    assert_eq!(instance_in(&toml), "dev-c");
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), "SPIRA_DB = /some/legacy/db\n", "the legacy file is never appended to");
}
