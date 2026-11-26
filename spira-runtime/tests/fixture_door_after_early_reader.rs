//! A reader that asks for config before any fixture, with no `SPIRA_TOML`, must not make the
//! fixture's own door fail afterwards: its own process, so no other test has resolved first.

#[macro_use]
extern crate spira_test_id;
#[test_id(tfkpdwd)]
#[test]
fn the_fixture_door_opens_after_an_early_reader_found_no_config() {
    {
        let _env = testkit::env(&[("SPIRA_TOML", None), ("SPIRA_HOME", None)]);
        assert!(spira_config::process::cfg("SPIRA_RUN").is_err());
    }
    let n = testkit::env_edits();
    spira_runtime::fixture::fixture_config_door();
    assert_eq!(testkit::env_edits(), n);
    spira_runtime::fixture::fixture();
}
