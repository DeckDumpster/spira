//! A release is resolved against its own registry, whatever home the caller passed down. A test
//! binary of its own, so that this is the process's first pin of `spira_config`'s config.

#[macro_use]
extern crate spira_test_id;

use release::config::{process_env, resolve_as_release, Flags};
use spira_config::process::{fixture_registry_home, fixture_toml_with};
use spira_config::registry::Registry;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[test_id(trqxxf3)]
#[test]
fn a_caller_on_a_registry_older_than_the_broker_still_places_the_targets_broker_config() {
    let dir = testkit::TempDir::new("activate-registry");
    let old = fixture_registry_home(&dir.join("old"));
    std::fs::remove_file(old.join("conf.d/SPIRA_BROKER_SOCKET")).expect("the old registry lacks the key");

    let releases = dir.join("releases");
    let rel = releases.join(SHA);
    let built = fixture_registry_home(&dir.join("built"));
    std::fs::create_dir_all(&rel).unwrap();
    std::fs::rename(built, rel.join("spira")).unwrap();
    let templates = rel.join("systemd");
    std::fs::create_dir_all(&templates).unwrap();
    let tree = testkit::manifest_dir().parent().unwrap().to_path_buf();
    std::fs::copy(tree.join("systemd/mosquitto.conf"), templates.join("mosquitto.conf")).unwrap();

    let run = dir.join("run");
    // Not `fixture_toml`: it pins the process registry from the tree, whose full key set then
    // answers for the old home too.
    let reg = Registry::load(&rel.join("spira")).unwrap();
    let spec = fixture_toml_with(&reg, dir.path(), &[("SPIRA_RELEASES", releases.to_str().unwrap()), ("SPIRA_RUN", run.to_str().unwrap())]);

    let _env = testkit::env(&[("SPIRA_HOME", Some(old.to_str().unwrap())), ("SPIRA_TOML", Some(spec.to_str().unwrap()))]);
    let cfg = resolve_as_release(&Flags::default(), &process_env(), &rel).unwrap();

    let values = cfg.host_values().unwrap();
    let socket = &values["SPIRA_BROKER_SOCKET"];
    assert_eq!(cfg.broker_socket_refusal, "", "socket={socket:?}");
    assert_eq!(*socket, run.join("fixture-broker.sock").display().to_string());
    assert_eq!(release::broker::place_conf(&templates, socket, &run), Ok(true));
    let placed = std::fs::read_to_string(run.join("mosquitto.conf")).unwrap();
    assert!(placed.contains(&format!("listener 0 {socket}")), "{placed}");
}

#[test_id(tfil5vw)]
#[test]
fn a_release_without_a_registry_of_its_own_is_refused_naming_the_path() {
    let _env = testkit::env_read();
    let dir = testkit::TempDir::new("activate-registry-bare");
    let rel = dir.join(SHA);
    std::fs::create_dir_all(rel.join("spira")).unwrap();
    let e = resolve_as_release(&Flags::default(), &Default::default(), &rel).err().expect("a bare release is refused");
    assert!(e.contains(&rel.join("spira/conf.d").display().to_string()), "{e}");
}
