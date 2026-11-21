//! `test-id-duplicate` — a literal `#[test_id(<id>)]` appears once in the tree, test site or
//! macro invocation argument alike.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::rust_test::literal_test_ids;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "test-id-duplicate";

pub struct TestIdDuplicate {
    ids: AtomicUsize,
}

impl Rule for TestIdDuplicate {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        super::test_id_unstamped::in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let mut seen: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        let mut n = 0;
        for e in crate::scope(tree, self)? {
            let Some(src) = tree.content(e) else { continue };
            for (line, id) in literal_test_ids(src) {
                n += 1;
                seen.entry(id).or_default().push((e.path.clone(), line));
            }
        }
        self.ids.store(n, Ordering::Relaxed);
        let mut out = Vec::new();
        for (id, at) in seen.iter().filter(|(_, at)| at.len() > 1) {
            for (i, (path, line)) in at.iter().enumerate() {
                let other = &at[usize::from(i == 0)];
                out.push(Finding {
                    rule: NAME,
                    path: path.clone(),
                    line: Some(*line),
                    message: format!("test id {id} is also at {}:{}: delete one #[test_id] and run test-registry stamp --path {path}", other.0, other.1),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "An id names one test's intent. Never copy an id: the copy gets a fresh one from \
test-registry stamp --path <file> once its #[test_id] line is deleted."
    }

    fn checked(&self) -> Option<(usize, String)> {
        Some((self.ids.load(Ordering::Relaxed), "ids".into()))
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(TestIdDuplicate { ids: AtomicUsize::new(0) })]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(files: &[(&str, &str)]) -> (Vec<String>, Option<(usize, String)>) {
        let t = TempDir::new("tiddup");
        for (p, s) in files {
            t.write(p, s);
        }
        let tree = Tree::from_paths(t.path(), files.iter().map(|f| f.0), std::iter::empty::<&str>());
        let rule = TestIdDuplicate { ids: AtomicUsize::new(0) };
        let found = rule.check(&tree).unwrap().iter().map(|f| f.to_string()).collect();
        (found, rule.checked())
    }

    #[test]
    fn two_sites_with_one_id_are_two_findings_each_naming_the_other() {
        let (got, checked) = run(&[
            ("a/src/lib.rs", "#[test_id(t0aaaaa)]\n#[test]\nfn x() {}\n"),
            ("b/src/lib.rs", "// pad\n#[test_id(t0aaaaa)]\n#[test]\nfn y() {}\n"),
        ]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("test-id-duplicate: a/src/lib.rs:1:") && got[0].contains("b/src/lib.rs:2"), "{got:?}");
        assert!(got[1].starts_with("test-id-duplicate: b/src/lib.rs:2:") && got[1].contains("a/src/lib.rs:1"), "{got:?}");
        assert_eq!(checked, Some((2, "ids".into())));
    }

    #[test]
    fn distinct_ids_and_macro_fragments_are_no_finding() {
        let (got, checked) = run(&[(
            "a/src/lib.rs",
            "#[test_id(t0aaaaa)]\n#[test]\nfn x() {}\n#[test_id(t0bbbbb)]\n#[test]\nfn y() {}\nmacro_rules! m { ($( #[test_id($id:ident)] $n:ident ),*) => { $( #[test_id($id)] #[test] fn $n() {} )* } }\nm!(#[test_id(t0ccccc)] z);\n",
        )]);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(checked.map(|c| c.0), Some(3), "a macro argument counts, a $ fragment does not");
    }
}
