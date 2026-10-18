// covers: unit-run/** systemd/spira-unit-run*.service
use testkit::TempDir;
use unit_run::*;

fn shipped(name: &str) -> String {
    std::fs::read_to_string(format!("{}/../systemd/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap_or_default()
}

#[test]
fn io_accounting_dropin_is_written_once() {
    let d = TempDir::new("unit-run-acct");
    assert!(ensure_io_accounting(d.path()).unwrap());
    assert_eq!(std::fs::read_to_string(accounting_dropin_path(d.path())).unwrap(), ACCOUNTING_DROPIN);
    assert!(!ensure_io_accounting(d.path()).unwrap());
}

#[test]
fn io_controller_presence_is_read_from_the_controllers_file() {
    let d = TempDir::new("unit-run-ctl");
    let f = d.path().join("cgroup.controllers");
    std::fs::write(&f, "cpu memory pids\n").unwrap();
    assert!(!io_controller_available(&f));
    std::fs::write(&f, "cpu io memory pids\n").unwrap();
    assert!(io_controller_available(&f));
    assert!(!io_controller_available(&d.path().join("absent")));
}

#[test]
fn the_listener_unit_turns_accounting_on() {
    let u = shipped("spira-unit-run-dbus.service");
    assert!(u.lines().any(|l| l.starts_with("ExecStart=") && l.ends_with("unit-run watch")), "positive control: the reader parses the unit");
    assert!(u.lines().any(|l| l.starts_with("ExecStartPre=") && l.ends_with("unit-run ensure-accounting")));
}

#[test]
fn the_journal_follower_unit_is_not_shipped() {
    assert!(shipped("spira-unit-run-dbus.service").contains("[Service]"), "positive control: shipped() finds a unit");
    assert!(!std::path::Path::new(&format!("{}/../systemd/spira-unit-run.service", env!("CARGO_MANIFEST_DIR"))).exists());
}

#[test]
fn a_record_carries_the_fields_its_consumers_read() {
    let r = Record {
        invocation_id: "inv".into(),
        unit: "a.service".into(),
        started_us: Some(1_000_000),
        ended_us: 3_000_000,
        result: "success".into(),
        exit_code: None,
        exit_status: None,
        cpu_usec: None,
        memory_peak: None,
        io_read_bytes: Some(7),
        io_write_bytes: None,
    };
    let f: std::collections::BTreeMap<String, serde_json::Value> = record_fields(&r, Some("a.timer"), "inst-x").into_iter().collect();
    assert_eq!(f["duration_ms"], 2000);
    assert_eq!(f["kind"], "timer-triggered");
    assert_eq!(f["io_read_bytes"], 7);
    assert!(!f.contains_key("cpu_usec"), "an absent fact is left out, not nulled");
}
