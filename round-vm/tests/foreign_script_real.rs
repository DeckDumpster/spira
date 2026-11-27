//! `FOREIGN_SCRIPT` in a real shell against a real git mirror; only `podman` is faked.
#[macro_use]
extern crate spira_test_id;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use testkit::git;

struct Pass {
    _tmp: testkit::TempDir,
    work: PathBuf,
    code: Option<i32>,
    said: String,
}

impl Pass {
    fn result(&self, file: &str) -> Option<String> {
        std::fs::read_to_string(self.work.join(".runtime/spira/batch-results").join(file)).ok()
    }
}

/// One round whose `podman run` does `entry`, under a cap of `cap` seconds.
fn pass(name: &str, entry: &str, cap: u64) -> Pass {
    let tmp = testkit::TempDir::new(name);
    let root = tmp.path().to_path_buf();
    let (src, home, stub, mirror) = (root.join("src"), root.join("home"), root.join("stub"), root.join("mirror.git"));
    for d in [&src, &home, &stub] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&src, &["init", "--quiet"]);
    std::fs::write(src.join("tracked"), "the round's tree\n").unwrap();
    git(&src, &["add", "tracked"]);
    git(&src, &["commit", "--quiet", "-m", "c"]);
    let sha = git(&src, &["rev-parse", "HEAD"]);
    git(&src, &["update-ref", "refs/rounds/1", &sha]);
    git(&root, &["clone", "--quiet", "--mirror", "src", "mirror.git"]);
    testkit::write_exe(stub.join("podman"), &format!("#!/bin/sh\n[ \"$1\" = run ] || exit 0\necho entry-output\n{entry}\n"));

    // The script pins its own PATH and fetches over git://; both are pointed at the fixture.
    let url = "\"git://${host_addr}:${port}/mirror.git\"";
    assert!(round_vm::run::FOREIGN_SCRIPT.contains(url));
    let path_line = round_vm::run::FOREIGN_SCRIPT.lines().find(|l| l.starts_with("export PATH=")).unwrap();
    let script = round_vm::run::FOREIGN_SCRIPT
        .replace(url, &format!("\"{}\"", mirror.display()))
        .replace(path_line, &format!("export PATH=\"{}:{}\"", stub.display(), system_paths::SYSTEM_PATH_MIN));
    std::fs::write(root.join("round.sh"), script).unwrap();
    let cap = cap.to_string();
    let args = ["host", "1", "", "1", "", "", "", "1", &cap, "1", "", "refs/rounds/1", &sha, "the-entry", "", "", "", "", "", "", "image", ""];
    let o = Command::new("bash").arg(root.join("round.sh")).args(args).env_clear().env("HOME", &home).env("PATH", system_paths::SYSTEM_PATH_MIN).stdin(Stdio::null()).output().unwrap();
    Pass { work: home.join("round-work"), code: o.status.code(), said: String::from_utf8_lossy(&o.stderr).into_owned(), _tmp: tmp }
}

fn stamped(p: &Pass) -> Vec<String> {
    p.result("round-phases").unwrap_or_default().lines().filter_map(|l| l.split(' ').next()).map(str::to_string).collect()
}

#[test_id(tke7w2w)]
#[test]
fn an_entry_past_the_cap_is_cut_and_recorded_with_its_phase_and_no_verdict() {
    let p = pass("foreign-cut", "exec sleep 30", 1);
    assert_eq!(p.code, Some(124), "{}", p.said);
    assert_eq!(std::fs::read_to_string(p.work.join("tracked")).unwrap(), "the round's tree\n", "the round's commit is checked out");
    assert_eq!(stamped(&p), ["clone", "pull", "entry"], "{}", p.said);
    assert_eq!(p.result("round-phase").as_deref(), Some("phase=entry rc=124\n"));
    let kind = p.result("round-fault-kind").expect("the cut is recorded");
    assert!(kind.starts_with("over-cap guard=ci-entry-cap phase=entry elapsed=") && kind.contains("s cap=1s"), "{kind}");
    assert_eq!(p.result("ci.result"), None, "a cut entry has no verdict");
}

#[test_id(td99zl4)]
#[test]
fn an_entry_that_passes_is_green_and_one_that_exits_124_early_is_an_ordinary_red() {
    let green = pass("foreign-green", "exit 0", 600);
    assert_eq!(green.code, Some(0), "{}", green.said);
    assert!(green.result("ci.result").unwrap_or_default().starts_with("ok "), "{}", green.said);
    assert_eq!(green.result("round-fault-kind"), None);
    let early = pass("foreign-early", "exit 124", 600);
    assert_eq!(early.code, Some(1), "{}", early.said);
    assert!(early.result("ci.result").unwrap_or_default().starts_with("red "), "{}", early.said);
    assert_eq!(early.result("round-fault-kind"), None, "the entry's own status is not the cap's cut");
}
