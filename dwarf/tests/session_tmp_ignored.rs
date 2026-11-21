#[macro_use]
extern crate spira_test_id;
use std::process::Command;

#[test_id(tz24dkq)]
#[test]
fn git_add_all_in_a_worktree_with_a_session_tmpdir_stages_nothing_under_runtime() {
    let d = testkit::TempDir::new("ignored");
    let git = |args: &[&str]| Command::new("git").arg("-C").arg(d.path()).args(args).output().unwrap();
    assert!(git(&["init", "-q"]).status.success());
    let tmp = dwarf::run::Run::make_session_tmpdir(d.path()).unwrap();
    std::fs::write(tmp.join("scratch"), "s").unwrap();
    assert!(git(&["add", "-A"]).status.success());
    let staged = git(&["diff", "--cached", "--name-only"]);
    let staged = String::from_utf8_lossy(&staged.stdout);
    assert!(!staged.contains(".runtime"), "{staged}");
}
