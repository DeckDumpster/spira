//! A reader that asks for config before any fixture, with no `SPIRA_TOML`, must not make the
//! fixture's own door fail afterwards: its own process, so no other test has resolved first.

#[test]
fn the_fixture_door_opens_after_an_early_reader_found_no_config() {
    {
        let _env = testkit::env(&[("SPIRA_TOML", None), ("SPIRA_HOME", None)]);
        assert!(spira_config::process::cfg("SPIRA_RUN").is_err());
    }
    spira_runtime::fixture::fixture_config_door();
    spira_runtime::fixture::fixture();
}
