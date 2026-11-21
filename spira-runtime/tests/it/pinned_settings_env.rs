//! `pinned_settings` resolves the complete fixture without editing the process environment, so
//! a test that runs after one of its callers reads no `SPIRA_TOML` or `SPIRA_HOME` it did not set.

use spira_runtime::tasks::d::acceptance::pinned_settings;

#[test_id(tqd0lv1)]
#[test]
fn pinned_settings_leaves_the_process_config_door_as_it_found_it() {
    let before = (std::env::var_os("SPIRA_TOML"), std::env::var_os("SPIRA_HOME"));
    pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((std::env::var_os("SPIRA_TOML"), std::env::var_os("SPIRA_HOME")), before);
}
