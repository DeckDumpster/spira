//! `spira-build-dir lease` is the hand-build door onto the pool dwarves lease from.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
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

#[test_id(tqvbzcp)]
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

#[test_id(tiw0ppl)]
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

#[test_id(tvhsbzd)]
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

fn touched_after_record(dir: &Path, file: &Path) {
    let record = std::fs::metadata(dir.join(".unit-tests-built-manifest")).unwrap().modified().unwrap();
    // clock-safe: the mtime is set relative to the record, not to the wall clock
    let f = std::fs::File::options().write(true).open(file).unwrap();
    f.set_modified(record + Duration::from_secs(1)).unwrap();
}

#[test_id(t7idqqx)]
#[test]
fn a_file_rewritten_with_the_same_bytes_during_the_lease_is_cleaned_on_release() {
    let b = setup("bdcli-same-bytes");
    let mut p1 = sleeper();
    let (d1, _) = b.lease("hand-1", &p1);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("a/src/lib.rs"), LEASE_BYTES).unwrap();
    touched_after_record(&d1, &b.tree.join("a/src/lib.rs"));
    release(&b, &mut p1);
    assert_eq!(cargo_log(&b), "clean -q -p crate-a");
}

#[test_id(tx3uue5)]
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

#[test_id(tlf3w20)]
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
    let (d2, _) = b.lease("hand-2", &p2);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(root.join("doc.txt"), "doc\n").unwrap();
    touched_after_record(&d2, &root.join("doc.txt"));
    release(&b, &mut p2);
    assert_eq!(cargo_log(&b), "clean -q -p crate-a");
}

fn unit(dir: &Path, name: &str, stamp: Option<std::time::SystemTime>) -> PathBuf {
    let u = dir.join("debug/.fingerprint").join(name);
    std::fs::create_dir_all(&u).unwrap();
    if let Some(t) = stamp {
        let f = std::fs::File::create(u.join("invoked.timestamp")).unwrap();
        f.set_modified(t).unwrap();
    }
    u
}

fn edit_a_in_lease(b: &Box_) -> (Child, PathBuf) {
    let p = sleeper();
    let (d, _) = b.lease("hand-1", &p);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("a/src/lib.rs"), "pub fn f() { let _ = 3; }\n").unwrap();
    (p, d)
}

#[test_id(tkq3m7a)]
#[test]
fn a_unit_stamped_after_the_edit_survives_release() {
    let b = setup("bdcli-unit-kept");
    let (mut p, d) = edit_a_in_lease(&b);
    let later = std::time::SystemTime::now() + Duration::from_secs(3600);
    let u = unit(&d, "crate-a-0123456789abcdef", Some(later));
    release(&b, &mut p);
    assert!(u.exists(), "the fresh unit stays");
    assert!(!b.log.exists(), "nothing is cleaned: {}", cargo_log(&b));
}

#[test_id(tr5n2xe)]
#[test]
fn a_unit_stamped_before_the_edit_loses_its_fingerprint_and_a_newer_sibling_stays() {
    let b = setup("bdcli-unit-stale");
    let (mut p, d) = edit_a_in_lease(&b);
    let now = std::time::SystemTime::now();
    let stale = unit(&d, "crate-a-0123456789abcdef", Some(now - Duration::from_secs(3600)));
    let fresh = unit(&d, "crate-a-fedcba9876543210", Some(now + Duration::from_secs(3600)));
    release(&b, &mut p);
    assert!(!stale.exists(), "the stale unit is removed");
    assert!(fresh.exists(), "the newer sibling stays");
    assert!(!b.log.exists(), "a crate with a kept unit is not cleaned: {}", cargo_log(&b));
}

#[test_id(tz8v1cd)]
#[test]
fn an_edit_with_a_back_dated_mtime_after_the_stamp_drops_the_unit() {
    let b = setup("bdcli-unit-backdated");
    let p = sleeper();
    let (d, _) = b.lease("hand-1", &p);
    let mut p = p;
    let _ = std::fs::remove_file(&b.log);
    let u = unit(&d, "crate-a-0123456789abcdef", Some(std::time::SystemTime::now() + Duration::from_secs(60)));
    let f = std::fs::File::options().write(true).open(b.tree.join("a/src/lib.rs")).unwrap();
    std::fs::write(b.tree.join("a/src/lib.rs"), "pub fn f() { let _ = 4; }\n").unwrap();
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(7200)).unwrap();
    // ctime is now; push the stamp behind it
    std::fs::File::options().write(true).open(u.join("invoked.timestamp")).unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)).unwrap();
    release(&b, &mut p);
    assert!(!u.exists(), "ctime newer than the stamp drops the unit");
    assert_eq!(cargo_log(&b), "clean -q -p crate-a");
}

#[test_id(tm4w9ze)]
#[test]
fn a_unit_with_no_timestamp_is_removed_and_a_deleted_file_cleans_its_crate() {
    let b = setup("bdcli-unit-nostamp");
    let (mut p, d) = edit_a_in_lease(&b);
    let bare = unit(&d, "crate-a-0123456789abcdef", None);
    release(&b, &mut p);
    assert!(!bare.exists(), "a unit with no timestamp is removed");
    assert_eq!(cargo_log(&b), "clean -q -p crate-a");

    let mut p2 = sleeper();
    let (d2, _) = b.lease("hand-2", &p2);
    let _ = std::fs::remove_file(&b.log);
    std::fs::remove_file(b.tree.join("b/src/lib.rs")).unwrap();
    let fresh = unit(&d2, "crate-b-0123456789abcdef", Some(std::time::SystemTime::now() + Duration::from_secs(3600)));
    release(&b, &mut p2);
    assert!(fresh.exists());
    assert_eq!(cargo_log(&b), "clean -q -p crate-b", "a deleted file cleans the crate whatever its units");
}

fn clean_tree(b: &Box_, dir: &Path) -> String {
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    spira_config::build_dir::clean(&home.join(spira_config::build_dir::CLEAN_SCRIPT), &b.tree, dir, &b.bin, false).unwrap()
}

#[test_id(thvvkjk)]
#[test]
fn a_unit_built_since_the_last_recorded_run_is_rebuilt_though_no_bytes_changed() {
    let b = setup("bdcli-foreign-build");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let _ = std::fs::remove_file(&b.log);
    let later = std::time::SystemTime::now() + Duration::from_secs(3600);
    let u = unit(&d, "crate-a-0123456789abcdef", Some(later));
    let said = clean_tree(&b, &d);
    assert!(!u.exists(), "the unit another build made is dropped: {said}");
    assert!(
        said.lines().any(|l| l.contains("built since the last recorded run") && l.ends_with("rebuilding: crate-a")),
        "{said}"
    );
    assert!(!b.log.exists(), "nothing is cargo-cleaned: {}", cargo_log(&b));
}

#[test_id(t2bk9e8)]
#[test]
fn a_unit_built_before_the_last_recorded_run_stays_warm() {
    let b = setup("bdcli-own-build");
    let d = b.base.join("dir.1");
    let earlier = std::time::SystemTime::now() - Duration::from_secs(3600);
    let u = unit(&d, "crate-a-0123456789abcdef", Some(earlier));
    clean_tree(&b, &d);
    let _ = std::fs::remove_file(&b.log);
    let said = clean_tree(&b, &d);
    assert!(u.exists(), "{said}");
    assert!(!said.contains("built since the last recorded run"), "{said}");
    assert!(!b.log.exists(), "nothing is cleaned: {}", cargo_log(&b));
}
