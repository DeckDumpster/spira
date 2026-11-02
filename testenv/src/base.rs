//! The revision a run tests: a branch as it would land, its tip merged onto the current
//! landing ref (sp-5zz517, brain designs/fresh-base-2026-11-02). The landing ref only moves
//! forward, so every local test of a repo sees one base, and with it one Cargo.lock, instead
//! of whatever base each branch was cut from (dumpstar held 29 lock variants across 348
//! branches, ~100 copies of each crate in one shared target, 2026-11-02).

use std::path::Path;
use std::process::{Command, Stdio};

/// What the run tests, given a branch tip and the landing ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Basis {
    /// The landing ref is already in the tip: the tip is the tree, unchanged.
    Tip,
    /// A merge commit of the tip onto the landing ref, made for this run.
    Merged { commit: String, tree: String, base: String },
    /// The tip does not merge onto the landing ref; these paths conflict.
    Conflict { base: String, paths: Vec<String> },
}

fn git(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> std::io::Result<std::process::Output> {
    // batch-job: part of a testenv trial, bounded by the trial deadline, not per call
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(args).env_remove("GIT_DIR").env_remove("GIT_WORK_TREE").env_remove("GIT_INDEX_FILE").stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.output()
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim_end().to_string()
}

fn err(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).trim().to_string()
}

/// Resolves the revision to test for `tip` (a commit id) against `landref` (a ref name).
/// The merge commit's identity and dates are fixed, so one (base, tip) pair always yields
/// the same commit id. Err: git could not answer at all.
pub fn onto(repo: &Path, landref: &str, tip: &str) -> Result<Basis, String> {
    let b = git(repo, &["rev-parse", "--verify", "-q", &format!("{landref}^{{commit}}")], &[]).map_err(|e| format!("git: {e}"))?;
    if !b.status.success() {
        return Err(format!("cannot resolve the landing ref {landref}"));
    }
    let base = out(&b);
    let anc = git(repo, &["merge-base", "--is-ancestor", &base, tip], &[]).map_err(|e| format!("git: {e}"))?;
    match anc.status.code() {
        Some(0) => return Ok(Basis::Tip),
        Some(1) => {}
        _ => return Err(format!("merge-base {base} {tip}: {}", err(&anc))),
    }
    let m = git(repo, &["merge-tree", "--write-tree", "--name-only", "--no-messages", &base, tip], &[]).map_err(|e| format!("git: {e}"))?;
    let text = out(&m);
    let mut lines = text.lines();
    let tree = lines.next().unwrap_or("").trim().to_string();
    match m.status.code() {
        Some(0) => {}
        Some(1) => {
            let paths: Vec<String> = lines.map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
            return Ok(Basis::Conflict { base, paths });
        }
        _ => return Err(format!("merge-tree {base} {tip}: {}", err(&m))),
    }
    let msg = format!("testenv: {tip} merged onto {landref} ({base})");
    let fixed = [
        ("GIT_AUTHOR_NAME", "testenv"),
        ("GIT_AUTHOR_EMAIL", "testenv@spira.invalid"),
        ("GIT_AUTHOR_DATE", "@0 +0000"),
        ("GIT_COMMITTER_NAME", "testenv"),
        ("GIT_COMMITTER_EMAIL", "testenv@spira.invalid"),
        ("GIT_COMMITTER_DATE", "@0 +0000"),
    ];
    let c = git(repo, &["commit-tree", &tree, "-p", &base, "-p", tip, "-m", &msg], &fixed).map_err(|e| format!("git: {e}"))?;
    if !c.status.success() {
        return Err(format!("commit-tree {tree}: {}", err(&c)));
    }
    Ok(Basis::Merged { commit: out(&c), tree, base })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let fixed = [
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@t"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@t"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ];
        let o = git(dir, args, &fixed).unwrap();
        assert!(o.status.success(), "git {args:?}: {}", err(&o));
        out(&o)
    }

    /// A repo with `main` at m1, a branch cut from m0, and main moved on to m1.
    fn repo(branch_file: &str, branch_text: &str, main_file: &str) -> (testkit::TempDir, String) {
        let d = testkit::TempDir::new("base-onto");
        let p = d.path();
        sh(p, &["init", "-q", "-b", "main"]);
        fs::write(p.join("a"), "a\n").unwrap();
        sh(p, &["add", "-A"]);
        sh(p, &["commit", "-qm", "m0"]);
        sh(p, &["checkout", "-qb", "spira/x"]);
        fs::write(p.join(branch_file), branch_text).unwrap();
        sh(p, &["add", "-A"]);
        sh(p, &["commit", "-qm", "b1"]);
        let tip = sh(p, &["rev-parse", "HEAD"]);
        sh(p, &["checkout", "-q", "main"]);
        fs::write(p.join(main_file), "main\n").unwrap();
        sh(p, &["add", "-A"]);
        sh(p, &["commit", "-qm", "m1"]);
        (d, tip)
    }

    #[test]
    fn a_stale_branch_is_tested_merged_onto_the_landing_ref_with_both_sides_present() {
        let (d, tip) = repo("b", "b\n", "only-on-main");
        let Basis::Merged { commit, base, .. } = onto(d.path(), "main", &tip).unwrap() else { panic!("not merged") };
        assert_eq!(base, sh(d.path(), &["rev-parse", "main"]));
        let files = sh(d.path(), &["ls-tree", "--name-only", &commit]);
        assert!(files.contains("only-on-main") && files.lines().any(|l| l == "b"), "{files}");
        assert_eq!(sh(d.path(), &["rev-parse", &format!("{commit}^2")]), tip);
    }

    #[test]
    fn the_same_tip_on_the_same_base_always_merges_to_the_same_commit() {
        let (d, tip) = repo("b", "b\n", "c");
        let one = onto(d.path(), "main", &tip).unwrap();
        let two = onto(d.path(), "main", &tip).unwrap();
        assert_eq!(one, two);
    }

    #[test]
    fn a_branch_that_conflicts_with_the_landing_ref_names_the_conflicting_path() {
        let (d, tip) = repo("a", "branch\n", "a");
        match onto(d.path(), "main", &tip).unwrap() {
            Basis::Conflict { paths, .. } => assert_eq!(paths, vec!["a".to_string()]),
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_branch_already_on_the_landing_ref_is_tested_as_its_own_tip() {
        let (d, _) = repo("b", "b\n", "c");
        sh(d.path(), &["checkout", "-q", "spira/x"]);
        sh(d.path(), &["merge", "-q", "--no-edit", "main"]);
        let tip = sh(d.path(), &["rev-parse", "HEAD"]);
        assert_eq!(onto(d.path(), "main", &tip).unwrap(), Basis::Tip);
    }
}
