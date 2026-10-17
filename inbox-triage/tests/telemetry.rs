//! Triaging a batch of inbox lines pushes the run metrics plus how many events passed and were dropped.

use spira_otel::metrics::fixture::{bare, Capture};
use std::io::Write;
use std::path::Path;

#[test]
fn a_batch_of_events_pushes_passed_and_dropped_counts() {
    let cap = Capture::start();
    let tmp = testkit::TempDir::new("inbox-triage-telemetry");
    let inbox = tmp.path().join("inbox.log");
    std::fs::write(&inbox, "").unwrap();
    let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_METRICS_ENDPOINT", &cap.addr), ("SPIRA_CONCIERGE_INBOX", &inbox.display().to_string()), ("SPIRA_CONCIERGE_INBOX_DEDUP", "600")]);
    let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
    let mut child = bare(Path::new(env!("CARGO_BIN_EXE_inbox-triage")), &toml, &home).stdout(std::process::Stdio::null()).spawn().expect("inbox-triage runs");
    std::thread::sleep(std::time::Duration::from_millis(600));
    let mut f = std::fs::OpenOptions::new().append(true).open(&inbox).unwrap();
    writeln!(f, "alpha event").unwrap();
    writeln!(f, "alpha event").unwrap();
    f.flush().unwrap();
    let seen = cap.samples(4);
    let _ = child.kill();
    let _ = child.wait();
    let value = |n: &str| seen.iter().find(|s| s.name == n).map(|s| s.value);
    assert_eq!(value("spira_inbox_events_passed"), Some(1.0), "{seen:?}");
    assert_eq!(value("spira_inbox_events_dropped"), Some(1.0), "{seen:?}");
    cap.assert_ran("inbox-triage", "triage");
}
