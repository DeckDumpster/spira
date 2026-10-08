//! `incident alarm` against the real binary: firing a condition twice leaves exactly one
//! inbox line, writes no spool entry (no bead path), and a second condition gets its own.

use std::process::{Command, Stdio};

fn alarm(toml: &std::path::Path, title: &str, reference: &str) -> bool {
    let mut c = Command::new(env!("CARGO_BIN_EXE_incident"))
        .args(["alarm", title, "-"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SPIRA_TOML", toml)
        .env("SPIRA_HOME", concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))
        .env("SPIRA_INCIDENT_REF", reference)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    c.wait().unwrap().success()
}

#[test]
fn a_condition_fired_twice_is_one_inbox_line_and_no_bead() {
    let tmp = testkit::TempDir::new("incident-alarm");
    let inbox = tmp.join("elsewhere/inbox.log");
    let run = tmp.join("run");
    let toml = spira_config::process::fixture_toml(
        &tmp,
        &[("SPIRA_RUN", &run.display().to_string()), ("SPIRA_CONCIERGE_INBOX", &inbox.display().to_string())],
    );
    assert!(alarm(&toml, "SLOW QUERY: select 1", "incident:slow-query-a"));
    assert!(alarm(&toml, "SLOW QUERY: select 1", "incident:slow-query-a"));
    let text = std::fs::read_to_string(&inbox).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("ALARM SLOW QUERY: select 1 [incident:slow-query-a]"));

    assert!(alarm(&toml, "DRAINING: world.sh summons gated", "incident:draining"));
    assert_eq!(std::fs::read_to_string(&inbox).unwrap().lines().count(), 2);
    assert!(!run.join("incident-spool").exists(), "an alarm must never reach the bead spool");
}
