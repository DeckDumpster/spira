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
    for d in ["heimdall", "watchtower", "skew", "notify", "refresh", "verify-asks", "gate-check", "cert-sweep-full", "cert-sweep-sample", "refusal-watch", "idea-prompt", "kit-bump", "filings-collect", "runtime-watch"] {
        assert_eq!(plane_of_shipped(&format!("spira-{d}.timer")), Some(Plane::Observability), "{d}");
    }
    assert_eq!(plane_of_shipped("spira-lifecycle-section.service"), Some(Plane::Observability));
    assert_eq!(plane_of_shipped("spira-reconciler-goodput.service"), Some(Plane::Observability));
    assert_eq!(plane_of_shipped("spira-alarms.timer"), Some(Plane::Observability), "the dead-man check outlives a world stop");
    for w in ["call", "sentinel", "landing-pass", "verdict", "publish", "reconciler-flow", "groom", "gh-intake", "ops", "czar-pass", "mimir", "straggler-sweep", "reap-terminal"] {
        assert_eq!(plane_of_shipped(&format!("spira-{w}.timer")), Some(Plane::Work), "{w}");
    }
    for m in ["archivist", "archive", "mail-tidy", "moot-sweep", "reclaim"] {
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
        // SPIRA_TOML, not the old XDG `$HOME/.config/spira/` default config file auto-discovery (per
        // Ryan 2026-10-05: one source of config — the launcher sets it explicitly, never
        // guessed). A complete fixture (every registered key declared), not a hand-rolled
        // partial one, with SPIRA_RUN pinned into this fixture's own tmp dir — SPIRA_INSTANCE
        // stays the fixture's own default, "prod", for which containment is a no-op.
        let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_RUN", &tmp.join("run").display().to_string())]);
        let stub = tmp.join("systemctl");
        testkit::write_exe(
            &stub,
            r#"#!/bin/sh
verb=""; arg=""; states=""
for a in "$@"; do
    case "$a" in --state=*) states="${a#--state=}" ;; esac
done
for a in "$@"; do
    case "$a" in --*|-p|Result|Type|--value) continue ;; esac
    if [ -z "$verb" ]; then verb="$a"; elif [ -z "$arg" ]; then arg="$a"; fi
done
file="$UNIT_DIR/$(printf '%s' "$arg" | sed 's/-prod\././')"
case "$verb" in
list-unit-files)
    case "$arg" in
        'spira-*.timer') for f in "$UNIT_DIR"/spira-*.timer; do b=$(basename "$f" .timer); echo "$b-prod.timer enabled"; done ;;
        'spira-*.service') for f in "$UNIT_DIR"/spira-*.service; do b=$(basename "$f" .service); echo "$b-prod.service enabled"; done ;;
        *'*'*) ;;
        *) [ -f "$file" ] && echo "$arg enabled" ;;
    esac ;;
list-units)
    [ "$arg" = 'spira-*.service' ] && [ -f "$LIVE" ] && while read -r u s; do
        case ",${states:-$s}," in *",$s,"*) echo "$u loaded $s running -" ;; esac
    done < "$LIVE" ;;
cat) [ -f "$file" ] && cat "$file" || exit 1 ;;
is-enabled) echo enabled ;;
is-active) echo inactive; exit 3 ;;
stop|start|reset-failed) echo "$verb $arg" >> "$CALLS" ;;
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
            .arg(format!("LIVE={}", self.tmp.join("live").display()))
            .arg(format!("SPIRA_CTRL={}", self.tmp.join("no-ctrl.json").display()))
            .arg("world")
            .args(args)
            .output()
            .expect("cannot spawn world");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        let calls = std::fs::read_to_string(&calls).unwrap_or_default().lines().map(str::to_string).collect();
        (stdout, calls)
    }

    /// Units systemd reports as loaded, one `<unit> <ACTIVE state>` per line — what the stub
    /// serves to `list-units spira-*.service`, filtered by `--state=` the way systemd does.
    fn live(&self, units: &[(&str, &str)]) {
        let text: String = units.iter().map(|(u, s)| format!("{u} {s}\n")).collect();
        std::fs::write(self.tmp.join("live"), text).unwrap();
    }

    /// Run one `ExecStartPre=` line from a shipped unit, its `@SPIRA_PROD_ROOT@/bin/` resolved
    /// to this build's binaries, in the fixture's environment. Returns (succeeded, output).
    fn exec_start_pre(&self, line: &str) -> (bool, String) {
        let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_world")).parent().unwrap().to_path_buf();
        let resolved = line.replace("@SPIRA_PROD_ROOT@/bin/", &format!("{}/", bin_dir.display()));
        let argv: Vec<&str> = resolved.split_whitespace().collect();
        let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let out = Command::new("env")
            .arg("-i")
            .arg(format!("HOME={}", self.tmp.join("home").display()))
            .arg(format!("PATH={}:/usr/bin:/bin", bin_dir.display()))
            .arg(format!("SPIRA_HOME={}", real_home.display()))
            .arg(format!("SPIRA_TOML={}", self.toml.display()))
            .args(&argv)
            .output()
            .expect("cannot spawn the ExecStartPre command");
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
    }

    fn halt_stamp(&self) -> PathBuf {
        self.tmp.join("run").join("world.halted")
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
    assert!(stopped(&calls, "call") && stopped(&calls, "sentinel"), "positive control: work timers must stop\n{out}\n{calls:?}");
    for kept in ["heimdall", "watchtower", "skew", "gate-check", "verify-asks", "watch-notify", "archivist", "mail-tidy"] {
        assert!(!stopped(&calls, kept), "{kept} must keep running through a work halt\n{out}");
    }
    assert!(f.stamp("world.halted"), "the work plane's stamp is world.halted");
    assert!(!f.stamp("world.halted.observability") && !f.stamp("world.halted.maintenance"));
}

#[test]
fn stop_observability_stops_only_the_detectors_and_does_not_set_the_work_stamp() {
    let f = Fixture::new("planes-stop-obs");
    let (out, calls) = f.world(&["stop", "--observability"]);
    assert!(stopped(&calls, "heimdall") && stopped(&calls, "watchtower") && stopped(&calls, "gate-check"), "{out}\n{calls:?}");
    assert!(!stopped(&calls, "call") && !stopped(&calls, "archivist"), "{out}");
    assert!(f.stamp("world.halted.observability"));
    assert!(!f.stamp("world.halted"), "a detector detecting world.halted must not read an observability stop as a work halt");
}

#[test]
fn stop_maintenance_is_its_own_switch() {
    let f = Fixture::new("planes-stop-maint");
    let (out, calls) = f.world(&["stop", "--maintenance"]);
    assert!(stopped(&calls, "archivist") && stopped(&calls, "mail-tidy"), "{out}\n{calls:?}");
    assert!(!stopped(&calls, "call") && !stopped(&calls, "heimdall"));
}

#[test]
fn start_brings_back_work_and_observability_but_not_maintenance_and_clears_their_stamps() {
    let f = Fixture::new("planes-start");
    f.world(&["stop", "--all"]);
    assert!(f.stamp("world.halted") && f.stamp("world.halted.observability") && f.stamp("world.halted.maintenance"));
    let (out, calls) = f.world(&["start"]);
    let started = |b: &str| calls.iter().any(|c| c == &format!("start spira-{b}-prod.timer"));
    assert!(started("call") && started("heimdall"), "{out}\n{calls:?}");
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

#[test]
fn a_plain_stop_ends_a_running_rounds_loop_and_gate_worker_as_services_and_as_timers() {
    let f = Fixture::new("planes-stop-oneshots");
    // A oneshot mid-run is `activating`, not `active`: the rounds loop and the gate worker
    // are oneshots that run for minutes to hours.
    f.live(&[
        ("spira-sentinel-prod.service", "active"),
        ("spira-rounds-prod.service", "activating"),
        ("spira-gate-worker-prod.service", "activating"),
    ]);
    let (out, calls) = f.world(&["stop"]);
    let stopped_unit = |u: &str| calls.iter().any(|c| c == &format!("stop {u}"));
    assert!(stopped_unit("spira-sentinel-prod.service"), "positive control: an active work service is stopped\n{out}\n{calls:?}");
    for u in ["spira-rounds-prod.service", "spira-gate-worker-prod.service", "spira-rounds-prod.timer", "spira-gate-worker-prod.timer"] {
        assert!(stopped_unit(u), "{u} must be stopped by a work halt\n{out}\n{calls:?}");
    }
}

#[test]
fn a_plain_stop_leaves_mail_delivery_running() {
    let f = Fixture::new("planes-stop-mail");
    f.live(&[("spira-sentinel-prod.service", "active"), ("spira-mail-deliver-prod.service", "active")]);
    let (out, calls) = f.world(&["stop"]);
    assert!(calls.iter().any(|c| c == "stop spira-sentinel-prod.service"), "positive control\n{out}\n{calls:?}");
    assert!(
        !calls.iter().any(|c| c == "stop spira-mail-deliver-prod.service"),
        "mail delivery is the operator's channel in; a halt must not take it down\n{out}\n{calls:?}"
    );
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
fn while_halted_the_rounds_loop_and_the_gate_worker_refuse_to_start_naming_the_stamp() {
    let f = Fixture::new("planes-halt-guard");
    for unit in ["spira-rounds.service", "spira-gate-worker.service"] {
        let guards = halt_guard_lines(unit);
        assert_eq!(guards.len(), 1, "{unit} carries exactly one halt check in ExecStartPre: {guards:?}");
        let (ok, out) = f.exec_start_pre(&guards[0]);
        assert!(ok, "positive control: with no halt stamp {unit} starts\n{out}");

        std::fs::write(f.halt_stamp(), "2000-01-01T00:00:00Z\nwhy: planted halt\n").unwrap();
        let (ok, out) = f.exec_start_pre(&guards[0]);
        assert!(!ok, "{unit} must refuse to start while the world is halted\n{out}");
        assert!(out.contains(&f.halt_stamp().display().to_string()), "the refusal names the stamp\n{out}");
        assert!(out.contains("planted halt") && out.contains("world start"), "the refusal names the halt and what lifts it\n{out}");
        std::fs::remove_file(f.halt_stamp()).unwrap();
    }
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

#[test]
fn start_brings_back_a_work_daemon_that_no_timer_starts() {
    let f = Fixture::new("planes-start-daemon");
    f.world(&["stop"]);
    let (out, calls) = f.world(&["start"]);
    // The only shipped work-plane unit that is not a oneshot; a halted boot leaves it failed
    // on its start guard and nothing else ever starts it.
    assert!(calls.iter().any(|c| c == "reset-failed spira-submission-prod.service"), "{out}\n{calls:?}");
    assert!(calls.iter().any(|c| c == "start spira-submission-prod.service"), "{out}\n{calls:?}");
    for oneshot in ["spira-rounds-prod.service", "spira-gate-worker-prod.service", "spira-sentinel-prod.service"] {
        assert!(!calls.iter().any(|c| c == &format!("start {oneshot}")), "a oneshot is its timer's to start: {oneshot}\n{out}");
    }
    assert!(!calls.iter().any(|c| c == "start spira-lc-prod.service" || c == "start spira-cockpit-prod.service"), "no plane, no start\n{out}");
}
