//! `test-id-unstamped` — every Rust `#[test]` and `#[tokio::test]` carries `#[test_id(<id>)]`.
//! Inside a `macro_rules!` body the id must be a `$` fragment the invocation supplies.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::rust_test::test_sites;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "test-id-unstamped";

pub struct TestIdUnstamped {
    tests: AtomicUsize,
}

pub fn in_scope(path: &str) -> bool {
    path.ends_with(".rs") && !path.starts_with("spira-lint/tests/fixtures/")
}

impl Rule for TestIdUnstamped {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        in_scope(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let mut out = Vec::new();
        let mut n = 0;
        for e in crate::scope(tree, self)? {
            let Some(src) = tree.content(e) else { continue };
            for site in test_sites(src) {
                n += 1;
                let stamped = match (&site.id, site.in_macro_rules) {
                    (None, _) => false,
                    (Some(id), true) => id.starts_with('$'),
                    (Some(_), false) => true,
                };
                if !stamped {
                    let message = if site.in_macro_rules {
                        format!("test {} in a macro_rules! body has no #[test_id($id)]: take the id as a $ fragment from each invocation", site.fn_name)
                    } else {
                        format!("test {} has no #[test_id]: run test-registry stamp --path {}", site.fn_name, e.path)
                    };
                    out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(site.line), message });
                }
            }
        }
        self.tests.store(n, Ordering::Relaxed);
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "The id is stable with the test's intent: a rename or a move keeps it. A test rewritten to \
check something else gets a new id: delete its #[test_id] and run test-registry stamp --path <file> \
again. Never copy an id from another test."
    }

    fn checked(&self) -> Option<(usize, String)> {
        Some((self.tests.load(Ordering::Relaxed), "tests".into()))
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(TestIdUnstamped { tests: AtomicUsize::new(0) })]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(src: &str) -> (Vec<String>, Option<(usize, String)>) {
        let t = TempDir::new("tidunstamped");
        t.write("c/src/lib.rs", src);
        let tree = Tree::from_paths(t.path(), ["c/src/lib.rs"], std::iter::empty::<&str>());
        let rule = TestIdUnstamped { tests: AtomicUsize::new(0) };
        let found = rule.check(&tree).unwrap().iter().map(|f| f.to_string()).collect();
        (found, rule.checked())
    }

    #[test_id(t7m3f51)]
    #[test]
    fn an_unstamped_test_is_one_finding_naming_the_stamp_command() {
        let (got, checked) = run("#[test]\nfn a() {}\n");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("test-id-unstamped: c/src/lib.rs:1:"), "{got:?}");
        assert!(got[0].contains("test-registry stamp --path c/src/lib.rs"), "{got:?}");
        assert_eq!(checked, Some((1, "tests".into())));
    }

    #[test_id(t7oybn5)]
    #[test]
    fn a_stamped_test_is_no_finding() {
        let (got, checked) = run("#[test_id(t0aaaaa)]\n#[test]\nfn a() {}\n");
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(checked.map(|c| c.0), Some(1), "the matcher saw the site");
    }

    #[test_id(tdi9d99)]
    #[test]
    fn a_test_attribute_in_a_string_is_no_finding() {
        let (got, checked) = run("const S: &str = \"#[test]\\nfn a() {}\";\n");
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(checked.map(|c| c.0), Some(0));
    }

    #[test_id(t9wp1fu)]
    #[test]
    fn a_macro_body_test_counts_as_stamped_only_with_a_fragment_id() {
        let ok = "macro_rules! m { ($( #[test_id($id:ident)] $n:ident ),*) => { $( #[test_id($id)] #[test] fn $n() {} )* } }\n";
        assert!(run(ok).0.is_empty());
        let bad = "macro_rules! m { ($( $n:ident ),*) => { $( #[test] fn $n() {} )* } }\n";
        let (got, _) = run(bad);
        assert_eq!(got.len(), 1, "{got:?}");
        let literal = "macro_rules! m { ($n:ident) => { #[test_id(t0aaaaa)] #[test] fn $n() {} } }\n";
        assert_eq!(run(literal).0.len(), 1, "a literal id in a macro body would repeat per invocation");
    }
}
