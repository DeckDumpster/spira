//! `round-recert` end to end under no inherited environment: a release whose `queue` keeps one
//! round's state in files and whose `systemd-run`/`systemctl` keep the transient units in files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const QUEUE: &str = r#"#!/bin/sh
S=@S@
echo "queue $*" >> $S/calls
[ "$1" = round ] || exit 64
case "$2" in
status)
    if [ -e $S/open ]; then
        echo "round=open"; echo "phase=$(cat $S/phase)"; echo "batch_id=r-1"; echo "repo=$3"; echo "head=abc123"
    else
        echo "round=none"; echo "repo=$3"
    fi ;;
certify)
    if [ -e $S/red ]; then echo "red-output=/x/red.out"; exit 1; fi
    if [ -e $S/incomplete ]; then echo "NO RESULT"; exit 75; fi
    echo "queue.sh round certify: round $3 is GREEN at abc123"; echo green > $S/phase ;;
land)
    [ "$(cat $S/phase)" = green ] || { echo "not green"; exit 1; }
    [ -e $S/land-3 ] && { rm -f $S/open; exit 3; }
    [ -e $S/land-stays ] && { echo "queue.sh round land: $3 landed"; exit 0; }
    rm -f $S/open; echo "queue.sh round land: $3 landed" ;;
*) exit 64 ;;
esac
"#;

const SYSTEMD_RUN: &str = "#!/bin/sh\necho \"$*\" >> @S@/runs\nfor a in \"$@\"; do case \"$a\" in --unit=*) echo active > @S@/active-${a#--unit=};; esac; done\n";
const SYSTEMCTL: &str = "#!/bin/sh\nfor a in \"$@\"; do u=$a; done\nif [ -e @S@/active-$u ]; then cat @S@/active-$u; else echo inactive; exit 3; fi\n";

struct Fixture {
    _tmp: testkit::TempDir,
    root: PathBuf,
    state: PathBuf,
    release: PathBuf,
    toml: String,
}

fn fixture(name: &str, phase: Option<&str>) -> Fixture {
    let tmp = testkit::TempDir::new(&format!("round-recert-{name}"));
    let root = tmp.path().to_path_buf();
    let (state, release) = (root.join("state"), root.join("release"));
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(release.join("bin")).unwrap();
    for (n, body) in [("queue", QUEUE), ("systemd-run", SYSTEMD_RUN), ("systemctl", SYSTEMCTL)] {
        testkit::write_exe(release.join("bin").join(n), &body.replace("@S@", &state.display().to_string()));
    }
    fs::copy(env!("CARGO_BIN_EXE_round-recert"), release.join("bin/round-recert")).unwrap();
    if let Some(p) = phase {
        fs::write(state.join("open"), "").unwrap();
        fs::write(state.join("phase"), p).unwrap();
    }
    let over = root.join("over.toml");
    fs::write(&over, "[spira]\npath = \"\"\n").unwrap();
    let complete = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira-config/tests/fixtures/complete.toml");
    let toml = format!("{}:{}", complete.display(), over.display());
    Fixture { _tmp: tmp, root, state, release, toml }
}

fn recert(f: &Fixture, args: &[&str]) -> Output {
    let home = f.root.join("home");
    fs::create_dir_all(&home).unwrap();
    testkit::retry_busy(|| Command::new(f.release.join("bin/round-recert")).env_clear().env("HOME", &home).env("SPIRA_TOML", &f.toml).args(args).output()).unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn lines(f: &Fixture, file: &str) -> Vec<String> {
    fs::read_to_string(f.state.join(file)).unwrap_or_default().lines().map(String::from).collect()
}

#[test]
fn an_open_round_with_no_driver_gets_one_unit_and_a_second_call_starts_nothing() {
    let f = fixture("start", Some("certifying"));
    let first = recert(&f, &["--batch", "r-1", "--repo", "spira"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first));
    assert!(text(&first).contains("started round-recert-r-1"), "{}", text(&first));
    let runs = lines(&f, "runs");
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert!(runs[0].contains("--unit=round-recert-r-1") && runs[0].contains("--collect") && runs[0].contains("--user"), "{runs:?}");
    assert!(runs[0].contains("--setenv=SPIRA_TOML=") && runs[0].contains("over.toml"), "{runs:?}");
    assert!(runs[0].ends_with("round-recert --drive --batch r-1 --repo spira"), "{runs:?}");
    assert!(runs[0].contains(&format!("{}/bin/round-recert", f.release.canonicalize().unwrap().display())), "{runs:?}");

    let second = recert(&f, &["--batch", "r-1", "--repo", "spira"]);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second));
    assert!(text(&second).contains("already running round-recert-r-1"), "{}", text(&second));
    assert_eq!(lines(&f, "runs").len(), 1);
}

#[test]
fn a_round_that_is_not_open_is_refused_with_its_state_and_starts_nothing() {
    let none = fixture("none", None);
    let o = recert(&none, &["--batch", "r-1", "--repo", "spira"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("round r-1 is not open in spira (no round is open)"), "{}", text(&o));

    let ended = fixture("ended", Some("landed"));
    let o = recert(&ended, &["--batch", "r-1", "--repo", "spira"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("round r-1 is landed, not open"), "{}", text(&o));

    let other = fixture("other", Some("opened"));
    let o = recert(&other, &["--batch", "r-9", "--repo", "spira"]);
    assert!(text(&o).contains("open: r-1"), "{}", text(&o));
    for f in [&none, &ended, &other] {
        assert!(lines(f, "runs").is_empty());
    }
}

#[test]
fn a_driven_round_that_certifies_green_lands_through_queue_round_land() {
    let f = fixture("green", Some("certifying"));
    let o = recert(&f, &["--drive", "--batch", "r-1", "--repo", "spira"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("r-1 landed at abc123"), "{}", text(&o));
    let verbs: Vec<String> = lines(&f, "calls").into_iter().filter(|l| !l.contains(" status ")).collect();
    assert_eq!(verbs, ["queue round certify r-1 spira", "queue round land r-1 spira"]);
    assert!(!f.state.join("open").exists());
}

#[test]
fn a_driven_round_that_certifies_red_is_left_open_and_never_lands() {
    let f = fixture("red", Some("certifying"));
    fs::write(f.state.join("red"), "").unwrap();
    let o = recert(&f, &["--drive", "--batch", "r-1", "--repo", "spira"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("is not green: RED") && text(&o).contains("round left open"), "{}", text(&o));
    assert!(lines(&f, "calls").iter().all(|l| !l.contains(" land ")));
    assert!(f.state.join("open").exists());
}

#[test]
fn arguments_that_are_not_plain_names_are_refused_before_anything_runs() {
    let f = fixture("args", Some("opened"));
    for args in [vec!["--batch", "../x", "--repo", "spira"], vec!["--batch", "r-1"], vec!["--repo", "spira"], vec!["--batch", "r-1", "--repo", "a b"]] {
        let o = recert(&f, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", text(&o));
    }
    assert!(lines(&f, "calls").is_empty() && lines(&f, "runs").is_empty());
}

#[test]
fn a_pass_that_ends_incomplete_with_no_red_says_so_and_never_lands() {
    let f = fixture("incomplete", Some("certifying"));
    fs::write(f.state.join("incomplete"), "").unwrap();
    let o = recert(&f, &["--drive", "--batch", "r-1", "--repo", "spira"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("incomplete, no red named (certify rc=75)"), "{}", text(&o));
    assert!(lines(&f, "calls").iter().all(|l| !l.contains(" land ")));
}

#[test]
fn a_land_that_exits_zero_with_the_round_still_open_is_not_reported_landed() {
    let f = fixture("stays", Some("certifying"));
    fs::write(f.state.join("land-stays"), "").unwrap();
    let o = recert(&f, &["--drive", "--batch", "r-1", "--repo", "spira"]);
    assert_eq!(o.status.code(), Some(1), "{}", text(&o));
    assert!(text(&o).contains("LAND FAILED") && text(&o).contains("still open"), "{}", text(&o));
}

#[test]
fn a_land_whose_release_was_not_activated_is_reported_landed_with_that_said() {
    let f = fixture("deploy", Some("certifying"));
    fs::write(f.state.join("land-3"), "").unwrap();
    let o = recert(&f, &["--drive", "--batch", "r-1", "--repo", "spira"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("landed at abc123 but its release was not activated"), "{}", text(&o));
}

#[test]
fn a_unit_that_has_finished_is_not_a_driver_so_the_next_call_starts_a_new_one() {
    let f = fixture("finished", Some("certifying"));
    assert!(text(&recert(&f, &["--batch", "r-1", "--repo", "spira"])).contains("started"));
    fs::remove_file(f.state.join("active-round-recert-r-1")).unwrap();
    assert!(text(&recert(&f, &["--batch", "r-1", "--repo", "spira"])).contains("started"));
    assert_eq!(lines(&f, "runs").len(), 2);
    fs::write(f.state.join("active-round-recert-r-1"), "activating\n").unwrap();
    assert!(text(&recert(&f, &["--batch", "r-1", "--repo", "spira"])).contains("already running"));
    assert_eq!(lines(&f, "runs").len(), 2);
}

#[test]
fn the_argv_the_unit_is_started_with_reaches_the_drive_mode_of_the_real_release_binary() {
    let f = fixture("argv", Some("certifying"));
    fs::copy(env!("CARGO_BIN_EXE_release"), f.release.join("bin/release")).unwrap();
    recert(&f, &["--batch", "r-1", "--repo", "spira"]);
    let run = lines(&f, "runs").remove(0);
    let argv: Vec<&str> = run.split(' ').skip_while(|a| !a.ends_with("/bin/round-recert")).skip(1).collect();
    assert_eq!(argv.first(), Some(&"round-recert"), "{run}");
    let home = f.root.join("home");
    let o = testkit::retry_busy(|| Command::new(f.release.join("bin/release")).env_clear().env("HOME", &home).env("SPIRA_TOML", &f.toml).args(&argv).output()).unwrap();
    assert!(!text(&o).contains("unknown flag --drive"), "{}", text(&o));
    assert!(text(&o).contains("round-recert: driving r-1"), "{}", text(&o));
}

#[test]
fn a_unit_started_from_the_bare_round_recert_binary_drives_with_the_argv_it_was_given() {
    let f = fixture("bare", Some("certifying"));
    recert(&f, &["--batch", "r-1", "--repo", "spira"]);
    let run = lines(&f, "runs").remove(0);
    let argv: Vec<&str> = run.split(' ').skip_while(|a| !a.ends_with("/bin/round-recert")).skip(1).collect();
    let o = recert(&f, &argv);
    assert!(text(&o).contains("round-recert: driving r-1"), "{}", text(&o));
}
