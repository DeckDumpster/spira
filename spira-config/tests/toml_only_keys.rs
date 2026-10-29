//! A key only `spira.toml` carries (`spira/conf.toml.d`) resolves through `cfg` like any other.

#[test]
fn cfg_parse_resolves_a_toml_only_key_against_the_complete_fixture() {
    let dir = testkit::TempDir::new("spira-config-toml-only");
    let toml = spira_config::process::fixture_toml(&dir, &[]);
    let _env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);
    assert_eq!(spira_config::process::cfg_parse::<u32>("SPIRA_HOTFIX_ALERT_HOURS"), Ok(4));
    assert_eq!(spira_config::process::cfg_parse::<u32>("SPIRA_NO_PROGRESS_BREAKER_CLAIMS"), Ok(4));
    assert!(
        spira_config::process::cfg("SPIRA_NOT_A_REGISTERED_KEY").unwrap_err().contains("not a registered config key"),
        "positive control: an unregistered key is still refused"
    );
}

#[test]
fn check_config_refuses_a_listed_key_the_registry_lacks_and_passes_a_registered_one() {
    let dir = testkit::TempDir::new("spira-config-check-keys");
    let toml = spira_config::process::fixture_toml(&dir, &[]);
    let _env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);
    assert!(spira_config::process::check_config(&["SPIRA_HOTFIX_ALERT_HOURS"]).is_ok(), "positive control");
    let err = spira_config::process::check_config(&["SPIRA_HOTFIX_ALERT_HOURS", "SPIRA_NOT_A_REGISTERED_KEY"])
        .unwrap_err();
    assert!(err.contains("SPIRA_NOT_A_REGISTERED_KEY") && !err.contains("SPIRA_HOTFIX_ALERT_HOURS"), "{err}");
}
