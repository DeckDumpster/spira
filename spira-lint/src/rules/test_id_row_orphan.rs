//! `test-id-row-orphan` — a tip does not remove the last `#[test_id(<id>)]` an active registry
//! `test:<id>` row names. The rows come from the file the caller
//! hands spira-lint with `--deny-list` (the gate's `SPIRA_TEST_DENY_LIST`), carried on the `Tree`.
//!
//! The checked count is registries read, not rows: a registry with no `test:` row is a check
//! that ran. No configured path reads nothing and counts 0, which the gate treats as silent.

use std::collections::BTreeSet;
use std::sync::Mutex;

use spira_config::unit_tier::{DenyList, DENY_LIST_ENV};

use crate::rust_test::literal_test_ids;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "test-id-row-orphan";
const GATE_BEAD: &str = "SPIRA_GATE_BEAD";

type Cfg = Box<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

pub struct TestIdRowOrphan {
    cfg: Cfg,
    read: Mutex<Option<Read>>,
}

struct Read {
    path: String,
    rows: usize,
    test_rows: usize,
}

impl TestIdRowOrphan {
    fn deny_list(tree: &Tree) -> Result<(String, DenyList), LintError> {
        let Some(path) = tree.deny_list().map(str::to_string) else {
            return Ok((String::new(), DenyList::default()));
        };
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
        let (deny_path, list) = Self::deny_list(tree)?;
        let rows = list.test_rows();
        let read = (!deny_path.is_empty()).then(|| Read { path: deny_path.clone(), rows: list.len(), test_rows: rows.len() });
        *self.read.lock().unwrap() = read;
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
        Some(match &*self.read.lock().unwrap() {
            Some(r) => (1, format!("registry ({}: {} rows, {} test rows)", r.path, r.rows, r.test_rows)),
            None => (0, "registry (none configured)".into()),
        })
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
    let cfg: Cfg = Box::new(|k| Ok(std::env::var(k).unwrap_or_default()));
    vec![Box::new(TestIdRowOrphan { cfg, read: Mutex::new(None) })]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const ROW: &str = "disabled\ttest:t0aaaaa\tsp-x\t-\n";

    fn rule(gate_bead: &str) -> TestIdRowOrphan {
        let bead = gate_bead.to_string();
        let cfg: Cfg = Box::new(move |k| Ok(if k == GATE_BEAD { bead.clone() } else { String::new() }));
        TestIdRowOrphan { cfg, read: Mutex::new(None) }
    }

    fn tree(t: &TempDir, tip: &str, base: Option<&str>) -> Tree {
        t.write("c/src/lib.rs", tip);
        let deny = t.path().join("deny");
        let tree = Tree::from_paths(t.path(), ["c/src/lib.rs"], std::iter::empty::<&str>())
            .with_deny_list(deny.exists().then(|| deny.to_str().unwrap().to_string()));
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

    #[test_id(tzzin2i)]
    #[test]
    fn a_row_whose_id_the_tip_removed_is_a_finding_at_the_file_the_base_carried_it_in() {
        let t = TempDir::new("tidorphan");
        t.write("deny", ROW);
        let r = rule("");
        let got = r.check(&tree(&t, "fn a() {}\n", Some(WITH_ID))).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, "c/src/old.rs");
        assert!(got[0].message.starts_with("test:t0aaaaa is disabled on sp-x and no test carries it any more: clear the row (test-registry activate"), "{got:?}");
        let (n, unit) = r.checked().unwrap();
        assert_eq!(n, 1);
        assert!(unit.contains("1 rows, 1 test rows"), "{unit}");
    }

    #[test_id(tiasoef)]
    #[test]
    fn a_row_whose_id_the_tip_still_carries_is_no_finding() {
        let t = TempDir::new("tidorphan-kept");
        t.write("deny", ROW);
        let r = rule("");
        assert!(r.check(&tree(&t, WITH_ID, Some(WITH_ID))).unwrap().is_empty());
    }

    #[test_id(trxjoco)]
    #[test]
    fn the_gates_own_bead_may_delete_its_disabled_test() {
        let t = TempDir::new("tidorphan-own");
        t.write("deny", ROW);
        let r = rule("sp-x");
        assert!(r.check(&tree(&t, "fn a() {}\n", Some(WITH_ID))).unwrap().is_empty());
    }

    #[test_id(t482td6)]
    #[test]
    fn an_empty_deny_file_checks_one_registry_with_zero_test_rows() {
        let t = TempDir::new("tidorphan-empty");
        t.write("deny", "# test-registry deny-list at=1 rows=0\n");
        let r = rule("");
        assert!(r.check(&tree(&t, "fn a() {}\n", None)).unwrap().is_empty());
        let (n, unit) = r.checked().unwrap();
        assert_eq!(n, 1);
        assert!(unit.contains("deny") && unit.contains("0 rows, 0 test rows"), "{unit}");
    }

    #[test_id(tfjba2u)]
    #[test]
    fn a_registry_of_only_a_suite_row_checks_one_registry_with_no_findings() {
        let t = TempDir::new("tidorphan-suite");
        t.write("deny", "disabled\tsuite:test-example.sh\tsp-x22nuk\t-\n");
        let r = rule("");
        assert!(r.check(&tree(&t, "fn a() {}\n", None)).unwrap().is_empty());
        let (n, unit) = r.checked().unwrap();
        assert_eq!(n, 1);
        assert!(unit.contains("1 rows, 0 test rows"), "{unit}");
    }

    #[test_id(t2lo8we)]
    #[test]
    fn no_configured_path_reads_no_registry_and_checks_zero() {
        let t = TempDir::new("tidorphan-none");
        let r = rule("");
        assert!(r.check(&tree(&t, "fn a() {}\n", None)).unwrap().is_empty());
        assert_eq!(r.checked().unwrap().0, 0);
    }

    #[test_id(tf5pirx)]
    #[test]
    fn test_rows_with_no_base_are_refused() {
        let t = TempDir::new("tidorphan-nobase");
        t.write("deny", ROW);
        let r = rule("");
        let err = r.check(&tree(&t, "fn a() {}\n", None)).unwrap_err();
        assert!(matches!(err, LintError::Refused(_)), "{err}");
    }

    #[test_id(tnd0tym)]
    #[test]
    fn a_tree_handed_a_deny_file_checks_one_registry_naming_it() {
        let t = TempDir::new("tidorphan-handed");
        t.write("deny", ROW);
        let r = rule("");
        r.check(&tree(&t, WITH_ID, Some(WITH_ID))).unwrap();
        let (n, unit) = r.checked().unwrap();
        let path = t.path().join("deny");
        assert_eq!((n, unit), (1, format!("registry ({}: 1 rows, 1 test rows)", path.display())));
    }

    #[test_id(t3cbq2a)]
    #[test]
    fn a_tree_handed_no_deny_file_reports_none_configured() {
        let t = TempDir::new("tidorphan-unhanded");
        let r = rule("");
        r.check(&tree(&t, "fn a() {}\n", None)).unwrap();
        assert_eq!(r.checked().unwrap(), (0, "registry (none configured)".to_string()));
    }

    #[test_id(tbdv8ky)]
    #[test]
    fn a_missing_deny_file_is_refused_naming_the_path() {
        let t = TempDir::new("tidorphan-missing");
        let missing = t.path().join("nope");
        let tr = tree(&t, "fn a() {}\n", None).with_deny_list(Some(missing.to_str().unwrap().into()));
        match rule("").check(&tr) {
            Err(LintError::Refused(m)) => assert!(m.contains(missing.to_str().unwrap()), "{m}"),
            other => panic!("{:?}", other.map(|v| v.len())),
        }
    }
}
