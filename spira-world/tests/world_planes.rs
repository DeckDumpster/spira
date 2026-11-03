//! A world halt stops the work plane and leaves the detectors running. Plane membership is
//! each shipped unit's own `[X-Spira] Plane=`, read through `systemctl cat`; this suite
//! drives the real `world` binary against a stub systemctl serving the real shipped unit
//! files, so a unit that loses (or never gains) its declaration fails here.

use std::path::{Path, PathBuf};

use spira_world::sysctl::{plane_from_unit_text, Plane};

fn unit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd")
}

/// Shipped units world does not act on: the dashboard, its query engine, the socket.
const UNPLANED: &[&str] = &["spira-cockpit.service", "spira-loom.service", "spira-lc.service"];

fn shipped_units() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(unit_dir())
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("spira-") && (n.ends_with(".timer") || n.ends_with(".service")))
        .filter(|n| !UNPLANED.contains(&n.as_str()))
        .collect();
    v.sort();
    v
}

fn plane_of_shipped(unit: &str) -> Option<Plane> {
    plane_from_unit_text(&std::fs::read_to_string(unit_dir().join(unit)).unwrap())
}

fn halt_guard_lines(unit: &str) -> Vec<String> {
    std::fs::read_to_string(unit_dir().join(unit))
        .unwrap()
        .lines()
        .filter_map(|l| l.strip_prefix("ExecStartPre="))
        .filter(|l| l.contains("/bin/world "))
        .map(str::to_string)
        .collect()
}

#[test]
fn every_work_service_carries_the_halt_guard_and_no_other_does() {
    let services: Vec<String> = shipped_units().into_iter().filter(|u| u.ends_with(".service")).collect();
    let work: Vec<&String> = services.iter().filter(|u| plane_of_shipped(u) == Some(Plane::Work)).collect();
    assert!(work.iter().any(|u| *u == "spira-rounds.service"), "positive control: the scan finds the work services");
    let unguarded: Vec<_> = work.iter().filter(|u| halt_guard_lines(u).len() != 1).collect();
    assert!(unguarded.is_empty(), "work services without exactly one halt check: {unguarded:?}");
    let stray: Vec<_> = services.iter().filter(|u| plane_of_shipped(u) != Some(Plane::Work) && !halt_guard_lines(u).is_empty()).collect();
    assert!(stray.is_empty(), "a unit off the work plane must keep running through a halt: {stray:?}");
    assert_eq!(plane_of_shipped("spira-mail-deliver.service"), Some(Plane::Observability), "mail delivery stays up through a halt");
}
