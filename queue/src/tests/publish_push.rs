//! `[publish.<name>] mode = "push"` against a real bare remote: the green head is fast-forwarded
//! onto the remote branch, no forge API is touched, and a diverged remote is refused.

use super::*;
use crate::real::RealGit;
use spira_config::PublishMode;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Fixture {
    t: T,
    repo: PathBuf,
    remote: PathBuf,
}

fn fixture(mode: PublishMode) -> Fixture {
    let t = T::new(LandMode::QueueLocal);
    let base = t.dir.join("git");
    let (remote, repo) = (base.join("remote.git"), base.join("repo"));
    fs::create_dir_all(&base).unwrap();
    git(&base, &["init", "-q", "--bare", "-b", "main", remote.to_str().unwrap()]);
    git(&base, &["init", "-q", "-b", "main", repo.to_str().unwrap()]);
    git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    git(&repo, &["checkout", "-q", "-b", "local/main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "spira: land sp-a"]);
    {
        let mut r = t.lib.r.borrow_mut();
        r.path = Some(repo.clone());
        r.publish_mode = mode;
    }
    t.lib.real_git.set(true);
    let head = git(&repo, &["rev-parse", "local/main"]);
    full_suite_passed(&t, &head);
    Fixture { t, repo, remote }
}

#[test]
fn push_mode_fast_forwards_the_remote_branch_and_calls_no_forge() {
    let f = fixture(PublishMode::Push);
    f.t.forge.publish_prs.borrow_mut().push(("9".into(), "spira/publish/old".into()));
    let head = git(&f.repo, &["rev-parse", "local/main"]);
    assert_eq!(f.t.run_on(&RealGit, &["publish"]), 0, "{}", f.t.err());
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), head);
    assert!(f.t.forge.calls.borrow().is_empty(), "{:?}", f.t.forge.calls.borrow());
    assert!(!f.t.qfile("publish").exists());

    let before = git(&f.repo, &["rev-parse", "local/main"]);
    git(&f.repo, &["commit", "-q", "--allow-empty", "-m", "spira: land sp-b"]);
    let head = git(&f.repo, &["rev-parse", "local/main"]);
    assert_ne!(before, head);
    full_suite_passed(&f.t, &head);
    assert_eq!(f.t.run_on(&RealGit, &["publish-settle"]), 0, "{}", f.t.err());
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), head);
    assert!(f.t.forge.calls.borrow().is_empty(), "{:?}", f.t.forge.calls.borrow());
    assert!(!f.t.qfile("publish").exists());
}

#[test]
fn push_mode_refuses_a_diverged_remote_naming_both_tips() {
    let f = fixture(PublishMode::Push);
    let other = f.t.dir.join("git/other");
    git(&f.t.dir.join("git"), &["clone", "-q", f.remote.to_str().unwrap(), other.to_str().unwrap()]);
    git(&other, &["commit", "-q", "--allow-empty", "-m", "someone else"]);
    git(&other, &["push", "-q", "origin", "main"]);
    let remote_tip = git(&f.remote, &["rev-parse", "main"]);
    let head = git(&f.repo, &["rev-parse", "local/main"]);

    assert_ne!(f.t.run_on(&RealGit, &["publish"]), 0);
    let err = f.t.err();
    assert!(err.contains(&remote_tip) && err.contains(&head), "{err}");
    assert!(err.contains("non-fast-forward"), "{err}");
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), remote_tip);
    assert!(f.t.forge.calls.borrow().is_empty());
}

#[test]
fn pr_mode_still_opens_a_forge_pr_for_the_same_fixture() {
    let f = fixture(PublishMode::Pr);
    assert_eq!(f.t.run_on(&RealGit, &["publish"]), 0, "{}", f.t.err());
    assert!(
        f.t.forge.calls.borrow().iter().any(|c| c.starts_with("pr-create ")),
        "positive control: the default mode opens a PR"
    );
    assert_ne!(git(&f.remote, &["rev-parse", "main"]), git(&f.repo, &["rev-parse", "local/main"]));
}

#[test]
fn push_mode_keeps_the_land_commit_guard() {
    let f = fixture(PublishMode::Push);
    git(&f.repo, &["commit", "-q", "--amend", "--allow-empty", "-m", "an unmarked commit"]);
    let head = git(&f.repo, &["rev-parse", "local/main"]);
    full_suite_passed(&f.t, &head);
    let before = git(&f.remote, &["rev-parse", "main"]);
    assert_ne!(f.t.run_on(&RealGit, &["publish"]), 0);
    assert!(f.t.err().contains("no land commit"), "{}", f.t.err());
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), before);
}

#[test]
fn push_mode_refuses_while_a_publish_pr_record_is_still_open() {
    let f = fixture(PublishMode::Push);
    fs::create_dir_all(f.t.qfile("publish").parent().unwrap()).unwrap();
    fs::write(f.t.qfile("publish"), "pr=5\nhead=h\n").unwrap();
    let before = git(&f.remote, &["rev-parse", "main"]);
    assert_ne!(f.t.run_on(&RealGit, &["publish"]), 0);
    assert!(f.t.err().contains("already open"), "{}", f.t.err());
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), before);
}

#[test]
fn a_divergence_alert_reaches_the_tests_own_mailbox_never_the_hosts() {
    let f = fixture(PublishMode::Push);
    let host = f.t.dir.join("host");
    let host_log = host.join("spool");
    fs::create_dir_all(&host).unwrap();
    testkit::write_exe(
        host.join("mail"),
        &format!("#!/bin/sh\ncat >> {}\n", host_log.display()),
    );
    *f.t.lib.host_path.borrow_mut() = Some(format!("{}:/usr/bin:/bin", host.display()));
    let other = f.t.dir.join("git/other");
    git(&f.t.dir.join("git"), &["clone", "-q", f.remote.to_str().unwrap(), other.to_str().unwrap()]);
    git(&other, &["commit", "-q", "--allow-empty", "-m", "someone else"]);
    git(&other, &["push", "-q", "origin", "main"]);

    assert_ne!(f.t.run_on(&RealGit, &["publish"]), 0);

    assert!(!host_log.exists(), "the host's mailbox received a test's alert");
    let own = fs::read_to_string(f.t.dir.join("run/mail-log")).unwrap_or_default();
    assert!(own.contains("divergence") && own.contains("someone else"), "{own}");
}

#[test]
fn notify_refuses_when_the_mail_on_path_is_not_test_owned() {
    let _serial = crate::testutil::serial();
    let bin = crate::testutil::tmpdir("host-mail");
    testkit::write_exe(bin.join("mail"), "#!/bin/sh\ncat >/dev/null\n");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let _env = testkit::env(&[("PATH", Some(&path))]);
    let r = std::panic::catch_unwind(|| crate::ops::helpers::notify("concierge", "x", "s", "b"));
    assert!(r.is_err(), "a host mail wrapper was accepted as test-owned");
}
