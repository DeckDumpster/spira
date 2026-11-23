//! One process, so the config pin is this binary's own: a refusal is never pinned, a success is
//! kept for the life of the process even when the file goes away.

#[macro_use]
extern crate spira_test_id;
use cockpit_collect::{io, probes};

#[test_id(tp17ytt)]
#[test]
fn a_resolved_config_is_pinned_and_a_refused_one_is_not() {
    let dir = testkit::TempDir::new("cockpit-pinned-config");
    let run = dir.path().join("run");
    std::fs::create_dir_all(&run).unwrap();
    let home = spira_config::resolve::locate_home_for_process().expect("SPIRA_HOME names the harness tree");
    let toml = spira_config::toml_path_at(dir.path());
    let _env = testkit::env(&[("SPIRA_TOML", Some(toml.to_str().unwrap())), ("SPIRA_HOME", Some(home.to_str().unwrap()))]);

    assert!(probes::Cfg::load().is_err(), "no file yet: refused");
    assert!(io::try_run_dir().is_err(), "no file yet: refused");

    let written = spira_config::process::fixture_toml(dir.path(), &[("SPIRA_RUN", run.to_str().unwrap()), ("SPIRA_WORKSPACES", dir.path().to_str().unwrap())]);
    assert_eq!(written, toml);
    let cfg = probes::Cfg::load().expect("the refusal was not pinned");
    assert!(probes::run("now", &cfg).is_some());
    assert_eq!(io::run_dir(), run);

    std::fs::remove_file(&toml).unwrap();
    let again = probes::Cfg::load().expect("the pinned config answers without the file");
    assert_eq!(again.instance, cfg.instance);
    assert!(probes::run("now", &again).is_some());
    assert_eq!(io::run_dir(), run, "the second answer is from the same run directory");
}
