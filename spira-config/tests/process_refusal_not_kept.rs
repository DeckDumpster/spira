//! A refused resolution is not kept: the process door re-reads `$SPIRA_TOML` until one succeeds,
//! then pins that success. Own test binary, so the process-global cell starts empty.

#[macro_use]
extern crate spira_test_id;
#[test_id(tjs6jcy)]
#[test]
fn a_refused_resolution_is_not_kept_and_a_resolved_one_is() {
    {
        let _env = testkit::env(&[("SPIRA_TOML", None)]);
        let err = spira_config::process::config().expect_err("no SPIRA_TOML refuses");
        assert!(err.contains("SPIRA_TOML is not set"), "{err}");
    }

    let dir = testkit::TempDir::new("process-refusal-not-kept");
    let toml = spira_config::process::fixture_toml(dir.path(), &[]);
    let home = spira_config::resolve::locate_home_for_process().unwrap().display().to_string();
    {
        let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap())), ("SPIRA_HOME", Some(&home))]);
        if let Err(e) = spira_config::process::config() {
            panic!("the door stayed shut after a refusal: {e}");
        }
    }

    let _env = testkit::env(&[("SPIRA_TOML", None)]);
    assert!(spira_config::process::config().is_ok(), "a resolved config is pinned for the process");
}
