//! `target-reap`'s entry, `testenv::target_reap::run`, over worktrees planted on disk with their
//! modification times set: the bounded reap takes the oldest tenth by count and nothing else.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use testenv::target_reap::{parse, run};

fn age(path: &Path, secs: u64) {
    fs::File::open(path).unwrap().set_modified(SystemTime::now() - Duration::from_secs(secs)).unwrap();
}

/// `n` idle worktrees `sp-w00`.. (named as beads are); `w{i}` last touched `(i + 2)` hours ago.
fn plant(root: &Path, n: usize) {
    for i in 0..n {
        let d = root.join(format!("sp-w{i:02}"));
        fs::create_dir_all(d.join("target/debug")).unwrap();
        fs::write(d.join("target/debug/x"), b"built").unwrap();
        fs::write(d.join("src.rs"), b"kept").unwrap();
        age(&d.join("src.rs"), (i as u64 + 2) * 3600);
        age(&d, (i as u64 + 2) * 3600);
    }
}

fn reaped(root: &Path, n: usize) -> Vec<usize> {
    (0..n).filter(|i| !root.join(format!("sp-w{i:02}/target")).exists()).collect()
}

fn call(args: &[&str]) -> (u8, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&args, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap() + &String::from_utf8(err).unwrap())
}

#[test]
fn a_tenth_of_twenty_idle_targets_removes_exactly_the_two_oldest() {
    let _env = testkit::env(&[("SPIRA_REAP_IDLE_SECS", Some("3600"))]);
    let dir = testkit::TempDir::new("reap-fraction");
    plant(&dir.path(), 20);
    let (code, said) = call(&["--fraction", "0.1", "--worktrees", dir.path().to_str().unwrap()]);
    assert_eq!(code, 0, "{said}");
    assert_eq!(reaped(&dir.path(), 20), vec![18, 19], "the two oldest, w19 and w18, and no others");
    assert!(dir.path().join("sp-w19/src.rs").is_file(), "source stays");
}

#[test]
fn a_dry_run_names_the_bound_and_removes_nothing() {
    let _env = testkit::env(&[("SPIRA_REAP_IDLE_SECS", Some("3600"))]);
    let dir = testkit::TempDir::new("reap-fraction-dry");
    plant(&dir.path(), 20);
    let (code, said) = call(&["--dry-run", "--fraction", "0.1", "--worktrees", dir.path().to_str().unwrap()]);
    assert_eq!(code, 0, "{said}");
    assert!(reaped(&dir.path(), 20).is_empty());
    assert!(said.contains("sp-w19") && said.contains("sp-w18") && !said.contains("sp-w17"), "{said}");
}

#[test]
fn a_bound_over_a_few_still_takes_the_oldest_one() {
    let _env = testkit::env(&[("SPIRA_REAP_IDLE_SECS", Some("3600"))]);
    let dir = testkit::TempDir::new("reap-fraction-few");
    plant(&dir.path(), 3);
    assert_eq!(call(&["--fraction", "0.1", "--worktrees", dir.path().to_str().unwrap()]).0, 0);
    assert_eq!(reaped(&dir.path(), 3), vec![2]);
}

#[test]
fn the_fraction_argument_is_refused_outside_zero_to_one_and_scratch_needs_it() {
    for bad in [&["--fraction", "0"][..], &["--fraction", "1.5"], &["--fraction"], &["--fraction", "x"], &["--scratch"]] {
        assert!(parse(&bad.iter().map(|s| s.to_string()).collect::<Vec<_>>()).is_err(), "{bad:?}");
    }
    assert_eq!(call(&["--fraction", "0"]).0, 2);
}

#[test]
fn bounded_scratch_reap_takes_the_oldest_tenth_of_stale_gate_trees() {
    let dir = testkit::TempDir::new("reap-fraction-scratch");
    let (root, trees) = (dir.path().join("scratch"), dir.path().join("trees"));
    fs::create_dir_all(&trees).unwrap();
    for i in 0..20 {
        let d = root.join(format!("tree{i:02}"));
        fs::create_dir_all(&d).unwrap();
        age(&d, (i as u64 + 2) * 3600);
    }
    let root_s = root.to_str().unwrap().to_string();
    let _env = testkit::env(&[("SPIRA_GATE_TARGET_ROOT", Some(&root_s)), ("SPIRA_GATE_TARGET_MAX_AGE_MIN", Some("60"))]);
    let (code, said) = call(&["--fraction", "0.1", "--scratch", "--worktrees", trees.to_str().unwrap()]);
    assert_eq!(code, 0, "{said}");
    let gone: Vec<usize> = (0..20).filter(|i| !root.join(format!("tree{i:02}")).exists()).collect();
    assert_eq!(gone, vec![18, 19], "{said}");
}

#[test]
fn a_box_with_no_gate_scratch_reaps_none_and_says_so() {
    let _env = testkit::env(&[("SPIRA_GATE_TARGET_ROOT", None), ("SPIRA_TOML", Some("/no/such/spira-toml-for-this-test"))]);
    let dir = testkit::TempDir::new("reap-fraction-none");
    let mut out = Vec::new();
    assert!(!testenv::target_reap::reap_scratch_report(dir.path(), Some(0.1), false, false, &|_| false, &mut out));
    assert!(out.is_empty());
    let (code, said) = call(&["--fraction", "0.1", "--scratch", "--worktrees", dir.path().to_str().unwrap()]);
    assert_eq!(code, 1, "no SPIRA_RUN and no named root: {said}");
    assert!(said.contains("no gate scratch to find"), "{said}");
}

#[test]
fn every_flag_parses_and_an_unknown_one_is_usage() {
    let args = |a: &[&str]| parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    let a = args(&["--dry-run", "--if-below-floor", "--worktrees", "/w", "--fraction", "0.5", "--scratch"]).unwrap();
    assert_eq!((a.dry, a.if_below_floor, a.scratch, a.fraction), (true, true, true, Some(0.5)));
    assert_eq!(a.dir.as_deref(), Some(Path::new("/w")));
    assert!(args(&["--bogus"]).unwrap_err().starts_with("usage:"));
    assert!(args(&["--worktrees"]).is_err());
    assert_eq!(args(&[]).unwrap(), testenv::target_reap::Args::default());
}
