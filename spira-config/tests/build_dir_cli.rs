//! `spira-build-dir lease` is the hand-build door onto the pool dwarves lease from.

#[macro_use]
extern crate spira_test_id;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A cargo that logs its arguments and, for `clean`, prints `summary` as cargo does.
fn stub_cargo(log: &Path, summary: Option<&str>) -> String {
    let said = summary.map_or(String::new(), |l| format!("[ \"$1\" != clean ] || echo '     {l}' >&2\n"));
    format!("#!/bin/sh\necho \"$*\" >> {}\n{said}", log.display())
}

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
            ("SPIRA_DWARF_BUILD_FOLDERS_PER_REPO", "5"),
        ],
    );
    let bindir = d.path().join("bin");
    let log = d.path().join("cargo.log");
    std::fs::create_dir_all(&bindir).unwrap();
    testkit::write_exe(
        bindir.join("cargo"),
        &stub_cargo(&log, Some("Removed 3 files, 1.5GiB total")),
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
    fn run(&self, who: &str, holder: &Child, extra: &[&str]) -> std::process::Output {
        let home = spira_config::resolve::locate_home_for_process().unwrap();
        Command::new(testkit::bin_exe("spira-build-dir"))
            .args([
                "lease",
                "--who",
                who,
                "--pid",
                &holder.id().to_string(),
                "--root",
            ])
            .arg(&self.tree)
            .args(extra)
            .env_clear()
            .env("SPIRA_TOML", &self.toml)
            .env("SPIRA_HOME", &home)
            .env("PATH", &self.bin)
            .output()
            .expect("spira-build-dir runs")
    }

    fn lease(&self, who: &str, holder: &Child) -> (PathBuf, String) {
        let out = self.run(who, holder, &["--repo", "spira"]);
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
    let lock = b.base.join("spira/1.lock");
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

#[test_id(tqkebmx)]
#[test]
fn a_hand_lease_without_a_repository_is_refused_with_the_usage() {
    let b = setup("bdcli-no-repo");
    let mut p = sleeper();
    let out = b.run("hand-1", &p, &[]);
    let _ = (p.kill(), p.wait());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("usage: spira-build-dir lease") && err.contains(" --repo <name>") && !err.contains("[--repo"), "{err}");
    assert!(out.stdout.is_empty() && !b.base.exists(), "nothing is leased");
}

#[test_id(tf1jg2k)]
#[test]
fn a_hand_lease_prints_the_first_folder_of_the_repository_it_names() {
    let b = setup("bdcli-repo-path");
    let mut p = sleeper();
    let (dir, err) = b.lease("hand-1", &p);
    assert_eq!(dir, b.base.join("spira/1"), "{err}");
    release(&b, &mut p);
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
        "clean --workspace",
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
        "clean -p crate-b",
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
    assert_eq!(cargo_log(&b), "clean -p crate-a", "release cleans only the crate changed in the lease");

    let a = std::fs::File::options().write(true).open(b.tree.join("a/src/lib.rs")).unwrap();
    std::fs::write(b.tree.join("a/src/lib.rs"), LEASE_BYTES).unwrap();
    a.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)).unwrap();
    let _ = std::fs::remove_file(&b.log);
    let mut p4 = sleeper();
    let (d4, err) = b.lease("hand-4", &p4);
    assert_eq!(d4, d1);
    assert_eq!(
        cargo_log(&b),
        "clean -p crate-a",
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
    assert_eq!(cargo_log(&b), "clean -p crate-a");
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
    assert_eq!(cargo_log(&b), "clean --workspace");
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
    assert_eq!(cargo_log(&b), "clean -p crate-a");
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
    assert_eq!(cargo_log(&b), "clean -p crate-a");
}

#[test_id(tm4w9ze)]
#[test]
fn a_unit_with_no_timestamp_is_removed_and_a_deleted_file_cleans_its_crate() {
    let b = setup("bdcli-unit-nostamp");
    let (mut p, d) = edit_a_in_lease(&b);
    let bare = unit(&d, "crate-a-0123456789abcdef", None);
    release(&b, &mut p);
    assert!(!bare.exists(), "a unit with no timestamp is removed");
    assert_eq!(cargo_log(&b), "clean -p crate-a");

    let mut p2 = sleeper();
    let (d2, _) = b.lease("hand-2", &p2);
    let _ = std::fs::remove_file(&b.log);
    std::fs::remove_file(b.tree.join("b/src/lib.rs")).unwrap();
    let fresh = unit(&d2, "crate-b-0123456789abcdef", Some(std::time::SystemTime::now() + Duration::from_secs(3600)));
    release(&b, &mut p2);
    assert!(fresh.exists());
    assert_eq!(cargo_log(&b), "clean -p crate-b", "a deleted file cleans the crate whatever its units");
}

// These spawn the clean script because it is what is under test: the rule exists only in it.
fn clean_as(b: &Box_, dir: &Path, release: bool) -> String {
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    spira_config::build_dir::clean(&home.join(spira_config::build_dir::CLEAN_SCRIPT), &b.tree, dir, &b.bin, release, "sp-test", None).unwrap()
}

fn clean_tree(b: &Box_, dir: &Path) -> String {
    clean_as(b, dir, false)
}

fn backdate(file: &Path, secs: u64) {
    // clock-safe: sets an mtime in the past; nothing waits on the clock
    let to = std::time::SystemTime::now() - Duration::from_secs(secs);
    std::fs::File::options().append(true).create(true).open(file).unwrap().set_modified(to).unwrap();
}

#[test_id(tq4lk1a)]
#[test]
fn a_tree_whose_lockfile_alone_differs_cleans_nothing_at_lease() {
    let b = setup("bdcli-lock-only");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("Cargo.lock"), "# another tree's lock\n").unwrap();
    let said = clean_tree(&b, &d);
    assert!(said.contains("only Cargo.lock changed"), "{said}");
    assert!(!b.log.exists(), "cargo clean ran: {}", cargo_log(&b));
    let _ = std::fs::remove_file(&b.log);
    let again = clean_tree(&b, &d);
    assert!(!again.contains("clean"), "the new lock is the record now, so there is nothing to say: {again}");
    assert!(!b.log.exists());
}

#[test_id(tq4lk2b)]
#[test]
fn a_tree_whose_lockfile_and_one_crate_differ_cleans_that_crate_at_lease() {
    let b = setup("bdcli-lock-and-crate");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("Cargo.lock"), "# another tree's lock\n").unwrap();
    std::fs::write(b.tree.join("a/src/lib.rs"), "pub fn f() { let _ = 2; }\n").unwrap();
    clean_tree(&b, &d);
    assert_eq!(cargo_log(&b), "clean -p crate-a");
}

#[test_id(tq4lk3c)]
#[test]
fn a_tree_whose_workspace_manifest_differs_still_cleans_the_workspace_at_lease() {
    let b = setup("bdcli-root-manifest");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let _ = std::fs::remove_file(&b.log);
    std::fs::write(b.tree.join("Cargo.lock"), "# another tree's lock\n").unwrap();
    std::fs::write(b.tree.join("Cargo.toml"), "[workspace]\nmembers = [\"a\", \"b\"]\n# changed\n").unwrap();
    let said = clean_tree(&b, &d);
    assert!(said.contains("Cargo.toml changed"), "{said}");
    assert_eq!(cargo_log(&b), "clean --workspace");
}

/// `cargo test --offline --locked` in `tree`, building in `target`.
fn real_cargo_test(tree: &Path, target: &Path) -> std::process::Output {
    Command::new("cargo") // real cargo is the subject: only it decides whether a unit built under another lockfile is reused
        .args(["test", "--offline", "--locked", "-q"])
        .current_dir(tree)
        .env("CARGO_TARGET_DIR", target)
        .env("RUSTC_WRAPPER", "") // no compiler cache: the sandbox has no network, and the units must be this target's own
        .output()
        .unwrap()
}

// Real cargo, unlike the rest of this file: the claim under test is cargo's own behaviour, that a
// unit built against one resolved dependency version is not reused for a tree that resolves another.
// The clean's lockfile rule is safe only while that holds, and a fake cargo cannot show it.
#[test_id(tq4lk4d)]
#[test]
fn a_tree_whose_lockfile_alone_differs_does_not_run_the_other_trees_test_binary() {
    let d = testkit::TempDir::new("bdcli-lock-real");
    let (vendor, tree, target) = (d.path().join("vendor"), d.path().join("tree"), d.path().join("target"));
    let sum = "0".repeat(64);
    for (version, v) in [("0.1.0", 0), ("0.1.1", 1)] {
        let dep = vendor.join(format!("dep-{version}"));
        std::fs::create_dir_all(dep.join("src")).unwrap();
        std::fs::write(dep.join("Cargo.toml"), format!("[package]\nname = \"dep\"\nversion = \"{version}\"\nedition = \"2021\"\n")).unwrap();
        std::fs::write(dep.join("src/lib.rs"), format!("pub const V: u32 = {v};\n")).unwrap();
        std::fs::write(dep.join(".cargo-checksum.json"), format!("{{\"files\":{{}},\"package\":\"{sum}\"}}")).unwrap();
    }
    std::fs::create_dir_all(tree.join("w/src")).unwrap();
    std::fs::create_dir_all(tree.join(".cargo")).unwrap();
    std::fs::write(tree.join("Cargo.toml"), "[workspace]\nmembers = [\"w\"]\nresolver = \"2\"\n").unwrap();
    std::fs::write(tree.join("rust-toolchain.toml"), include_str!("../../rust-toolchain.toml")).unwrap();
    std::fs::write(tree.join(".cargo/config.toml"), format!("[source.crates-io]\nreplace-with = \"local\"\n[source.local]\ndirectory = {:?}\n", vendor.display().to_string())).unwrap();
    std::fs::write(tree.join("w/Cargo.toml"), "[package]\nname = \"w\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\ndep = \"0.1\"\n").unwrap();
    std::fs::write(tree.join("w/src/lib.rs"), "#[test]\nfn v() {\n    assert_eq!(dep::V, 1);\n}\n").unwrap();
    let lock = |version: &str| {
        format!("version = 3\n\n[[package]]\nname = \"dep\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{sum}\"\n\n[[package]]\nname = \"w\"\nversion = \"0.1.0\"\ndependencies = [\n \"dep\",\n]\n")
    };
    let home = spira_config::resolve::locate_home_for_process().unwrap();
    let script = home.join(spira_config::build_dir::CLEAN_SCRIPT);
    let path = std::env::var("PATH").unwrap_or_default();

    std::fs::write(tree.join("Cargo.lock"), lock("0.1.0")).unwrap();
    spira_config::build_dir::clean(&script, &tree, &target, &path, false, "sp-test", None).unwrap();
    let a = real_cargo_test(&tree, &target);
    let a_said = format!("{}{}", String::from_utf8_lossy(&a.stdout), String::from_utf8_lossy(&a.stderr));
    assert!(!a.status.success() && a_said.contains("left: 0"), "tree A's test runs against dep 0.1.0 and is red: {a_said}");

    std::fs::write(tree.join("Cargo.lock"), lock("0.1.1")).unwrap();
    let said = spira_config::build_dir::clean(&script, &tree, &target, &path, false, "sp-test", None).unwrap();
    assert!(said.contains("only Cargo.lock changed"), "the lease clean removes nothing: {said}");
    let b = real_cargo_test(&tree, &target);
    let b_said = format!("{}{}", String::from_utf8_lossy(&b.stdout), String::from_utf8_lossy(&b.stderr));
    assert!(b.status.success(), "tree B built its own test binary against dep 0.1.1; it did not run tree A's: {b_said}");
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

#[test_id(tiat16m)]
#[test]
fn a_release_advances_the_stamp_so_the_next_clean_keeps_the_units_the_lease_built() {
    let b = setup("bdcli-release-stamps");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    backdate(&d.join(".unit-tests-built-stamp"), 7200);
    let built = std::time::SystemTime::now() - Duration::from_secs(3600);
    let u = unit(&d, "crate-a-0123456789abcdef", Some(built));
    clean_as(&b, &d, true);
    let said = clean_tree(&b, &d);
    assert!(u.exists(), "{said}");
}

#[test_id(tip5b1u)]
#[test]
fn with_no_stamp_the_records_own_time_decides_which_units_are_rebuilt() {
    let b = setup("bdcli-no-stamp");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let _ = std::fs::remove_file(d.join(".unit-tests-built-stamp"));
    backdate(&d.join(".unit-tests-built-manifest"), 3600);
    let now = std::time::SystemTime::now();
    let older = unit(&d, "crate-a-0123456789abcdef", Some(now - Duration::from_secs(7200)));
    let newer = unit(&d, "crate-a-fedcba9876543210", Some(now - Duration::from_secs(1800)));
    let said = clean_tree(&b, &d);
    assert!(older.exists(), "a unit older than the record stays: {said}");
    assert!(!newer.exists(), "a unit newer than the record is dropped: {said}");
}

fn report(dir: &Path) -> spira_config::build_dir::CleanReport {
    spira_config::build_dir::clean_report(dir).expect("the clean wrote a report")
}

fn only_act(dir: &Path) -> spira_config::build_dir::CleanAct {
    let mut acts = report(dir).acts;
    assert_eq!(acts.len(), 1, "{acts:?}");
    acts.remove(0)
}

fn kind(a: &spira_config::build_dir::CleanAct) -> (&str, &str) {
    (&a.branch, &a.cause)
}

/// Whether `bytes` is `du -sb` of a unit holding `files` bytes: coreutils differ on counting the
/// directory's own size, `dir_len`.
fn is_du_of(bytes: Option<u64>, files: u64, dir_len: u64) -> bool {
    bytes == Some(files) || bytes == Some(files + dir_len)
}

#[test_id(tblk8bl)]
#[test]
fn a_directory_with_no_record_reports_a_whole_workspace_clean_with_cargos_count() {
    let b = setup("bdcli-report-new");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let r = report(&d);
    let stamped = std::fs::metadata(d.join(".unit-tests-built-stamp")).unwrap().modified().unwrap();
    assert!(r.rc == 0 && r.at >= stamped, "stamped by the script's own clock, at its exit: {r:?}");
    let a = only_act(&d);
    assert_eq!(kind(&a), ("whole-workspace", "missing-record"));
    assert_eq!((a.bytes, a.files), (Some(1_610_612_736), Some(3)));
}

#[test_id(ttlo7zm)]
#[test]
fn a_changed_lockfile_at_release_reports_the_build_input_that_changed() {
    let b = setup("bdcli-report-input");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    std::fs::write(b.tree.join("Cargo.lock"), "# lock changed\n").unwrap();
    clean_as(&b, &d, true);
    let a = only_act(&d);
    assert_eq!(kind(&a), ("whole-workspace", "build-input-changed"));
    assert_eq!(a.inputs, ["Cargo.lock"]);
}

#[test_id(txewbqf)]
#[test]
fn a_workspace_with_no_member_reports_an_unhashable_tree() {
    let b = setup("bdcli-report-unhashable");
    std::fs::write(b.tree.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    assert_eq!(kind(&only_act(&d)), ("whole-workspace", "unhashable-tree"));
}

#[test_id(tudphnf)]
#[test]
fn an_edited_crate_reports_a_per_crate_clean_naming_it() {
    let b = setup("bdcli-report-crate");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    std::fs::write(b.tree.join("b/src/lib.rs"), "pub fn f() { let _ = 1; }\n").unwrap();
    clean_tree(&b, &d);
    let a = only_act(&d);
    assert_eq!(kind(&a), ("per-crate", "bytes-changed"));
    assert_eq!(a.crates, ["crate-b"]);
}

#[test_id(tisa3uc)]
#[test]
fn a_unit_built_after_the_stamp_reports_the_bytes_and_files_it_removed() {
    let b = setup("bdcli-report-unit");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let later = std::time::SystemTime::now() + Duration::from_secs(3600);
    let u = unit(&d, "crate-a-0123456789abcdef", Some(later));
    std::fs::write(u.join("lib"), vec![0u8; 4096]).unwrap();
    let dir_len = std::fs::metadata(&u).unwrap().len();
    clean_tree(&b, &d);
    let a = only_act(&d);
    assert_eq!(kind(&a), ("units-built-after-stamp", ""));
    assert_eq!(a.crates, ["crate-a"]);
    assert!(is_du_of(a.bytes, 4096, dir_len), "{a:?}");
    assert_eq!(a.files, Some(2), "the file and the unit's stamp");
}

#[test_id(taeli5n)]
#[test]
fn a_release_that_drops_a_stale_unit_reports_it() {
    let b = setup("bdcli-report-release");
    let (mut p, d) = edit_a_in_lease(&b);
    let now = std::time::SystemTime::now();
    let stale = unit(&d, "crate-a-0123456789abcdef", Some(now - Duration::from_secs(3600)));
    unit(&d, "crate-a-fedcba9876543210", Some(now + Duration::from_secs(3600)));
    let dir_len = std::fs::metadata(&stale).unwrap().len();
    release(&b, &mut p);
    let a = only_act(&d);
    assert_eq!(kind(&a), ("release-units-dropped", ""));
    assert_eq!(a.crates, ["crate-a"]);
    assert!(is_du_of(a.bytes, 0, dir_len), "{a:?}");
    assert_eq!(a.files, Some(1));
}

#[test_id(trel889)]
#[test]
fn a_clean_with_nothing_changed_reports_no_act_over_the_last_report() {
    let b = setup("bdcli-report-none");
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    assert_eq!(report(&d).acts.len(), 1, "positive control: the first clean acted");
    clean_tree(&b, &d);
    assert_eq!(report(&d).acts, []);
}

#[test_id(tb81g9z)]
#[test]
fn a_cargo_that_prints_no_summary_reports_unknown_bytes() {
    let b = setup("bdcli-report-silent");
    testkit::write_exe(Path::new(b.bin.split(':').next().unwrap()).join("cargo"), &stub_cargo(&b.log, None));
    let d = b.base.join("dir.1");
    clean_tree(&b, &d);
    let a = only_act(&d);
    assert_eq!((a.bytes, a.files), (None, None));
}

#[test_id(t8fk41q)]
#[test]
fn cargos_removal_summary_is_read_in_every_unit_it_prints() {
    let b = setup("bdcli-report-summary");
    let cargo = Path::new(b.bin.split(':').next().unwrap()).join("cargo");
    for (i, (line, files, bytes)) in [
        ("Removed 0 files", 0, 0),
        ("Removed 2 files, 359B total", 2, 359),
        ("Removed 8 files, 8.7MiB total", 8, 9_122_611),
        ("Removed 1 files, 11.0GiB total", 1, 11_811_160_064),
    ]
    .into_iter()
    .enumerate()
    {
        testkit::write_exe(&cargo, &stub_cargo(&b.log, Some(line)));
        let d = b.base.join(format!("dir.{}", i + 1));
        let said = clean_tree(&b, &d);
        assert!(said.contains(line), "cargo's output still reaches the caller: {said}");
        let a = only_act(&d);
        assert_eq!((a.files, a.bytes), (Some(files), Some(bytes)), "{line}");
    }
}

#[test_id(tu44o0v)]
#[test]
fn a_tree_that_is_no_cargo_workspace_reports_no_act() {
    let b = setup("bdcli-report-no-cargo");
    std::fs::remove_file(b.tree.join("Cargo.toml")).unwrap();
    let d = b.base.join("dir.1");
    std::fs::create_dir_all(&d).unwrap();
    clean_tree(&b, &d);
    assert_eq!(report(&d).acts, []);
}

// The sink answers one connection, and the clean's push is the first a lease makes.
#[test_id(tr12pkq)]
#[test]
fn a_lease_pushes_what_its_clean_removed_under_its_holder_and_mode() {
    let b = setup("bdcli-report-push");
    let (addr, sink) = spira_otel::metrics::run::fixture_sink_within(Duration::from_secs(20));
    let toml = spira_config::process::fixture_toml(
        b._d.path(),
        &[
            ("SPIRA_DWARF_BUILD_BASE", b.base.to_str().unwrap()),
            ("SPIRA_DWARF_BUILD_BUDGET_GIB", "60"),
            ("SPIRA_DWARF_BUILD_FOLDERS_PER_REPO", "5"),
            ("SPIRA_METRICS_ENDPOINT", &addr),
        ],
    );
    let b = Box_ { toml, ..b };
    let mut p = sleeper();
    b.lease("hand-pushes", &p);
    let body = sink.join().unwrap().expect("the lease pushed");
    for want in ["build_dir_clean_bytes", "hand-pushes", "lease", "whole-workspace", "missing-record"] {
        assert!(body.contains(want), "{want} missing from the push: {body}");
    }
    let _ = (p.kill(), p.wait());
}
