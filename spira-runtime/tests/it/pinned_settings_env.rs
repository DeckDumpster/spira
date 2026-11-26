//! `pinned_settings` resolves the complete fixture without editing the process environment, so
//! a test that runs after one of its callers reads no `SPIRA_TOML` or `SPIRA_HOME` it did not set.

use spira_runtime::tasks::d::acceptance::pinned_settings;

#[test_id(t3etx59)]
#[test]
fn pinned_settings_leaves_the_process_config_door_as_it_found_it() {
    let n = testkit::env_edits();
    let settings = pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!(testkit::env_edits(), n);
    assert!(!settings.run_dir.as_os_str().is_empty());
}
