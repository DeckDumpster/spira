//! The exit commit of an archive run, against a real git checkout with a real remote.

#[macro_use]
extern crate spira_test_id;
use archivist::config::Env;
use archivist::run::{commit_after, Outcome};
use std::path::Path;
use testkit::{git, TempDir};

fn env(wiki: &Path) -> Env {
    Env {
        db: ".".into(),
        run: "tmp".into(),
        runes: String::new(),
        token_projects: String::new(),
        wiki: Some(wiki.display().to_string()),
        agent: "claude".into(),
        tz: "UTC".into(),
        every: 40,
        idle: 1800,
        ctx_warn: 200_000,
        ctx_high: 400_000,
        ctx_limit: 1_000_000,
        model: "m".into(),
        timeout: 900,
        per_pass: 1,
        timeout_retries: 3,
        fail_retries: 3,
        fail_backoff: 600,
    }
}

fn brain(tag: &str) -> (TempDir, TempDir) {
    let remote = TempDir::new(&format!("{tag}-remote"));
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    let work = TempDir::new(&format!("{tag}-brain"));
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.name", "t"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["remote", "add", "origin", &remote.display().to_string()]);
    std::fs::create_dir_all(work.join("wiki/notes")).unwrap();
    std::fs::write(work.join("log.md"), "# log\n").unwrap();
    std::fs::write(work.join("wiki/notes/seed.md"), "seed\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-q", "-m", "seed"]);
    git(&work, &["push", "-q", "-u", "origin", "main"]);
    (remote, work)
}

#[test_id(twrocd6)]
#[test]
fn a_failed_exit_commit_fails_the_run_naming_the_dirty_path() {
    let (_remote, work) = brain("exit-fail");
    let cfg = env(&work);
    let before = archivist::brain::snapshot(&work);
    std::fs::write(work.join("wiki/notes/new-page.md"), "page\n").unwrap();
    let hook = work.join(".git/hooks/pre-commit");
    testkit::write_exe(&hook, "#!/bin/sh\nexit 1\n");

    let committed = commit_after(&cfg, "sess", Some(before)).unwrap();
    let err = committed.clone().unwrap_err();
    assert!(err.contains("paths not landed: wiki/notes/new-page.md"), "error must list the dirty path: {err}");
    assert_eq!(Outcome::of(0, &Some(committed)), Outcome::CommitFailed);
}

#[test_id(thy7wab)]
#[test]
fn a_log_entry_is_committed_with_the_page_an_earlier_run_left_dirty() {
    let (_remote, work) = brain("exit-announced");
    let cfg = env(&work);
    std::fs::write(work.join("wiki/notes/orphan.md"), "an earlier run's page\n").unwrap();
    let before = archivist::brain::snapshot(&work);

    std::fs::write(work.join("log.md"), "# log\n- Updated: [[orphan]]\n").unwrap();
    commit_after(&cfg, "sess", Some(before)).unwrap().unwrap();

    assert_eq!(git(&work, &["status", "--porcelain"]), "");
    assert!(git(&work, &["ls-files"]).contains("wiki/notes/orphan.md"));
}
