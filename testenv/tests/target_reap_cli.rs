//! Black-box regression for two production sp-x9kbg gaps, run against the REAL compiled
//! `target-reap` binary, never a mock:
//!
//! 1. `landing-pass` spawns `target-reap` by bare name (`real.rs` `ensure()`) WITHOUT
//!    setting `SPIRA_HOME` — only `rebase_stale` does that. So the binary must resolve its
//!    own harness home in-process (the same three rungs `queue`'s and `landing-pass`'s own
//!    `harness_home` climb), never require the caller to have set it.
//! 2. "Tip is an ancestor of the landing ref" is trivially true for a worktree JUST cut
//!    from base — a fresh aeon claim with zero commits of its own. Landed is the lifecycle
//!    record's LANDED (`spira-lc state`, sp-2c1n0) — a fresh claim is WORKING there — or a
//!    freshly claimed worktree gets its target/ deleted mid-compile.
//!
//! Both are exercised together: one fixture, one invocation, under an `env -i`-equivalent
//! environment (HOME + PATH only — PATH carries this binary's own directory and
//! `/usr/bin:/bin`; SPIRA_LC_BIN pins a stub `spira-lc` — the way `landing-pass` launches it after its sweep).

use std::fs;
use std::path::Path;
use std::process::Command;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

fn commit(dir: &Path, file: &str, subject: &str) {
    fs::write(dir.join(file), subject).unwrap();
    run_git(dir, &["add", file]);
    run_git(dir, &["commit", "-q", "-m", subject]);
}

#[test]
fn sp_x9kbg_target_reap_under_a_bare_env_reaps_only_the_truly_landed_worktree() {
    let t = testkit::TempDir::new("target-reap-cli");
    let repo = t.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, "f", "base");

    // sp-land1: a real landing. Its own commit names the bead (law-aeon-commits-name-
    // their-bead), and main is fast-forwarded onto it — exactly what a push-mode landing
    // does, no merge commit required.
    run_git(&repo, &["checkout", "-q", "-b", "feature-landed"]);
    commit(&repo, "g", "sp-land1: finished the work");
    run_git(&repo, &["checkout", "-q", "main"]);
    run_git(&repo, &["merge", "-q", "--ff-only", "feature-landed"]);
    run_git(&repo, &["checkout", "-q", "main"]); // leave HEAD sane for later worktree adds

    let worktrees = t.path().join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();

    // The landed worktree: Concierge-named, directly on feature-landed (same commit now on
    // main, citing sp-land1).
    let landed_wt = worktrees.join("concierge-sp-land1");
    run_git(&repo, &["worktree", "add", "-q", landed_wt.to_str().unwrap(), "feature-landed"]);

    // sp-fresh: a brand-new aeon claim, cut from main a moment ago. Zero commits of its
    // own — tip equals main's tip, so "is an ancestor" is trivially true, but main holds
    // no commit naming sp-fresh at all.
    let fresh_wt = worktrees.join("sp-fresh");
    run_git(&repo, &["worktree", "add", "-q", "-b", "feature-fresh", fresh_wt.to_str().unwrap(), "main"]);

    for (wt, bytes) in [(&landed_wt, 4096usize), (&fresh_wt, 2048usize)] {
        let d = wt.join("target/aeon");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("x"), vec![1u8; bytes]).unwrap();
    }

    // The lifecycle record: sp-land1 LANDED, sp-fresh WORKING (a claim, never landed).
    let lc_dir = t.path().join("lc-bin");
    fs::create_dir_all(&lc_dir).unwrap();
    testkit::write_exe(
        lc_dir.join("spira-lc"),
        "#!/bin/sh\n[ \"$1\" = state ] || exit 2\ncase \"$2\" in sp-land1) echo LANDED ;; sp-fresh) echo WORKING ;; *) exit 1 ;; esac\n",
    );

    let bin = env!("CARGO_BIN_EXE_target-reap");
    let bin_dir = Path::new(bin).parent().unwrap();
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());

    let out = Command::new(bin)
        .arg("--worktrees")
        .arg(&worktrees)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &path)
        // Pinned, so the run can never reach a real lifecycle record through a release
        // that config discovery finds on this box.
        .env("SPIRA_LC_BIN", lc_dir.join("spira-lc"))
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    assert!(out.status.success(), "target-reap under a bare env (no SPIRA_HOME) must still resolve its own harness home: {text}");
    assert!(!landed_wt.join("target").exists(), "the truly landed worktree's target/ must go even with no SPIRA_HOME set: {text}");
    assert!(fresh_wt.join("target/aeon/x").is_file(), "a freshly cut, zero-commit worktree must NEVER be reaped: {text}");
}
