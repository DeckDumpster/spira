//! A process that derives its release pins it, and resolution then needs no release in the
//! environment. Own test binary, so the process-global cell starts empty.

#[macro_use]
extern crate spira_test_id;

use spira_config::process::{fixture_registry_home, fixture_toml, TOML_ENV};
use spira_config::registry::pin_for_process;

#[test_id(t9jx616)]
#[test]
fn a_pinned_release_serves_resolution_with_none_in_the_environment() {
    let dir = testkit::TempDir::new("registry-pin");
    let home = fixture_registry_home(dir.path());
    std::fs::write(
        home.join("conf.d/SPIRA_PIN_PROBE"),
        "TYPE=string\nGROUP=\nDOC=\nDEFAULT<<'SPIRA_CONF_DEFAULT_EOF'\n: \"${SPIRA_PIN_PROBE:=}\"\nSPIRA_CONF_DEFAULT_EOF\n",
    )
    .unwrap();

    let pinned = pin_for_process(&home).expect("the first pin loads");
    let toml = fixture_toml(dir.path(), &[("SPIRA_PIN_PROBE", "from-the-pin")]);

    {
        let _env = testkit::env(&[("SPIRA_HOME", None), ("SPIRA_RELEASE", None)]);
        let other = dir.path().join("other");
        let env = std::collections::BTreeMap::from([
            ("HOME".to_string(), dir.path().display().to_string()),
            (TOML_ENV.to_string(), toml.display().to_string()),
        ]);
        let resolved = match spira_config::resolve::resolve_for_process(&other, &other, &env) {
            Ok(r) => r,
            Err(e) => panic!("resolve: {e}"),
        };
        assert_eq!(resolved.values.get("SPIRA_PIN_PROBE").map(String::as_str), Some("from-the-pin"));
    }

    assert!(std::ptr::eq(pinned, pin_for_process(&home).expect("the same home pins again")));
    let another = dir.path().join("another");
    let err = pin_for_process(&another).expect_err("a different home is refused");
    assert!(err.contains(&home.display().to_string()) && err.contains(&another.display().to_string()), "{err}");
}
