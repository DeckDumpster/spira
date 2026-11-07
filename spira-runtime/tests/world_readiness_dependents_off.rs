//! `World::readiness_dependents` with no control store configured: nothing is read, and the
//! carried revision comes back as it went in.

use spira_runtime::watchers::readiness_dependents::Pass;
use spira_runtime::world::World;

#[test]
fn without_a_control_store_the_pass_asks_the_store_nothing_and_keeps_the_revision() {
    let dir = testkit::TempDir::new("world-readiness-dependents-off");
    let toml = spira_config::process::fixture_toml(&dir, &[("SPIRA_LC_STORE_PATH", "")]);
    let _env = testkit::env(&[("SPIRA_TOML", toml.to_str())]);
    let world = World { lc_socket: dir.path().join("none.sock"), ..World::default() };
    let input = serde_json::to_string(&Pass { through: Some(41), changed: 0 }).unwrap();
    assert_eq!(world.readiness_dependents(&input).unwrap(), input);
}
