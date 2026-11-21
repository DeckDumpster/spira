#[macro_use]
extern crate spira_test_id;
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn fixture() -> testkit::TempDir {
    let d = testkit::TempDir::new("scratch");
    git(d.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(d.path().join(".runtime/session-tmp-8")).unwrap();
    std::fs::write(d.path().join(".runtime/session-tmp-8/old"), "o").unwrap();
    git(d.path(), &["add", "-f", "-A"]);
    git(d.path(), &["commit", "-q", "-m", "base"]);
    git(d.path(), &["checkout", "-q", "-b", "work"]);
    d
}

#[test_id(tyn0cso)]
#[test]
fn a_branch_that_force_adds_session_scratch_is_refused_naming_the_path() {
    let d = fixture();
    std::fs::create_dir_all(d.path().join(".runtime/session-tmp-9")).unwrap();
    std::fs::write(d.path().join(".runtime/session-tmp-9/x"), "x").unwrap();
    git(d.path(), &["add", "-f", ".runtime/session-tmp-9/x"]);
    git(d.path(), &["commit", "-q", "-m", "scratch"]);
    let e = work::scratch::gate(d.path(), "main").unwrap_err();
    assert!(e.contains(".runtime/session-tmp-9/x"), "{e}");
}

#[test_id(tfbeslp)]
#[test]
fn a_branch_that_deletes_session_scratch_is_not_refused() {
    let d = fixture();
    git(d.path(), &["rm", "-q", "-r", ".runtime/session-tmp-8"]);
    git(d.path(), &["commit", "-q", "-m", "rm"]);
    work::scratch::gate(d.path(), "main").unwrap();
}

#[test_id(t7u5yzm)]
#[test]
fn an_unresolvable_landing_ref_refuses() {
    let d = fixture();
    assert!(work::scratch::gate(d.path(), "no-such-ref").is_err());
}
