//! `spira/unit-tests.sh` against a stub cargo: what its own run builds is not taken for a
//! build its record does not name. It spawns the script because the script is what is under
//! test: the order of its clean, its builds and its stamp exists nowhere else.

#[macro_use]
extern crate spira_test_id;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test_id(tkp9rdl)]
#[test]
fn a_second_run_keeps_the_units_the_first_run_built() {
    two_runs_keep_the_unit(0);
}

#[test_id(tsm9nv9)]
#[test]
fn a_red_run_exits_with_cargo_tests_status_and_still_keeps_its_units() {
    two_runs_keep_the_unit(101);
}

#[test_id(tagsbid)]
#[test]
fn a_second_run_after_the_root_manifest_alone_differs_keeps_the_units() {
    two_runs(0, false, Some("[workspace]\nmembers = [\"a\"]\n# another tree's manifest\n"));
}

/// In the container the script's user can neither create nor remove a file in the checkout's
/// `target/`: it can only rewrite the copy testenv left there.
#[test_id(tsvwqv5)]
#[test]
fn a_run_that_cannot_write_target_still_rewrites_the_copy_testenv_left() {
    two_runs(0, true, None);
}

#[test_id(tex2gv1)]
#[test]
fn a_clean_that_fails_without_a_report_leaves_no_stale_copy() {
    failed_clean("exit 1", None);
}

#[test_id(thddzw4)]
#[test]
fn a_clean_that_fails_after_removing_something_still_leaves_its_report() {
    let report = r#"{"at":"2026-09-29T19:18:40.000000000Z","rc":1,"acts":[]}"#;
    failed_clean(&format!("echo '{report}' > \"$CARGO_TARGET_DIR/.target-clean-report.json\"; exit 1"), Some(1));
}

/// Both ways the script can meet an old copy: one it can remove, and one it can only rewrite.
fn failed_clean(clean: &str, report_rc: Option<i32>) {
    for mode in [0o755, 0o555] {
        let d = testkit::TempDir::new("unit-tests-script-clean");
        let tree = d.path().join("tree");
        let target = d.path().join("cargo-target");
        for dir in [tree.join("spira"), tree.join("target"), target.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let home = spira_config::resolve::locate_home_for_process().unwrap();
        std::fs::copy(home.join("unit-tests.sh"), tree.join("spira/unit-tests.sh")).unwrap();
        std::fs::write(tree.join("spira/target-clean.sh"), format!("{clean}\n")).unwrap();
        std::fs::write(tree.join("target/unit-tests.crates"), "crate-a\n").unwrap();
        let copy = tree.join("target/unit-tests.clean.json");
        std::fs::write(&copy, r#"{"at":"2026-09-29T19:18:40.000000000Z","rc":0,"acts":[]}"#).unwrap();
        assert!(spira_config::build_dir::clean_report_at(&copy).is_some(), "positive control: the old copy parses");
        let lock = |m| std::fs::set_permissions(tree.join("target"), std::fs::Permissions::from_mode(m)).unwrap();
        lock(mode);
        let out = Command::new("bash")
            .arg(tree.join("spira/unit-tests.sh"))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .expect("unit-tests.sh runs");
        lock(0o755);
        assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(spira_config::build_dir::clean_report_at(&copy).map(|r| r.rc), report_rc, "target mode {mode:o}");
    }
}

#[test_id(txl9uww)]
#[test]
fn a_run_whose_build_step_fails_records_no_build_seconds() {
    let f = fixture("unit-tests-script-red-build", "false\n");
    let out = f.run(&f.d.path().join("cargo-target"));
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!f.d.path().join("cargo-target/debug/.lease-first-build-secs").exists());
}

/// A tree's own target whose `debug/` links into a leased folder.
#[test_id(tvlus2z)]
#[test]
fn a_run_through_a_linked_debug_directory_records_the_seconds_in_the_leased_one() {
    let f = fixture("unit-tests-script-linked", "true\n");
    let leased = f.d.path().join("lease/debug");
    std::fs::create_dir_all(&leased).unwrap();
    std::os::unix::fs::symlink(&leased, f.tree.join("target/debug")).unwrap();
    let out = f.run(&f.tree.join("target"));
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(f.d.path().join("lease/debug/.lease-first-build-secs").is_file());
}

fn two_runs_keep_the_unit(test_exit: i32) {
    two_runs(test_exit, false, None);
}

struct Fixture {
    d: testkit::TempDir,
    tree: std::path::PathBuf,
}

/// A one-crate tree holding the scripts under test, with `build` as its build step.
fn fixture(name: &str, build: &str) -> Fixture {
    let d = testkit::TempDir::new(name);
    let tree = d.path().join("tree");
    std::fs::create_dir_all(tree.join("a/src")).unwrap();
    std::fs::create_dir_all(tree.join("target")).unwrap();
    for (f, body) in [
        ("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n"),
        ("Cargo.lock", "# lock\n"),
        ("a/Cargo.toml", "[package]\nname = \"crate-a\"\n"),
        ("a/src/lib.rs", "pub fn f() {}\n"),
        ("target/unit-tests.crates", "crate-a\n"),
        ("target/unit-tests.bins", ""),
        ("target/unit-tests.build", build),
    ] {
        std::fs::write(tree.join(f), body).unwrap();
    }
    // The script takes the tree to be its own parent's parent.
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    std::fs::create_dir_all(tree.join("spira")).unwrap();
    for script in ["unit-tests.sh", "target-clean.sh"] {
        std::fs::copy(home.join(script), tree.join("spira").join(script)).unwrap();
    }
    // The sandbox entry needs bwrap; this stand-in only runs what follows `--around`.
    std::fs::create_dir_all(tree.join("tools/test-sandbox")).unwrap();
    testkit::write_exe(tree.join("tools/test-sandbox/runner"), "#!/bin/sh\nshift\nexec \"$@\"\n");
    std::fs::create_dir_all(d.path().join("bin")).unwrap();
    testkit::write_exe(d.path().join("bin/cargo"), "#!/bin/sh\nexit 0\n");
    Fixture { d, tree }
}

impl Fixture {
    fn run(&self, target: &std::path::Path) -> std::process::Output {
        let path = format!("{}:{}", self.d.path().join("bin").display(), std::env::var("PATH").unwrap_or_default());
        Command::new("bash")
            .arg(self.tree.join("spira/unit-tests.sh"))
            .env_clear()
            .env("PATH", path)
            .env("CARGO_TARGET_DIR", target)
            .output()
            .expect("unit-tests.sh runs")
    }
}

fn two_runs(test_exit: i32, locked_target: bool, root_manifest_between: Option<&str>) {
    let f = fixture("unit-tests-script", "true\n");
    let (d, tree) = (&f.d, &f.tree);
    let copy = tree.join("target/unit-tests.clean.json");
    let lock = |mode| std::fs::set_permissions(tree.join("target"), std::fs::Permissions::from_mode(mode)).unwrap();
    if locked_target {
        testenv::unit::prepare_clean_report(tree).unwrap();
        assert_eq!(std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777, 0o666, "another uid can rewrite it");
        lock(0o555);
    }
    let report = || spira_config::build_dir::clean_report_at(&copy);
    let target = d.path().join("cargo-target");
    let unit = target.join("debug/.fingerprint/crate-a-0123456789abcdef");
    testkit::write_exe(
        d.path().join("bin/cargo"),
        &format!(
            "#!/bin/sh\n[ \"$1\" = test ] || exit 0\nmkdir -p {0} && touch {0}/invoked.timestamp\nexit {test_exit}\n",
            unit.display()
        ),
    );
    let run = || {
        let out = f.run(&target);
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(test_exit), "{said}");
        said
    };
    run();
    let noted = target.join("debug/.lease-first-build-secs");
    let secs = std::fs::read_to_string(&noted).expect("the first run records its build's seconds");
    assert!(secs.trim().parse::<u64>().is_ok(), "{secs:?}");
    std::fs::write(&noted, "999\n").unwrap();
    assert!(unit.exists(), "the stub built a unit");
    assert!(copy.exists(), "the run leaves a copy of its clean's report");
    let first = report().expect("the copy parses");
    let acts: Vec<(&str, &str)> = first.acts.iter().map(|a| (a.branch.as_str(), a.cause.as_str())).collect();
    assert_eq!(acts, [("whole-workspace", "missing-record")]);
    // The stub's `test` would recreate a dropped unit, so the message and a marker tell.
    std::fs::write(unit.join("marker"), "first run").unwrap();
    if let Some(manifest) = root_manifest_between {
        std::fs::write(tree.join("Cargo.toml"), manifest).unwrap();
    }
    let said = run();
    assert_eq!(std::fs::read_to_string(&noted).unwrap(), "999\n", "the first build wins");
    assert!(unit.join("marker").exists(), "the second run keeps the unit: {said}");
    assert!(!said.contains("built since"), "{said}");
    assert_eq!(report().expect("the second copy parses").acts, [], "{said}");
    if locked_target {
        lock(0o755);
    }
}
