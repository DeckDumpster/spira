//! The handful of read-only git calls the checks need. Each mirrors one bash line exactly
//! (`git [-C repo] rev-parse --verify --quiet <ref>`, `merge-base --is-ancestor`, `log
//! --oneline`) — no repo library, because the bash never used one either.

use std::process::Command;

fn git(repo: Option<&str>, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("git");
    if let Some(r) = repo {
        cmd.arg("-C").arg(r);
    }
    cmd.args(args);
    cmd.output().unwrap_or_else(|_| std::process::Output {
        status: std::process::ExitStatus::default(),
        stdout: Vec::new(),
        stderr: Vec::new(),
    })
}

/// `git [-C repo] rev-parse --verify --quiet <ref>` — exit 0 means the ref exists.
pub fn ref_exists(repo: Option<&str>, refname: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", refname])
        .status
        .success()
}

/// `git [-C repo] merge-base --is-ancestor <tip> <onto>` — exit 0 means `tip` is already an
/// ancestor of (already merged into) `onto`.
pub fn is_ancestor(repo: Option<&str>, tip: &str, onto: &str) -> bool {
    git(repo, &["merge-base", "--is-ancestor", tip, onto])
        .status
        .success()
}

/// Count of `git [-C repo] log --oneline <rev>` lines matching any of `needles` — the bash's
/// `git log --oneline "$ref" | grep -E '(a|b)' | wc -l`. A failed `log` (bad ref, no repo)
/// yields 0, same as the bash's `|| _tc_async_on_main=0`.
pub fn oneline_log_matches(repo: Option<&str>, rev: &str, needles: &[&str]) -> u32 {
    let out = git(repo, &["log", "--oneline", rev]);
    if !out.status.success() {
        return 0;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter(|l| needles.iter().any(|n| l.contains(n)))
        .count() as u32
}

/// `. "$SPIRA_HOME/lib.sh"; spira_landref "$repo"` — lib.sh is out of this bead's scope
/// (DESIGN.md §3); this is the one call site that needs its base-ref resolution, reached
/// exactly the way `--disabled-timer-check` reaches world.sh/ctrl.sh: a fixed `bash -c`
/// seam, never a second hand-written resolver.
pub fn spira_landref(spira_home: &str, repo: &str) -> Option<String> {
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#". "$1/lib.sh" >/dev/null 2>&1 && spira_landref "$2""#)
        .arg("_")
        .arg(spira_home)
        .arg(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Cmd;

    fn init_repo(dir: &std::path::Path) {
        Cmd::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(dir)
            .status()
            .unwrap();
        Cmd::new("git")
            .args(["-C"])
            .arg(dir)
            .args(["config", "user.email", "t@t"])
            .status()
            .unwrap();
        Cmd::new("git")
            .args(["-C"])
            .arg(dir)
            .args(["config", "user.name", "t"])
            .status()
            .unwrap();
        Cmd::new("git")
            .args(["-C"])
            .arg(dir)
            .args(["commit", "-q", "--allow-empty", "-m", "init"])
            .status()
            .unwrap();
    }

    #[test]
    fn ref_exists_true_for_a_real_branch_false_otherwise() {
        let d = testkit::TempDir::new("wt-git-ref");
        init_repo(&d);
        Cmd::new("git")
            .args(["-C"])
            .arg(&d)
            .args(["branch", "spira/sp-x"])
            .status()
            .unwrap();
        assert!(ref_exists(Some(d.to_str().unwrap()), "refs/heads/spira/sp-x"));
        assert!(!ref_exists(Some(d.to_str().unwrap()), "refs/heads/spira/nope"));
    }

    #[test]
    fn is_ancestor_true_when_tip_already_on_main() {
        let d = testkit::TempDir::new("wt-git-anc");
        init_repo(&d);
        let head = String::from_utf8(
            Cmd::new("git")
                .args(["-C"])
                .arg(&d)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert!(is_ancestor(Some(d.to_str().unwrap()), &head, "main"));
        assert!(!is_ancestor(
            Some(d.to_str().unwrap()),
            "0000000000000000000000000000000000000000",
            "main"
        ));
    }

    #[test]
    fn oneline_log_matches_counts_lines_and_defaults_to_zero_on_bad_ref() {
        let d = testkit::TempDir::new("wt-git-log");
        init_repo(&d);
        assert_eq!(oneline_log_matches(Some(d.to_str().unwrap()), "main", &["sp-c8w16"]), 0);
        assert_eq!(
            oneline_log_matches(Some(d.to_str().unwrap()), "does-not-exist", &["sp-c8w16"]),
            0
        );
    }
}
