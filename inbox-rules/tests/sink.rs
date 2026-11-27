// covers: inbox-rules/src/lib.rs
//! `Sink` as every production writer builds it. The configuration is pinned per process, so each
//! test runs its body in a child of this binary that names its own fixture.
#[macro_use]
extern crate spira_test_id;

use std::path::{Path, PathBuf};
use std::process::Command;

use inbox_rules::{concierge_cursor, Bus, Sink};
use testkit::broker::Broker;

const CHILD: &str = "INBOX_SINK_CHILD_DIR";

fn fixture(dir: &Path, socket: &Path) -> Vec<(&'static str, String)> {
    let (run, inbox) = (dir.join("run"), dir.join("elsewhere/inbox.log"));
    spira_config::process::fixture_metrics_env(
        dir,
        "",
        &[
            ("SPIRA_BROKER_SOCKET", socket.to_str().unwrap()),
            ("SPIRA_RUN", run.to_str().unwrap()),
            ("SPIRA_CONCIERGE_INBOX", inbox.to_str().unwrap()),
        ],
    )
}

/// Runs the test whose name contains `test` again, alone, with `env` and `dir` named to it.
fn rerun(test: &str, dir: &Path, env: Vec<(&'static str, String)>) {
    let out = Command::new(std::env::current_exe().unwrap()).args([test, "--nocapture"]).envs(env).env(CHILD, dir).output().unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && said.contains("1 passed"), "{said}");
}

fn child_dir() -> Option<PathBuf> {
    std::env::var_os(CHILD).map(PathBuf::from)
}

#[test_id(te21x9v)]
#[test]
fn a_configured_sink_reads_its_keys_and_announces_on_the_configured_broker() {
    let Some(dir) = child_dir() else {
        let (broker, dir) = (Broker::up(), testkit::TempDir::new("sink-configured"));
        rerun("a_configured_sink_reads_its_keys", &dir, fixture(&dir, broker.socket()));
        assert_eq!(broker.retained("concierge/inbox").len(), 1, "the inbox is announced");
        assert_eq!(broker.retained("publisher/a-writer").len(), 1, "published on the configured broker");
        return;
    };
    let sink = Sink::configured("a-writer").unwrap();
    assert_eq!(sink.inbox, dir.join("elsewhere/inbox.log"));
    assert_eq!(sink.cursor, concierge_cursor(dir.join("run").to_str().unwrap()));
    assert_eq!((sink.bus.clone(), sink.name.as_str()), (Bus::Configured, "a-writer"));

    assert_eq!(sink.record("t1 P0 down"), Ok(()));

    assert_eq!(std::fs::read_to_string(&sink.inbox).unwrap(), "t1 P0 down\n");
}

#[test_id(t9o4ynv)]
#[test]
fn a_line_that_is_not_in_the_inbox_is_an_error() {
    let Some(_) = child_dir() else {
        let (broker, dir) = (Broker::up(), testkit::TempDir::new("sink-refused"));
        rerun("a_line_that_is_not_in_the_inbox", &dir, fixture(&dir, broker.socket()));
        return;
    };
    let mut sink = Sink::configured("a-writer").unwrap();
    sink.inbox = std::env::current_dir().unwrap();

    assert!(sink.record("t2 down").unwrap_err().starts_with("cannot append to"));
}

#[test_id(t189b3u)]
#[test]
fn a_missing_key_is_refused_by_name() {
    let Some(_) = child_dir() else {
        let dir = testkit::TempDir::new("sink-missing");
        let env = fixture(&dir, &dir.join("no.sock"));
        let declared = Path::new(&env.iter().find(|(k, _)| *k == "SPIRA_TOML").unwrap().1);
        spira_config::unset_paths_in_file(declared, &["spira.concierge_inbox"]).unwrap();
        rerun("a_missing_key_is_refused_by_name", &dir, env);
        return;
    };
    let refused = Sink::configured("a-writer").unwrap_err().to_lowercase();
    assert!(refused.contains("concierge_inbox"), "{refused}");
}
