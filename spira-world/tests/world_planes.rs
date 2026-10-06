//! A world halt stops the work plane and leaves the detectors running. Plane membership is
//! each shipped unit's own `[X-Spira] Plane=`, read through `systemctl cat`; this suite
//! drives the real `world` binary against a stub systemctl serving the real shipped unit
//! files, so a unit that loses (or never gains) its declaration fails here.

use std::path::{Path, PathBuf};
use std::process::Command;

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
fn a_timer_and_its_service_share_a_plane() {
    for t in shipped_units().iter().filter(|u| u.ends_with(".timer")) {
        let svc = t.replace(".timer", ".service");
        assert_eq!(plane_of_shipped(t), plane_of_shipped(&svc), "{t} and {svc} disagree");
    }
}

#[test]
fn detectors_are_observability_and_the_loop_is_work() {
    for d in ["auron", "watchtower", "skew", "notify", "refresh", "verify-asks", "gate-check", "cert-sweep-full", "cert-sweep-sample"] {
        assert_eq!(plane_of_shipped(&format!("spira-{d}.timer")), Some(Plane::Observability), "{d}");
    }
    for w in ["summon", "sentinel", "landing-pass", "verdict", "publish", "reconciler", "reconciler-flow", "groom", "gh-intake", "ops", "czar-pass", "maechen", "straggler-sweep"] {
        assert_eq!(plane_of_shipped(&format!("spira-{w}.timer")), Some(Plane::Work), "{w}");
    }
    for m in ["archivist", "archive", "mail-tidy", "moot-sweep"] {
        assert_eq!(plane_of_shipped(&format!("spira-{m}.timer")), Some(Plane::Maintenance), "{m}");
    }
}

struct Fixture {
    tmp: testkit::TempDir,
    toml: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let tmp = testkit::TempDir::new(name);
        std::fs::create_dir_all(tmp.join("home")).unwrap();
        std::fs::create_dir_all(tmp.join("run")).unwrap();
        // SPIRA_TOML, not the old XDG `$HOME/.config/spira/spira.toml` auto-discovery (per
        // Ryan 2026-10-05: one source of config — the launcher sets it explicitly, never
        // guessed). A complete fixture (every registered key declared), not a hand-rolled
        // partial one, with SPIRA_RUN pinned into this fixture's own tmp dir — SPIRA_INSTANCE
        // stays the fixture's own default, "prod", for which containment is a no-op.
        let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_RUN", &tmp.join("run").display().to_string())]);
        let stub = tmp.join("systemctl");
        testkit::write_exe(
            &stub,
            r#"#!/bin/sh
verb=""; arg=""
for a in "$@"; do
    case "$a" in --*|-p|Result|Type|--value) continue ;; esac
    if [ -z "$verb" ]; then verb="$a"; elif [ -z "$arg" ]; then arg="$a"; fi
done
file="$UNIT_DIR/$(printf '%s' "$arg" | sed 's/-prod\././')"
case "$verb" in
list-unit-files)
    case "$arg" in
        'spira-*.timer') for f in "$UNIT_DIR"/spira-*.timer; do b=$(basename "$f" .timer); echo "$b-prod.timer enabled"; done ;;
        *'*'*) ;;
        *) [ -f "$file" ] && echo "$arg enabled" ;;
    esac ;;
cat) [ -f "$file" ] && cat "$file" || exit 1 ;;
is-enabled) echo enabled ;;
is-active) echo inactive; exit 3 ;;
stop|start) echo "$verb $arg" >> "$CALLS" ;;
esac
exit 0
"#,
        );
        Fixture { tmp, toml }
    }

    fn world(&self, args: &[&str]) -> (String, Vec<String>) {
        let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_world")).parent().unwrap().to_path_buf();
        let calls = self.tmp.join("calls");
        let _ = std::fs::remove_file(&calls);
        // SPIRA_HOME is the checkout's own spira/ (where conf.d — the key registry —
        // lives), not a synthetic empty one: `cfg()` refuses a key with no conf.d/<KEY>
        // entry, and the old "an existing-but-empty conf.d is fine" tolerance is gone.
        let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let out = Command::new("env")
            .arg("-i")
            .arg(format!("HOME={}", self.tmp.join("home").display()))
            .arg(format!("PATH={}:/usr/bin:/bin", bin_dir.display()))
            .arg(format!("SPIRA_HOME={}", real_home.display()))
            .arg(format!("SPIRA_TOML={}", self.toml.display()))
            .arg(format!("SPIRA_SYSTEMCTL={}", self.tmp.join("systemctl").display()))
            .arg(format!("UNIT_DIR={}", unit_dir().display()))
            .arg(format!("CALLS={}", calls.display()))
            .arg(format!("SPIRA_CTRL={}", self.tmp.join("no-ctrl.json").display()))
            .arg("world")
            .args(args)
            .output()
            .expect("cannot spawn world");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        let calls = std::fs::read_to_string(&calls).unwrap_or_default().lines().map(str::to_string).collect();
        (stdout, calls)
    }

    fn stamp(&self, name: &str) -> bool {
        self.tmp.join("run").join(name).is_file()
    }
}

fn stopped(calls: &[String], base: &str) -> bool {
    calls.iter().any(|c| c == &format!("stop spira-{base}-prod.timer"))
}

#[test]
fn a_plain_stop_halts_the_work_plane_and_leaves_detectors_and_maintenance() {
    let f = Fixture::new("planes-stop-work");
    let (out, calls) = f.world(&["stop"]);
    assert!(stopped(&calls, "summon") && stopped(&calls, "sentinel"), "positive control: work timers must stop\n{out}\n{calls:?}");
    for kept in ["auron", "watchtower", "skew", "gate-check", "verify-asks", "watch-notify", "archivist", "mail-tidy"] {
        assert!(!stopped(&calls, kept), "{kept} must keep running through a work halt\n{out}");
    }
    assert!(f.stamp("world.halted"), "the work plane's stamp is world.halted");
    assert!(!f.stamp("world.halted.observability") && !f.stamp("world.halted.maintenance"));
}

#[test]
fn stop_observability_stops_only_the_detectors_and_does_not_set_the_work_stamp() {
    let f = Fixture::new("planes-stop-obs");
    let (out, calls) = f.world(&["stop", "--observability"]);
    assert!(stopped(&calls, "auron") && stopped(&calls, "watchtower") && stopped(&calls, "gate-check"), "{out}\n{calls:?}");
    assert!(!stopped(&calls, "summon") && !stopped(&calls, "archivist"), "{out}");
    assert!(f.stamp("world.halted.observability"));
    assert!(!f.stamp("world.halted"), "a detector detecting world.halted must not read an observability stop as a work halt");
}

#[test]
fn stop_maintenance_is_its_own_switch() {
    let f = Fixture::new("planes-stop-maint");
    let (out, calls) = f.world(&["stop", "--maintenance"]);
    assert!(stopped(&calls, "archivist") && stopped(&calls, "mail-tidy"), "{out}\n{calls:?}");
    assert!(!stopped(&calls, "summon") && !stopped(&calls, "auron"));
}

#[test]
fn start_brings_back_work_and_observability_but_not_maintenance_and_clears_their_stamps() {
    let f = Fixture::new("planes-start");
    f.world(&["stop", "--all"]);
    assert!(f.stamp("world.halted") && f.stamp("world.halted.observability") && f.stamp("world.halted.maintenance"));
    let (out, calls) = f.world(&["start"]);
    let started = |b: &str| calls.iter().any(|c| c == &format!("start spira-{b}-prod.timer"));
    assert!(started("summon") && started("auron"), "{out}\n{calls:?}");
    assert!(!started("archivist"), "{out}");
    assert!(!f.stamp("world.halted") && !f.stamp("world.halted.observability"));
    assert!(f.stamp("world.halted.maintenance"), "maintenance stays stopped until its own start");
}

#[test]
fn status_reports_the_planes_separately_through_a_work_halt() {
    let f = Fixture::new("planes-status");
    f.world(&["stop"]);
    let (out, _) = f.world(&["status"]);
    assert!(out.contains("spira: plane work: STOPPED"), "{out}");
    assert!(out.contains("spira: plane observability: RUNNING"), "{out}");
    assert!(out.contains("spira: plane maintenance: RUNNING"), "{out}");
}
