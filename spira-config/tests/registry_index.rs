//! A process whose home carries a registry index resolves its config from `spira.toml` and that
//! one file, not from one file per key.

use spira_config::registry::{self, Registry};

#[test]
fn resolving_with_a_registry_index_opens_only_spira_toml_and_the_index() {
    let dir = testkit::TempDir::new("spira-config-registry-index");
    let home = spira_config::process::fixture_registry_home(dir.path());
    let toml = dir.join("spira.toml");
    std::fs::write(&toml, spira_config::process::complete_fixture_text()).unwrap();

    let before = registry::files_opened();
    let keys = Registry::load(&home).unwrap().keys.len();
    assert!(registry::files_opened() - before >= keys, "positive control: without an index every key file is opened");

    registry::write_index(&home).unwrap();
    let _env = testkit::env(&[("SPIRA_HOME", home.to_str()), ("SPIRA_TOML", toml.to_str())]);
    let before = registry::files_opened();
    let resolved = spira_config::process::config().unwrap();
    assert_eq!(registry::files_opened() - before, 2, "spira.toml and the index, not one file per key");
    assert_eq!(registry::for_process().unwrap().keys.len(), keys);
    assert!(resolved.values.contains_key("SPIRA_RUN"));
}
