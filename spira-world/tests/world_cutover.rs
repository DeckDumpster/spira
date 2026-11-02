//! `world cutover-mode` and `world stop` through the real `world` binary, against a stub
//! systemctl and a fixture registry: what the verbs refuse, the stops and record they write, and
//! that a stop puts the world back exactly as cutover mode found it.

use std::path::{Path, PathBuf};
use std::process::Command;

use spira_config::scoped_stop;

struct Fixture {
    tmp: testkit::TempDir,
    toml: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let tmp = testkit::TempDir::new(name);
        for d in ["home", "run"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        let map = tmp.join("registered-repos");
        std::fs::write(&map, format!("alpha|{0}/alpha|queue.local\nbeta|{0}/beta|queue.local\n", tmp.path().display())).unwrap();
        let run = tmp.join("run").display().to_string();
        let toml = spira_config::process::fixture_toml(tmp.path(), &[("SPIRA_RUN", &run), ("SPIRA_REPO_MAP", &map.display().to_string()), ("SPIRA_HOME_REPO", "alpha")]);
        testkit::write_exe(
            tmp.join("systemctl"),
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
list-units) [ "$arg" = 'spira-dwarf-*' ] && [ -f "$LIVE_UNIT" ] && echo "spira-dwarf-builder-sp-live.service loaded active running dwarf" ;;
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

    fn run(&self) -> PathBuf {
        self.tmp.join("run")
    }

    fn world(&self, args: &[&str]) -> (i32, String) {
        let bin_dir = PathBuf::from(env!("CARGO_BIN_EXE_world")).parent().unwrap().to_path_buf();
        let real_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let out = Command::new("env")
            .arg("-i")
            .arg(format!("HOME={}", self.tmp.join("home").display()))
            .arg(format!("PATH={}:/usr/bin:/bin", bin_dir.display()))
            .arg(format!("SPIRA_HOME={}", real_home.display()))
            .arg(format!("SPIRA_TOML={}", self.toml.display()))
            .arg(format!("SPIRA_SYSTEMCTL={}", self.tmp.join("systemctl").display()))
            .arg(format!("UNIT_DIR={}", Path::new(env!("CARGO_MANIFEST_DIR")).join("../systemd").display()))
            .arg(format!("CALLS={}", self.tmp.join("calls").display()))
            .arg(format!("LIVE_UNIT={}", self.tmp.join("live-unit").display()))
            .arg(format!("SPIRA_CTRL={}", self.tmp.join("no-ctrl.json").display()))
            .arg("world")
            .args(args)
            .output()
            .expect("cannot spawn world");
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
    }

    fn halt(&self) {
        std::fs::write(self.run().join("world.halted"), "2026-10-31T00:00:00Z\nwhy: cutover\n").unwrap();
    }

    fn enter(&self) -> (i32, String) {
        self.world(&["cutover-mode", "--rev", "r42", "--reason", "controller cutover"])
    }
}

#[test]
fn cutover_mode_refuses_without_the_halt_stamp() {
    let f = Fixture::new("world-cutover-no-stamp");
    let (rc, out) = f.enter();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("not stopped"), "{out}");
    assert!(scoped_stop::repo_stops(&f.run()).is_empty(), "a refusal writes no stop\n{out}");
    assert!(!f.run().join("world.cutover-mode").exists());
}

#[test]
fn cutover_mode_refuses_with_a_live_dwarf() {
    let f = Fixture::new("world-cutover-live-dwarf");
    f.halt();
    std::fs::write(f.tmp.join("live-unit"), "").unwrap();
    let (rc, out) = f.enter();
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("spira-dwarf-builder-sp-live"), "the refusal names the live dwarf\n{out}");
    assert!(f.run().join("world.halted").is_file(), "the stamp stays");
    assert!(scoped_stop::repo_stops(&f.run()).is_empty());
}

#[test]
fn cutover_mode_writes_one_stop_per_repo_and_the_record_then_lifts_the_stamp() {
    let f = Fixture::new("world-cutover-enter");
    f.halt();
    let (rc, out) = f.enter();
    assert_eq!(rc, 0, "{out}");
    assert!(!f.run().join("world.halted").exists(), "the stamp is lifted for the window\n{out}");
    let stops = scoped_stop::repo_stops(&f.run());
    let repos: Vec<&str> = stops.iter().map(|s| s.repo.as_str()).collect();
    assert!(repos.contains(&"alpha") && repos.contains(&"beta"), "every registered repo is held: {repos:?}\n{out}");
    for s in &stops {
        assert_eq!(s.lanes, spira_world::cutover::LANES.to_vec(), "every non-round lane: {s:?}");
        assert!(!s.lanes.iter().any(|l| l == "rounds"));
        assert_eq!(s.owner, "cutover-mode");
    }
    let rec = spira_world::cutover::read(&f.run()).unwrap().expect("the record");
    assert_eq!((rec.rev.as_str(), rec.reason.as_str(), rec.stops.len()), ("r42", "controller cutover", stops.len()));
    assert!(!std::fs::read_to_string(f.tmp.join("calls")).unwrap_or_default().contains("start"), "cutover mode starts nothing");
    let (_, status) = f.world(&["status"]);
    assert!(status.contains("CUTOVER MODE") && status.contains("r42"), "{status}");
    let (rc, out) = f.world(&["start"]);
    assert_eq!(rc, 1, "start refuses in cutover mode\n{out}");
    assert!(out.contains("cutover mode"), "{out}");
}

#[test]
fn world_stop_restores_the_stamp_and_lifts_exactly_the_cutover_stops() {
    let f = Fixture::new("world-cutover-stop");
    let own = scoped_stop::pause_repo(&f.run(), "beta", &["rounds".to_string()], "concierge", "drain beta", "fix lands", 9).unwrap();
    f.halt();
    let (rc, out) = f.enter();
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = f.world(&["stop", "--why", "cutover done"]);
    assert_eq!(rc, 0, "{out}");
    assert!(f.run().join("world.halted").is_file(), "the halt stamp is back\n{out}");
    assert!(!f.run().join("world.cutover-mode").exists(), "the record is gone\n{out}");
    assert_eq!(scoped_stop::repo_stops(&f.run()), vec![own], "only the stop cutover mode found is left\n{out}");
}
