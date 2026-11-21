//! `no-new-shell-suite` — a branch may not add a shell test suite. "New" is read from the
//! base's tree, so no list of suites is kept anywhere. Contract: DESIGN.md.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;

use crate::{is_suite, Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "no-new-shell-suite";

#[derive(Default)]
pub struct NoNewShellSuite {
    /// (suites in the tree, suites at the base) after a clean check.
    checked: Cell<Option<(usize, usize)>>,
}

impl Rule for NoNewShellSuite {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_suite(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let base = tree.base_commit()?;
        let listing = tree
            .git(&["ls-tree", "--name-only", &base, "spira/"])
            .map_err(|e| LintError::Refused(format!("listing the base's suites: {e}")))?;
        let at_base: BTreeSet<String> =
            String::from_utf8_lossy(&listing).lines().filter(|l| is_suite(l)).map(str::to_string).collect();
        let mut here = 0;
        let mut out = Vec::new();
        let suites = match crate::scope(tree, self) {
            Ok(s) => s,
            Err(LintError::EmptyScope) => Vec::new(),
            Err(e) => return Err(e),
        };
        for e in suites {
            if tree.content(e).is_none() {
                continue;
            }
            here += 1;
            if !at_base.contains(&e.path) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: None,
                    message: "new shell test suite: a test is written in Rust — port this to a Rust test in the crate it exercises; a shell suite that exists is ported or deleted, never grown".into(),
                });
            }
        }
        if out.is_empty() {
            self.checked.set(Some((here, at_base.len())));
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "No new shell test suite: a test is written in Rust; a shell suite that exists is ported or deleted, never grown. Write the test in Rust."
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|(n, base)| (n, format!("suites ({base} at the base)")))
    }
}

pub fn rules() -> Vec<Box<dyn Rule>> {
    vec![Box::new(NoNewShellSuite::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn tree(t: &TempDir, tracked: &[&str], untracked: &[&str], at_base: &'static str, ls_fails: bool) -> Tree {
        for p in tracked.iter().chain(untracked) {
            t.write(p, "#!/bin/bash\n");
        }
        Tree::from_paths(t.path(), tracked.iter().map(|s| s.to_string()), untracked.iter().map(|s| s.to_string()))
            .with_base(Some("main".into()))
            .with_git(Box::new(move |a| match a {
                ["rev-parse", ..] => Ok(b"B\n".to_vec()),
                ["merge-base", ..] => Ok(b"F\n".to_vec()),
                ["ls-tree", "--name-only", "F", "spira/"] if !ls_fails => Ok(at_base.as_bytes().to_vec()),
                other => Err(format!("unexpected {other:?}")),
            }))
    }

    fn paths(f: Vec<Finding>) -> Vec<String> {
        f.into_iter().map(|f| f.path).collect()
    }

    const A: &str = "spira/test-a.sh\n";

    #[test_id(trfj9dn)]
    #[test]
    fn an_untracked_new_suite_is_refused() {
        let t = TempDir::new("nnss");
        let tr = tree(&t, &["spira/test-a.sh"], &["spira/test-new.sh"], A, false);
        assert_eq!(paths(NoNewShellSuite::default().check(&tr).unwrap()), ["spira/test-new.sh"]);
    }

    #[test_id(tobkopa)]
    #[test]
    fn a_tracked_new_suite_is_refused() {
        let t = TempDir::new("nnss");
        let tr = tree(&t, &["spira/test-a.sh", "spira/test-new.sh"], &[], A, false);
        assert_eq!(paths(NoNewShellSuite::default().check(&tr).unwrap()), ["spira/test-new.sh"]);
    }

    #[test_id(tr3vtsp)]
    #[test]
    fn an_edited_existing_suite_is_not_refused() {
        let t = TempDir::new("nnss");
        let tr = tree(&t, &["spira/test-a.sh"], &[], A, false);
        t.write("spira/test-a.sh", "changed\n");
        let rule = NoNewShellSuite::default();
        assert!(rule.check(&tr).unwrap().is_empty());
        assert_eq!(rule.checked().unwrap().0, 1);
    }

    #[test_id(ts8diln)]
    #[test]
    fn a_deleted_suite_is_not_refused() {
        let t = TempDir::new("nnss");
        let tr = tree(&t, &["spira/test-a.sh"], &[], "spira/test-a.sh\nspira/test-b.sh\n", false);
        assert!(NoNewShellSuite::default().check(&tr).unwrap().is_empty());
    }

    #[test_id(td6n83u)]
    #[test]
    fn files_that_are_not_direct_test_sh_children_are_not_suites() {
        let t = TempDir::new("nnss");
        let tr = tree(
            &t,
            &["spira/test-a.sh", "spira/testlib/new.sh", "spira/sub/test-x.sh", "spira/test-x.rs"],
            &[],
            A,
            false,
        );
        assert!(NoNewShellSuite::default().check(&tr).unwrap().is_empty());
    }

    #[test_id(t2y1up1)]
    #[test]
    fn a_branch_deleting_every_suite_is_not_refused() {
        let t = TempDir::new("nnss");
        let tr = tree(&t, &["spira/other.sh"], &[], A, false);
        assert!(NoNewShellSuite::default().check(&tr).unwrap().is_empty());
    }

    #[test_id(tj1bzze)]
    #[test]
    fn no_base_refuses() {
        let t = TempDir::new("nnss");
        t.write("spira/test-a.sh", "x\n");
        let tr = Tree::from_paths(t.path(), ["spira/test-a.sh"], Vec::<String>::new());
        assert!(matches!(NoNewShellSuite::default().check(&tr), Err(LintError::Refused(_))));
    }

    #[test_id(tqaxrk7)]
    #[test]
    fn a_failing_ls_tree_refuses() {
        let t = TempDir::new("nnss");
        let tr = tree(&t, &["spira/test-a.sh"], &[], A, true);
        assert!(matches!(NoNewShellSuite::default().check(&tr), Err(LintError::Refused(_))));
    }
}
