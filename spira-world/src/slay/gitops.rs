//! The plain-git half of slay's real git adapter, so it runs against a real repository in a
//! test: the destruction itself stays `hel`'s chokepoint.

use std::path::Path;
use std::process::Stdio;

use hel::git::Git;

pub fn short_tip(repo: &Path, br: &str) -> Option<String> {
    Git(repo).out(&["rev-parse", "--short", br]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn names_bead_between(repo: &Path, base: &str, br: &str, id: &str) -> bool {
    Git(repo).out(&["log", "--format=%s%n%b", &format!("{base}..{br}")]).is_some_and(|l| l.contains(id))
}

pub fn abort_inflight(wt: &Path) {
    let g = Git(wt);
    let _ = g.ok(&["rebase", "--abort"]);
    let _ = g.ok(&["merge", "--abort"]);
}

/// Stage everything and commit it, hooks skipped: the dirty-stage guard exists to stop a
/// builder staging by accident, and this is the deliberate staging of a dwarf's leftovers.
pub fn commit_all(wt: &Path, msg: &str) -> bool {
    Git(wt).ok(&["add", "-A"])
        && spira_config::bounded::bounded("git")
            .arg("-C")
            .arg(wt)
            .args(["-c", "user.name=slay", "-c", "user.email=slay@spira.local", "-c", "commit.gpgsign=false", "commit", "--no-verify", "-m", msg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

pub fn park(repo: &Path, refname: &str, br: &str) -> bool {
    Git(repo).update_ref(refname, br)
}

#[cfg(test)]
mod tests {
    use super::*;
    use testkit::{git, TempDir};

    fn repo() -> TempDir {
        let d = TempDir::new("slay-gitops");
        git(&d, &["init", "-q", "-b", "main"]);
        git(&d, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&d, &["checkout", "-q", "-b", "spira/sp-g1"]);
        std::fs::write(d.join("work.txt"), "work").unwrap();
        git(&d, &["add", "work.txt"]);
        git(&d, &["commit", "-q", "-m", "sp-g1 work"]);
        d
    }

    // covers: UC-landed-audit-reaping-23
    #[test]
    fn parking_makes_a_ref_at_the_branch_sha() {
        let d = repo();
        assert!(park(&d, "refs/slain/sp-g1", "spira/sp-g1"));
        assert_eq!(git(&d, &["rev-parse", "refs/slain/sp-g1"]), git(&d, &["rev-parse", "spira/sp-g1"]));
    }

    // covers: UC-dwarf-execution-26
    #[test]
    fn parking_a_branch_that_is_not_there_fails() {
        let d = repo();
        assert!(!park(&d, "refs/slain/sp-g1", "spira/no-such"));
    }

    // covers: UC-dwarf-execution-26
    #[test]
    fn a_dirty_tree_is_committed_as_the_wip_even_where_a_pre_commit_guard_refuses() {
        let d = repo();
        let hook = d.join(".git/hooks/pre-commit");
        testkit::write_exe(&hook, "#!/bin/sh\nexit 1\n");
        std::fs::write(d.join("unsaved.txt"), "uncommitted").unwrap();
        let before = git(&d, &["rev-parse", "HEAD"]);
        assert!(commit_all(&d, "sp-g1: wip — salvaged at slay (halted)"));
        assert_ne!(git(&d, &["rev-parse", "HEAD"]), before);
        assert_eq!(git(&d, &["log", "-1", "--format=%s"]), "sp-g1: wip — salvaged at slay (halted)");
        assert_eq!(git(&d, &["ls-tree", "--name-only", "HEAD", "unsaved.txt"]), "unsaved.txt", "the leftover went into the commit");
    }

    // covers: UC-dwarf-execution-26
    #[test]
    fn a_clean_tree_has_nothing_to_commit() {
        let d = repo();
        assert!(!commit_all(&d, "sp-g1: wip"));
    }

    // covers: UC-dwarf-execution-26
    #[test]
    fn the_bead_is_named_only_by_a_commit_of_its_own() {
        let d = repo();
        assert!(names_bead_between(&d, "main", "spira/sp-g1", "sp-g1"));
        assert!(!names_bead_between(&d, "main", "spira/sp-g1", "sp-other"));
        assert!(!names_bead_between(&d, "main", "spira/no-such", "sp-g1"));
    }

    // covers: UC-dwarf-execution-26
    #[test]
    fn the_short_tip_is_the_branch_tip_and_absent_for_no_branch() {
        let d = repo();
        let full = git(&d, &["rev-parse", "spira/sp-g1"]);
        assert!(full.starts_with(&short_tip(&d, "spira/sp-g1").unwrap()));
        assert_eq!(short_tip(&d, "spira/no-such"), None);
    }
}
