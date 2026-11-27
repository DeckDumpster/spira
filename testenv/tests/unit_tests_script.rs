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

/// In the container the script's user can neither create nor remove a file in the checkout's
/// `target/`: it can only rewrite the copy testenv left there.
#[test_id(tsvwqv5)]
#[test]
fn a_run_that_cannot_write_target_still_rewrites_the_copy_testenv_left() {
    two_runs(0, true);
}

fn two_runs_keep_the_unit(test_exit: i32) {
    two_runs(test_exit, false);
}

fn two_runs(test_exit: i32, locked_target: bool) {
    let d = testkit::TempDir::new("unit-tests-script");
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
        ("target/unit-tests.build", "true\n"),
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
    let copy = tree.join("target/unit-tests.clean.json");
    let lock = |mode| std::fs::set_permissions(tree.join("target"), std::fs::Permissions::from_mode(mode)).unwrap();
    if locked_target {
        testenv::unit::prepare_clean_report(&tree).unwrap();
        assert_eq!(std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777, 0o666, "another uid can rewrite it");
        lock(0o555);
    }
    let report = || spira_config::build_dir::clean_report_at(&copy);
    let target = d.path().join("cargo-target");
    let unit = target.join("debug/.fingerprint/crate-a-0123456789abcdef");
    let bin = d.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    testkit::write_exe(
        bin.join("cargo"),
        &format!(
            "#!/bin/sh\n[ \"$1\" = test ] || exit 0\nmkdir -p {0} && touch {0}/invoked.timestamp\nexit {test_exit}\n",
            unit.display()
        ),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let run = || {
        let out = Command::new("bash")
            .arg(tree.join("spira/unit-tests.sh"))
            .env_clear()
            .env("PATH", &path)
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .expect("unit-tests.sh runs");
        let said = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(test_exit), "{said}");
        said
    };
    run();
    assert!(unit.exists(), "the stub built a unit");
    assert!(copy.exists(), "the run leaves a copy of its clean's report");
    let first = report().expect("the copy parses");
    let acts: Vec<(&str, &str)> = first.acts.iter().map(|a| (a.branch.as_str(), a.cause.as_str())).collect();
    assert_eq!(acts, [("whole-workspace", "missing-record")]);
    // The stub's `test` would recreate a dropped unit, so the message and a marker tell.
    std::fs::write(unit.join("marker"), "first run").unwrap();
    let said = run();
    assert!(unit.join("marker").exists(), "the second run keeps the unit: {said}");
    assert!(!said.contains("built since"), "{said}");
    assert_eq!(report().expect("the second copy parses").acts, [], "{said}");
    if locked_target {
        lock(0o755);
    }
}
