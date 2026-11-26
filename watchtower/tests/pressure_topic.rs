// covers: watchtower/src/pressure_topic.rs
//! A watchtower pass over a fixture `/proc/pressure`, against a real broker.
#[macro_use]
extern crate spira_test_id;

use std::path::Path;
use std::sync::Arc;

use runtime::ports::{SeededRng, VirtualClock};
use spira_pub::{Options, Publisher};
use testkit::broker::Broker;
use testkit::TempDir;
use watchtower::pressure_topic::{record_path, Lines, Pressure, TOPIC};

const LINES: Lines = Lines { cpu: 20.0, io: 30.0 };

fn psi(dir: &Path, cpu: &str, io: &str) {
    for (resource, full) in [("cpu", cpu), ("io", io)] {
        let text = format!("some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300={full} total=0\n");
        std::fs::write(dir.join(resource), text).unwrap();
    }
}

fn pressure(broker: &Broker, run: &Path) -> Pressure {
    let options = Options::new(broker.socket(), Arc::new(VirtualClock::new(0)), Box::new(SeededRng::new(1)));
    Pressure::new(Publisher::with("watchtower", options).unwrap(), run)
}

fn held(broker: &Broker) -> Vec<serde_json::Value> {
    broker.retained(TOPIC).iter().map(|(_, p)| serde_json::from_str(p).unwrap()).collect()
}

#[test_id(t18kdg5)]
#[test]
fn a_pass_over_the_line_publishes_and_an_identical_pass_publishes_nothing() {
    let (broker, d) = (Broker::up(), TempDir::new("psi"));
    psi(d.path(), "1.00", "45.50");
    let p = pressure(&broker, d.path());
    p.pass(d.path(), &LINES, 100).unwrap();
    let first = serde_json::json!({"cpu": 1.0, "io": 45.5, "cpu_over": false, "io_over": true, "at": 100});
    assert_eq!(held(&broker), std::slice::from_ref(&first));

    p.pass(d.path(), &LINES, 200).unwrap();
    assert_eq!(held(&broker), [first], "a second publish would carry at=200");
    assert!(record_path(d.path()).is_file());
    assert_eq!(std::fs::read_dir(d.path().join("conditions")).unwrap().count(), 1, "one file, overwritten");
}

#[test_id(th6fnig)]
#[test]
fn a_new_process_connecting_republishes_the_recorded_band() {
    let (mut broker, d) = (Broker::up(), TempDir::new("psi"));
    psi(d.path(), "1.00", "45.50");
    pressure(&broker, d.path()).pass(d.path(), &LINES, 100).unwrap();
    broker.restart();
    assert!(held(&broker).is_empty(), "the broker keeps nothing");
    let next = pressure(&broker, d.path());
    next.reconnect_recorded();
    next.pass(d.path(), &LINES, 200).unwrap();
    // Dropping flushes what the connect queued.
    drop(next);
    assert_eq!(held(&broker)[0]["at"], 100);
}

#[test_id(t560w97)]
#[test]
fn with_the_broker_stopped_a_pass_returns_and_a_later_pass_publishes_the_crossing() {
    let (mut broker, d) = (Broker::up(), TempDir::new("psi"));
    psi(d.path(), "25.00", "0.00");
    broker.stop();
    let p = pressure(&broker, d.path());
    let e = p.pass(d.path(), &LINES, 100).unwrap_err();
    assert!(e.contains(TOPIC), "{e}");
    assert!(!record_path(d.path()).exists(), "an unpublished crossing is not recorded as published");
    broker.restart();
    p.pass(d.path(), &LINES, 200).unwrap();
    assert_eq!(held(&broker)[0]["at"], 200);
}
