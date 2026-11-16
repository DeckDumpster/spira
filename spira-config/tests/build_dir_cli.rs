//! `spira-build-dir lease` is the hand-build door onto the pool dwarves lease from.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Box_ {
    _d: testkit::TempDir,
    toml: PathBuf,
    base: PathBuf,
    bin: String,
    log: PathBuf,
    tree: PathBuf,
}

fn setup(name: &str) -> Box_ {
    let d = testkit::TempDir::new(name);
    let base = d.path().join("pool");
    let toml = spira_config::process::fixture_toml(
        d.path(),
        &[
            ("SPIRA_DWARF_BUILD_BASE", base.to_str().unwrap()),
            ("SPIRA_DWARF_BUILD_BUDGET_GIB", "60"),
        ],
    );
    let bindir = d.path().join("bin");
    let log = d.path().join("cargo.log");
    std::fs::create_dir_all(&bindir).unwrap();
    testkit::write_exe(
        bindir.join("cargo"),
        &format!("#!/bin/sh\necho \"$*\" >> {}\n", log.display()),
    );
    let tree = d.path().join("tree");
    for c in ["a", "b"] {
        std::fs::create_dir_all(tree.join(c).join("src")).unwrap();
        std::fs::write(
            tree.join(c).join("Cargo.toml"),
            format!("[package]\nname = \"crate-{c}\"\n"),
        )
        .unwrap();
        std::fs::write(tree.join(c).join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    }
    std::fs::write(
        tree.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\n",
    )
    .unwrap();
    std::fs::write(tree.join("Cargo.lock"), "# lock\n").unwrap();
    let bin = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Box_ {
        _d: d,
        toml,
        base,
        bin,
        log,
        tree,
    }
}

impl Box_ {
    fn lease(&self, who: &str, holder: &Child) -> (PathBuf, String) {
        let home = spira_config::resolve::locate_home_for_process().unwrap();
        let out = Command::new(testkit::bin_exe("spira-build-dir"))
            .args([
                "lease",
                "--who",
                who,
                "--pid",
                &holder.id().to_string(),
                "--root",
            ])
            .arg(&self.tree)
            .env_clear()
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", &home)
            .env("PATH", &self.bin)
            .output()
            .expect("spira-build-dir runs");
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(out.status.success(), "lease failed: {err}");
        (
            PathBuf::from(String::from_utf8(out.stdout).unwrap().trim()),
            err,
        )
    }
}

fn sleeper() -> Child {
    Command::new("sleep")
        .arg("60")
        .stdout(Stdio::null())
        .spawn()
        .unwrap()
}

fn wait_released(b: &Box_) {
    // The holder notices the exit, records the tree and empties the lock file.
    let lock = b.base.join("dir.1.lock");
    let deadline = Instant::now() + Duration::from_secs(20); // clock-safe: a cap on waiting for the holder to empty the lock file
    while std::fs::metadata(&lock).map_or(0, |m| m.len()) > 0 && Instant::now() < deadline {
        // clock-safe: cap on the wait for the marker file
        std::thread::sleep(Duration::from_millis(100)); // clock-safe: poll of the marker file
    }
    assert_eq!(
        std::fs::metadata(&lock).map_or(0, |m| m.len()),
        0,
        "the holder released"
    );
}

#[test]
fn two_concurrent_hand_leases_get_different_owned_directories() {
    let b = setup("bdcli-two");
    let (mut p1, mut p2) = (sleeper(), sleeper());
    let (d1, _) = b.lease("hand-1", &p1);
    let (d2, _) = b.lease("hand-2", &p2);
    assert_ne!(d1, d2, "a live lease is never handed out twice");
    assert!(
        d1.starts_with(&b.base) && d2.starts_with(&b.base),
        "both come from the dwarves' pool"
    );
    let _ = (p1.kill(), p2.kill(), p1.wait(), p2.wait());
}

#[test]
fn a_lease_after_the_holder_exits_reuses_the_warm_directory_with_a_selective_clean() {
    let b = setup("bdcli-warm");
    let mut p1 = sleeper();
    let (d1, err) = b.lease("hand-1", &p1);
    assert!(err.contains("— new"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&b.log).unwrap().trim(),
        "clean --workspace -q",
        "a target with no record cleans the whole workspace: {err}"
    );
    std::fs::create_dir_all(d1.join("debug/deps")).unwrap();
    std::fs::write(d1.join("debug/deps/libdep.rlib"), "dep").unwrap();
    let _ = (p1.kill(), p1.wait());
    wait_released(&b);
    std::fs::write(b.tree.join("b/src/lib.rs"), "pub fn f() { let _ = 1; }\n").unwrap();
    let _ = std::fs::remove_file(&b.log);
    let mut p2 = sleeper();
    let (d2, err) = b.lease("hand-2", &p2);
    assert_eq!(d2, d1, "the released directory is reused");
    assert!(err.contains("— warm"), "{err}");
    assert!(
        d1.join("debug/deps/libdep.rlib").exists(),
        "dependencies stay warm"
    );
    assert_eq!(
        std::fs::read_to_string(&b.log).unwrap().trim(),
        "clean -q -p crate-b",
        "only the changed crate is cleaned: {err}"
    );
    let _ = (p2.kill(), p2.wait());
}

#[test]
fn an_unchanged_tree_cleans_nothing_and_a_release_records_a_change_without_cleaning() {
    let b = setup("bdcli-record");
    let mut p1 = sleeper();
    let (d1, _) = b.lease("hand-1", &p1);
    let _ = (p1.kill(), p1.wait());
    wait_released(&b);

    let _ = std::fs::remove_file(&b.log);
    let mut p2 = sleeper();
    let (d2, err) = b.lease("hand-2", &p2);
    assert_eq!(d2, d1, "the released directory is reused");
    assert!(!b.log.exists(), "the same tree again cleans nothing: {err}");

    std::fs::write(b.tree.join("Cargo.lock"), "# lock changed\n").unwrap();
    let _ = (p2.kill(), p2.wait());
    wait_released(&b);
    assert!(
        !b.log.exists(),
        "a release records the change and never cleans"
    );

    let mut p3 = sleeper();
    let (d3, err) = b.lease("hand-3", &p3);
    assert_eq!(d3, d1, "the released directory is reused");
    assert!(
        !b.log.exists(),
        "the change the release recorded cleans nothing on the next lease: {err}"
    );
    let _ = (p3.kill(), p3.wait());
}
