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

const LEASE_BYTES: &str = "pub fn f() {}\n";

fn release(b: &Box_, holder: &mut Child) {
    let _ = (holder.kill(), holder.wait());
    wait_released(b);
}

fn cargo_log(b: &Box_) -> String {
    std::fs::read_to_string(&b.log).unwrap_or_default().trim().to_string()
}

#[test]
fn a_release_cleans_a_crate_changed_during_the_lease_and_the_next_lease_trusts_the_record() {
    let b = setup("bdcli-record");
    let mut p1 = sleeper();
    let (d1, _) = b.lease("hand-1", &p1);
    release(&b, &mut p1);

    let _ = std::fs::remove_file(&b.log);
    let mut p2 = sleeper();
    let (d2, err) = b.lease("hand-2", &p2);
    assert_eq!(d2, d1, "the released directory is reused");
    assert!(!b.log.exists(), "the same tree again cleans nothing: {err}");
    release(&b, &mut p2);
    assert!(!b.log.exists(), "an unchanged tree cleans nothing on release");

    let mut p3 = sleeper();
    b.lease("hand-3", &p3);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("a/src/lib.rs"), "pub fn f() { let _ = 2; }\n").unwrap();
    release(&b, &mut p3);
    assert_eq!(cargo_log(&b), "clean -q -p crate-a", "release cleans only the crate changed in the lease");

    let a = std::fs::File::options().write(true).open(b.tree.join("a/src/lib.rs")).unwrap();
    std::fs::write(b.tree.join("a/src/lib.rs"), LEASE_BYTES).unwrap();
    a.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)).unwrap();
    let _ = std::fs::remove_file(&b.log);
    let mut p4 = sleeper();
    let (d4, err) = b.lease("hand-4", &p4);
    assert_eq!(d4, d1);
    assert_eq!(
        cargo_log(&b),
        "clean -q -p crate-a",
        "bytes that differ from the release record are cleaned: {err}"
    );
    let _ = (p4.kill(), p4.wait());
}

#[test]
fn a_file_rewritten_with_the_same_bytes_during_the_lease_is_cleaned_on_release() {
    let b = setup("bdcli-same-bytes");
    let mut p1 = sleeper();
    b.lease("hand-1", &p1);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("a/src/lib.rs"), LEASE_BYTES).unwrap();
    release(&b, &mut p1);
    assert_eq!(cargo_log(&b), "clean -q -p crate-a");
}

#[test]
fn a_lockfile_change_during_the_lease_cleans_the_workspace_on_release() {
    let b = setup("bdcli-lock");
    let mut p1 = sleeper();
    b.lease("hand-1", &p1);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("Cargo.lock"), "# lock changed\n").unwrap();
    release(&b, &mut p1);
    assert_eq!(cargo_log(&b), "clean --workspace -q");
}

#[test]
fn a_file_outside_every_crate_that_a_crate_read_is_cleaned_on_release_when_touched() {
    let b = setup("bdcli-extras");
    let root = std::fs::canonicalize(&b.tree).unwrap();
    std::fs::write(root.join("doc.txt"), "doc\n").unwrap();
    let mut p1 = sleeper();
    let (d1, _) = b.lease("hand-1", &p1);
    std::fs::create_dir_all(d1.join("debug/deps")).unwrap();
    std::fs::write(
        d1.join("debug/deps/crate_a-1.d"),
        format!("x: {0}/a/src/lib.rs {0}/doc.txt\n", root.display()),
    )
    .unwrap();
    release(&b, &mut p1);

    let mut p2 = sleeper();
    b.lease("hand-2", &p2);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(root.join("doc.txt"), "doc\n").unwrap();
    release(&b, &mut p2);
    assert_eq!(cargo_log(&b), "clean -q -p crate-a");
}
