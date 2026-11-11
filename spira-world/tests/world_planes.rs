//! The loop is the scheduler's tasks, so no shipped unit is left on the work plane for a world
//! halt to stop, and none carries the old `world guard` start check: the scheduler's halt is
//! the one gate (S14). Plane membership is each shipped unit's own `[X-Spira] Plane=`; this
//! suite reads the real shipped unit files, so a unit that loses its declaration fails here.

use std::path::{Path, PathBuf};

use spira_world::scheduler::{is_kept, keep_units};
use spira_world::sysctl::{plane_from_unit_text, Plane};

fn unit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd")
}

/// Shipped units world does not act on: the dashboard's query engine, the socket.
const UNPLANED: &[&str] = &["spira-loom.service", "spira-lc.service"];

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
fn no_shipped_unit_is_left_on_the_work_plane_or_carries_the_old_halt_guard() {
    let services = shipped_units();
    assert!(services.iter().any(|u| u == "spira-runtime.service"), "positive control: the scan finds the shipped units");
    let work: Vec<_> = services.iter().filter(|u| plane_of_shipped(u) == Some(Plane::Work)).collect();
    assert!(work.is_empty(), "the loop is the scheduler's tasks; a work-plane unit left would run outside its halt: {work:?}");
    let guarded: Vec<_> = services.iter().filter(|u| !halt_guard_lines(u).is_empty()).collect();
    assert!(guarded.is_empty(), "the scheduler's halt is the one gate (S14); no unit re-implements it: {guarded:?}");
}

#[test]
fn the_scheduler_ships_and_is_on_the_keep_list() {
    assert!(unit_dir().join("spira-runtime.service").is_file());
    assert!(unit_dir().join("lc-serve.socket").is_file());
    assert!(unit_dir().join("dolt-beads.service").is_file());
    for u in keep_units("-prod") {
        assert!(is_kept(&u, "-prod"), "{u}");
    }
}

/// Mail delivery is the scheduler's (`mail.arrived` and `mail.wake`): world starts no
/// `spira-mail-deliver` unit, and the template is not shipped.
#[test]
fn world_names_no_mail_delivery_unit() {
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/world.rs")).unwrap();
    assert!(src.contains("fn cmd_start"), "positive control: the scan reads world's source");
    assert!(!src.contains("spira-mail-deliver"), "world still starts the retired spira-mail-deliver unit");
    assert!(!unit_dir().join("spira-mail-deliver.service").exists(), "spira-mail-deliver.service is still a template");
}
