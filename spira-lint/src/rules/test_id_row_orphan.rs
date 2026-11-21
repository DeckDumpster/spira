//! `test-id-row-orphan` — a tip does not remove the last `#[test_id(<id>)]` an active registry
//! `test:<id>` row names. The rows come from the file `SPIRA_TEST_DENY_LIST` names.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use spira_config::unit_tier::{DenyList, DENY_LIST_ENV};

use crate::rust_test::literal_test_ids;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "test-id-row-orphan";
const GATE_BEAD: &str = "SPIRA_GATE_BEAD";

type Cfg = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

pub struct TestIdRowOrphan {
    cfg: Cfg,
    rows: AtomicUsize,
}

impl TestIdRowOrphan {
    fn deny_list(&self) -> Result<(String, DenyList), LintError> {
        let path = (self.cfg)(DENY_LIST_ENV).map_err(LintError::Refused)?;
        if path.is_empty() {
            return Ok((path, DenyList::default()));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| LintError::Refused(format!("{DENY_LIST_ENV} {path}: {e}")))?;
        let list = DenyList::parse(&text).map_err(|e| LintError::Refused(format!("{DENY_LIST_ENV} {path}: {e}")))?;
        Ok((path, list))
    }
}

impl Rule for TestIdRowOrphan {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        super::test_id_unstamped::in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let (deny_path, list) = self.deny_list()?;
        let rows = list.test_rows();
        self.rows.store(rows.len(), Ordering::Relaxed);
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let base = tree.base_commit()?;
        let own_bead = (self.cfg)(GATE_BEAD).unwrap_or_default();
        let mut carried = BTreeSet::new();
        for e in crate::scope(tree, self)? {
            if let Some(src) = tree.content(e) {
                carried.extend(literal_test_ids(src).into_iter().map(|(_, id)| id));
            }
        }
        let mut out = Vec::new();
        for (state, id, bead) in rows {
            if carried.contains(id) || (!own_bead.is_empty() && bead == own_bead) {
                continue;
            }
            let path = self.file_at(tree, &base, id).unwrap_or_else(|| deny_path.clone());
            out.push(Finding {
                rule: NAME,
                path,
                line: None,
                message: format!(
                    "test:{id} is {state} on {bead} and no test carries it any more: clear the row (test-registry activate --test test:{id} --reason … --actor …) or keep the test"
                ),
            });
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A deny-list row names a test by its id. A tip that deletes that test leaves the row \
pointing at nothing; clear the row first, or keep the test. A bead may delete its own disabled \
test: its landing clears the row."
    }

    fn checked(&self) -> Option<(usize, String)> {
        Some((self.rows.load(Ordering::Relaxed), "registry rows".into()))
    }
}

impl TestIdRowOrphan {
    fn file_at(&self, tree: &Tree, base: &str, id: &str) -> Option<String> {
        let out = tree.git(&["grep", "-l", "-F", "-e", &format!("test_id({id})"), base, "--", "*.rs"]).ok()?;
        String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|l| l.strip_prefix(base)?.strip_prefix(':'))
            .map(str::to_string)
            .next()
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    let cfg: Cfg = Box::new(|k| if k == GATE_BEAD { Ok(std::env::var(k).unwrap_or_default()) } else { spira_config::process::cfg(k) });
    vec![Box::new(TestIdRowOrphan { cfg, rows: AtomicUsize::new(0) })]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const ROW: &str = "disabled\ttest:t0aaaaa\tsp-x\t-\n";

    fn rule(deny: &str, gate_bead: &str) -> TestIdRowOrphan {
        let (deny, bead) = (deny.to_string(), gate_bead.to_string());
        let cfg: Cfg = Box::new(move |k| Ok(if k == GATE_BEAD { bead.clone() } else { deny.clone() }));
        TestIdRowOrphan { cfg, rows: AtomicUsize::new(0) }
    }

    fn tree(t: &TempDir, tip: &str, base: Option<&str>) -> Tree {
        t.write("c/src/lib.rs", tip);
        let tree = Tree::from_paths(t.path(), ["c/src/lib.rs"], std::iter::empty::<&str>());
        let Some(base_src) = base else { return tree };
        let base_src = base_src.to_string();
        tree.with_base(Some("main".into())).with_git(Box::new(move |a| match a[0] {
            "rev-parse" => Ok(b"B\n".to_vec()),
            "merge-base" => Ok(b"F\n".to_vec()),
            "grep" if base_src.contains(a[4].trim_start_matches("test_id(").trim_end_matches(')')) => Ok(b"F:c/src/old.rs\n".to_vec()),
            "grep" => Ok(Vec::new()),
            _ => Err("unexpected".into()),
        }))
    }

    const WITH_ID: &str = "#[test_id(t0aaaaa)]\n#[test]\nfn a() {}\n";

    #[test]
    fn a_row_whose_id_the_tip_removed_is_a_finding_at_the_file_the_base_carried_it_in() {
        let t = TempDir::new("tidorphan");
        t.write("deny", ROW);
        let r = rule(t.path().join("deny").to_str().unwrap(), "");
        let got = r.check(&tree(&t, "fn a() {}\n", Some(WITH_ID))).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "c/src/old.rs");
        assert!(got[0].message.starts_with("test:t0aaaaa is disabled on sp-x and no test carries it any more: clear the row (test-registry activate"), "{got:?}");
        assert_eq!(r.checked(), Some((1, "registry rows".into())));
    }

    #[test]
    fn a_row_whose_id_the_tip_still_carries_is_no_finding() {
        let t = TempDir::new("tidorphan-kept");
        t.write("deny", ROW);
        let r = rule(t.path().join("deny").to_str().unwrap(), "");
        assert!(r.check(&tree(&t, WITH_ID, Some(WITH_ID))).unwrap().is_empty());
    }

    #[test]
    fn the_gates_own_bead_may_delete_its_disabled_test() {
        let t = TempDir::new("tidorphan-own");
        t.write("deny", ROW);
        let r = rule(t.path().join("deny").to_str().unwrap(), "sp-x");
        assert!(r.check(&tree(&t, "fn a() {}\n", Some(WITH_ID))).unwrap().is_empty());
    }

    #[test]
    fn an_empty_deny_file_checks_zero_rows_and_refuses_nothing() {
        let t = TempDir::new("tidorphan-empty");
        t.write("deny", "# test-registry deny-list at=1 rows=0\n");
        let r = rule(t.path().join("deny").to_str().unwrap(), "");
        assert!(r.check(&tree(&t, "fn a() {}\n", None)).unwrap().is_empty());
        assert_eq!(r.checked(), Some((0, "registry rows".into())));
    }

    #[test]
    fn test_rows_with_no_base_are_refused() {
        let t = TempDir::new("tidorphan-nobase");
        t.write("deny", ROW);
        let r = rule(t.path().join("deny").to_str().unwrap(), "");
        let err = r.check(&tree(&t, "fn a() {}\n", None)).unwrap_err();
        assert!(matches!(err, LintError::Refused(_)), "{err}");
    }
}
