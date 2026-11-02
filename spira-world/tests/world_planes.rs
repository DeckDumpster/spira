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

#[test]
fn every_shipped_unit_declares_a_plane() {
    let units = shipped_units();
    assert!(units.len() > 20, "positive control: the scan must find the shipped units, found {units:?}");
    let undeclared: Vec<_> = units.iter().filter(|u| plane_of_shipped(u).is_none()).collect();
    assert!(undeclared.is_empty(), "units with no valid [X-Spira] Plane=: {undeclared:?}");
}

#[test]
fn detectors_are_observability_and_the_loop_is_work() {
    for d in ["heimdall", "watchtower", "skew", "notify", "refresh", "verify-asks", "gate-check", "cert-sweep-full", "cert-sweep-sample", "refusal-watch", "idea-prompt", "kit-bump", "filings-collect", "runtime-watch"] {
        assert_eq!(plane_of_shipped(&format!("spira-{d}.timer")), Some(Plane::Observability), "{d}");
    }
    assert_eq!(plane_of_shipped("spira-lifecycle-section.service"), Some(Plane::Observability));
    assert_eq!(plane_of_shipped("spira-reconciler-goodput.service"), Some(Plane::Observability));
    assert_eq!(plane_of_shipped("spira-alarms.timer"), Some(Plane::Observability), "the dead-man check outlives a world stop");
    for w in ["call", "sentinel", "landing-pass", "verdict", "publish", "groom", "gh-intake", "ops", "czar-pass", "mimir", "straggler-sweep", "reap-terminal"] {
        assert_eq!(plane_of_shipped(&format!("spira-{w}.timer")), Some(Plane::Work), "{w}");
    }
    for m in ["archivist", "archive", "mail-tidy", "moot-sweep", "reclaim"] {
        assert_eq!(plane_of_shipped(&format!("spira-{m}.timer")), Some(Plane::Maintenance), "{m}");
    }
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
