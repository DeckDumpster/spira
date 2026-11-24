//! `no-new-shell-suite` — a branch may not add a shell test suite. "New" is read from the
//! base's tree, so no list of suites is kept anywhere. Contract: DESIGN.md.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;

use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "no-new-shell-suite";

fn is_shell_test(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let Some(base) = parts.pop() else { return false };
    if parts.contains(&"fixtures") || parts.first() == Some(&"spira-lint") {
        return false;
    }
    let script = [".sh", ".bash", ".bats"].iter().any(|x| base.ends_with(x));
    script && (base.starts_with("test-") && base.ends_with(".sh") || parts.iter().any(|d| ["test", "tests", "testlib"].contains(d)))
}

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
        is_shell_test(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let base = tree.base_commit()?;
        let listing = tree
            .git(&["ls-tree", "-r", "--name-only", &base])
            .map_err(|e| LintError::Refused(format!("listing the base's shell tests: {e}")))?;
        let at_base: BTreeSet<String> =
            String::from_utf8_lossy(&listing).lines().filter(|l| is_shell_test(l)).map(str::to_string).collect();
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
                    message: "new shell test: a test is written in Rust, never shell — write it as a Rust test in the crate it exercises, with a `// covers: UC-<area>-NN` line naming its cell".into(),
                });
            }
        }
        if out.is_empty() {
            self.checked.set(Some((here, at_base.len())));
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "No new shell test: write it as a Rust test in the crate it exercises, with a `// covers: UC-<area>-NN` line naming its cell."
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|(n, base)| (n, format!("shell tests ({base} at the base)")))
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
                ["ls-tree", "-r", "--name-only", "F"] if !ls_fails => Ok(at_base.as_bytes().to_vec()),
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

    #[test_id(tse0r6k)]
    #[test]
    fn shell_tests_outside_spira_are_refused() {
        let t = TempDir::new("nnss");
        let tr = tree(
            &t,
            &["spira/test-a.sh", "cockpit/test-x.sh", "install/tests/x.sh", "spira/testlib/new.sh", "x/test/y.bats"],
            &[],
            A,
            false,
        );
        assert_eq!(
            paths(NoNewShellSuite::default().check(&tr).unwrap()),
            ["cockpit/test-x.sh", "install/tests/x.sh", "spira/testlib/new.sh", "x/test/y.bats"]
        );
    }

    #[test_id(todooa7)]
    #[test]
    fn the_message_names_the_rust_exit_and_the_cell() {
        let t = TempDir::new("nnss");
        let tr = tree(&t, &["cockpit/test-x.sh"], &[], "", false);
        let m = NoNewShellSuite::default().check(&tr).unwrap().remove(0).message;
        assert!(m.contains("Rust test") && m.contains("// covers: UC-<area>-NN"), "{m}");
    }

    #[test_id(tg2ulrz)]
    #[test]
    fn existing_shell_tests_pass_and_fixtures_and_libs_are_not_tests() {
        let t = TempDir::new("nnss");
        let base = "cockpit/test-x.sh\ninstall/tests/x.sh\nspira/testlib/new.sh\n";
        let tr = tree(
            &t,
            &["cockpit/test-x.sh", "install/tests/x.sh", "spira/testlib/new.sh", "lifecycle-guard/tests/fixtures/x/y.sh", "spira-lint/config-fence-allow.d/spira/test-x.sh", "spira/lib-x.sh", "spira/test-x.rs"],
            &[],
            base,
            false,
        );
        t.write("cockpit/test-x.sh", "changed\n");
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
