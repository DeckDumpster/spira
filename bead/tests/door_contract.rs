//! The content door's contract run against a release `repo-contract` that overruns its bound.

#[macro_use]
extern crate spira_test_id;
use bead::filing::{contract_guard, Contract};
use std::time::Duration;

fn gone(pid: &str) -> bool {
    let stat = std::path::Path::new(system_paths::PROC).join(pid).join("stat");
    std::fs::read_to_string(stat).map_or(true, |s| s.rsplit_once(')').is_some_and(|(_, r)| matches!(r.split_whitespace().next(), Some("Z" | "X"))))
}

#[test_id(tnbobrp)]
#[test]
fn a_door_check_that_overruns_its_bound_is_killed_with_its_children_and_cannot_tell() {
    let release = testkit::TempDir::new("bead-door-contract");
    let bin = release.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (argv, grandchild) = (release.path().join("argv"), release.path().join("grandchild"));
    testkit::write_exe(
        bin.join("repo-contract"),
        &format!("#!/bin/sh\necho \"$@\" > {}\nsetsid sleep 600 >/dev/null 2>&1 &\necho $! > {}\nsleep 600\n", argv.display(), grandchild.display()),
    );
    let _env = testkit::env(&[("SPIRA_RELEASE", release.path().to_str())]);

    let refusal = contract_guard("deck", &[], &Contract::door("home", Duration::from_secs(2)), &|_| None).expect_err("filing admitted");

    assert!(refusal.text.contains("cannot tell whether deck meets the landing contract: repo-contract check gave exit 124"), "{}", refusal.text);
    assert_eq!(std::fs::read_to_string(argv).unwrap().trim(), "check --cached-shell deck");
    let pid = std::fs::read_to_string(grandchild).unwrap().trim().to_string();
    let mut polls = 0u64;
    while !gone(&pid) {
        polls += 1;
        assert!(polls < 200_000, "grandchild {pid} outlived the bound");
        std::thread::yield_now();
    }
}
