//! Black-box regression for two production sp-x9kbg gaps, run against the REAL compiled
//! `target-reap` binary, never a mock:
//!
//! 1. `target-reap` needs its own harness home (`$SPIRA_HOME`) to find `spira-lc` beside
//!    the release. Per Ryan 2026-10-05 (one source of config, no second source improvised
//!    from the filesystem layout), it no longer searches beside its own executable for a
//!    `spira/` when `SPIRA_HOME` is unset — the caller (`landing-pass`, or the unit this
//!    runs under) now sets `SPIRA_HOME` (and `SPIRA_RELEASE`) explicitly, and an unset one
//!    is a refusal.
//! 2. "Tip is an ancestor of the landing ref" is trivially true for a worktree JUST cut
//!    from base — a fresh aeon claim with zero commits of its own. Landed is the lifecycle
//!    record's LANDED (`spira-lc state`, sp-2c1n0) — a fresh claim is WORKING there — or a
//!    freshly claimed worktree gets its target/ deleted mid-compile.
//!
//! The functional case exercises both together: one fixture, one invocation, under an
//! `env -i`-equivalent environment (HOME + PATH + SPIRA_HOME + SPIRA_TOML — PATH carries
//! this binary's own directory and `/usr/bin:/bin`; SPIRA_LC_BIN pins a stub `spira-lc` —
//! the way `landing-pass` launches it after its sweep). A second, smaller case covers the
//! refusal when `SPIRA_HOME` is missing altogether.

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

/// `$SPIRA_HOME`: the tree's own `spira/` (where `conf.d` and `lib.sh` live), exactly as
/// the triage guide prescribes for a test that reaches a binary's top-level config loader.
fn spira_home() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../spira"))
}

#[test]
fn sp_x9kbg_target_reap_with_spira_home_set_reaps_only_the_truly_landed_worktree() {
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

    // SPIRA_RUN only feeds the optional, secondary gate-target-dir sweep here (the
    // `--worktrees` flag already supplies the worktree root this test actually cares
    // about) — any harmless, non-colliding path declares it so the top-level `cfg` read
    // exercises the real one-source-of-config path rather than silently defaulting.
    let toml = spira_config::process::fixture_toml(t.path(), &[("SPIRA_RUN", t.path().join("run").to_str().unwrap())]);

    let out = Command::new(bin)
        .arg("--worktrees")
        .arg(&worktrees)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &path)
        .env("SPIRA_HOME", spira_home())
        .env("SPIRA_TOML", &toml)
        // Pinned, so the run can never reach a real lifecycle record through a release
        // that config discovery finds on this box.
        .env("SPIRA_LC_BIN", lc_dir.join("spira-lc"))
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    assert!(out.status.success(), "target-reap with SPIRA_HOME/SPIRA_TOML set must resolve its own harness home: {text}");
    assert!(!landed_wt.join("target").exists(), "the truly landed worktree's target/ must go: {text}");
    assert!(fresh_wt.join("target/aeon/x").is_file(), "a freshly cut, zero-commit worktree must NEVER be reaped: {text}");
}

/// Per Ryan 2026-10-05 (one source of config): `target-reap` no longer searches beside its
/// own executable for a `spira/` when `SPIRA_HOME` is unset — it refuses, naming the gap,
/// exactly like every other binary that now reads config through one door.
#[test]
fn target_reap_with_no_spira_home_refuses_rather_than_searching() {
    let t = testkit::TempDir::new("target-reap-cli-bare");
    let worktrees = t.path().join("worktrees");
    fs::create_dir_all(&worktrees).unwrap();

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
        // Deliberately no SPIRA_HOME, no SPIRA_TOML.
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));

    assert!(!out.status.success(), "a bare env (no SPIRA_HOME) must refuse, not search beside its own executable: {text}");
    assert!(text.contains("SPIRA_HOME is not set"), "the refusal must name the gap: {text}");
}
