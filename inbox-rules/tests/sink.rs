// covers: inbox-rules/src/lib.rs
//! `Sink` as every production writer builds it. The configuration is pinned per process, so each
//! test runs its body in a child of this binary that names its own fixture.
#[macro_use]
extern crate spira_test_id;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
            ("SPIRA_CONCIERGE_PUSH_WAKE", "a-wake-command"),
            ("SPIRA_CONCIERGE_PUSH_PATTERN", "ZZ9"),
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

#[test_id(tarb0s2)]
#[test]
fn a_configured_sink_reads_every_key_and_its_push_starts_the_releases_flusher() {
    let Some(dir) = child_dir() else {
        let (broker, dir) = (Broker::up(), testkit::TempDir::new("sink-configured"));
        let release = dir.join("release");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        let fifo = std::ffi::CString::new(dir.join("flushed").to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "make the fifo");
        testkit::write_exe(release.join("bin/inbox-triage"), &format!("#!/bin/sh\necho \"$1\" > '{}'\n", dir.join("flushed").display()));
        let mut env = fixture(&dir, broker.socket());
        env.push(("SPIRA_RELEASE", release.display().to_string()));
        rerun("a_configured_sink_reads_every_key", &dir, env);
        assert_eq!(broker.retained("publisher/a-writer").len(), 1, "published on the configured broker");
        return;
    };
    let sink = Sink::configured("a-writer").unwrap();
    assert_eq!(sink.inbox, dir.join("elsewhere/inbox.log"));
    assert_eq!(sink.cursor, concierge_cursor(dir.join("run").to_str().unwrap()));
    assert_eq!((sink.bus.clone(), sink.name.as_str()), (Bus::Configured, "a-writer"));
    assert_eq!((sink.wake.as_str(), sink.pattern.as_str()), ("a-wake-command", "ZZ9"));

    assert_eq!(sink.record("t1 ZZ9 down"), Ok(true));

    let (said, flushed) = std::sync::mpsc::channel();
    let fifo = dir.join("flushed");
    std::thread::spawn(move || said.send(std::fs::read_to_string(fifo).unwrap()));
    assert_eq!(flushed.recv_timeout(Duration::from_secs(30)).expect("the flusher ran"), "flush\n");
}

#[test_id(t3lg3gz)]
#[test]
fn a_push_that_cannot_start_after_the_append_is_not_a_failed_write() {
    let Some(_) = child_dir() else {
        let (broker, dir) = (Broker::up(), testkit::TempDir::new("sink-noflusher"));
        let mut env = fixture(&dir, broker.socket());
        env.push(("SPIRA_RELEASE", dir.join("no-release").display().to_string()));
        rerun("a_push_that_cannot_start_after_the_append", &dir, env);
        return;
    };
    let mut sink = Sink::configured("a-writer").unwrap();

    assert_eq!(sink.record("t1 ZZ9 down"), Ok(false));
    assert_eq!(std::fs::read_to_string(&sink.inbox).unwrap(), "t1 ZZ9 down\n");

    sink.inbox = sink.inbox.parent().unwrap().to_path_buf();
    assert!(sink.record("t2 ZZ9 down").is_err(), "a line that is not in the inbox is an error");
}

#[test_id(t189b3u)]
#[test]
fn a_missing_key_is_refused_by_name() {
    let Some(_) = child_dir() else {
        let dir = testkit::TempDir::new("sink-missing");
        let env = fixture(&dir, &dir.join("no.sock"));
        let declared = Path::new(&env.iter().find(|(k, _)| *k == "SPIRA_TOML").unwrap().1);
        spira_config::unset_paths_in_file(declared, &["spira.concierge_push_pattern"]).unwrap();
        rerun("a_missing_key_is_refused_by_name", &dir, env);
        return;
    };
    let refused = Sink::configured("a-writer").unwrap_err().to_lowercase();
    assert!(refused.contains("concierge_push_pattern"), "{refused}");
}
