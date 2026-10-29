//! The reconciler's no-progress breaker keys resolve through `process::cfg_parse`: a key
//! outside `spira/conf.d` is "not a registered config key" to a binary, whatever spira.toml says.

use std::path::Path;

#[test]
fn the_no_progress_breaker_keys_resolve_from_the_process_config() {
    let tmp = testkit::TempDir::new("cfg-breaker-keys");
    let toml = spira_config::process::fixture_toml(&tmp, &[]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira").display().to_string();
    let _env = testkit::env(&[("SPIRA_TOML", Some(&toml.display().to_string())), ("SPIRA_HOME", Some(&home))]);
    assert_eq!(spira_config::process::cfg_parse::<u32>("SPIRA_NO_PROGRESS_BREAKER_CLAIMS"), Ok(4));
    assert_eq!(spira_config::process::cfg_parse::<u32>("SPIRA_NO_PROGRESS_BREAKER_WINDOW_MINUTES"), Ok(180));
}
