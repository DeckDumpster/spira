//! `testlib-migrated` — no suite redefines a testlib.sh assertion primitive.
//! Ported from `spira/test-testlib-migrated.sh`. Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{direct_child, lines, Entry, Finding, LintError, Rule, Tree};

pub struct TestlibMigrated;

const NAME: &str = "testlib-migrated";
const ALLOW_FILE: &str = "spira-lint/testlib-migrated-allow";

/// A line that defines one of testlib.sh's primitives at the start of the line.
fn defines_primitive(content: &[u8]) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^(ok|bad|fail|is|want|nowant|notwant|wantrc)\(\)").expect("static regex")
    });
    lines(content).iter().any(|l| re.is_match(l))
}

/// `spira/test-*.sh`, directly in spira/.
fn is_suite(path: &str) -> bool {
    direct_child(path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
}

impl Rule for TestlibMigrated {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_suite(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let suites = crate::scope(tree, self)?;
        let allow: Vec<(usize, String)> = tree
            .read_text(ALLOW_FILE)
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                !(t.is_empty() || t.starts_with('#'))
            })
            .map(|(i, l)| (i + 1, l.trim().to_string()))
            .collect();
        let listed = |p: &str| allow.iter().any(|(_, a)| a == p);
        let mut out = Vec::new();
        let mut offenders = Vec::new();
        for e in suites {
            let Some(c) = tree.content(e) else { continue };
            if defines_primitive(c) {
                offenders.push(e.path.clone());
                if !listed(&e.path) {
                    out.push(Finding {
                        rule: NAME,
                        path: e.path.clone(),
                        line: None,
                        message: "defines its own ok()/bad()/is()/want()/nowant()/wantrc() — source testlib.sh instead".into(),
                    });
                }
            }
        }
        for (line, p) in &allow {
            if !offenders.iter().any(|o| o == p) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("lists {p}, which no longer needs an exception — remove the line (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Suites share testlib.sh's assertion primitives; a suite with its own ok()/want() drifts \
from the runner's counting. spira-lint/testlib-migrated-allow only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TestlibMigrated.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_primitive_is_caught_and_a_testlib_suite_is_not() {
        let t = TempDir::new("tlm");
        t.write("spira/test-dummy.sh", ". \"$HERE/testlib.sh\"\nok \"x\"\n");
        t.write("spira/test-planted.sh", "#!/bin/bash\nok() { :; }\n");
        t.write(ALLOW_FILE, "");
        let got = run(&t, &["spira/test-dummy.sh", "spira/test-planted.sh"]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("testlib-migrated: spira/test-planted.sh: defines"), "{got:?}");
        // indented or called, never defined at line start: not a redefinition
        t.write("spira/test-planted.sh", "#!/bin/bash\n  ok() { :; }\nwant a b c\n");
        assert!(run(&t, &["spira/test-dummy.sh", "spira/test-planted.sh"]).unwrap().is_empty());
    }

    #[test]
    fn every_primitive_counts() {
        for p in ["ok", "bad", "fail", "is", "want", "nowant", "notwant", "wantrc"] {
            assert!(defines_primitive(format!("x\n{p}() {{ :; }}\n").as_bytes()), "{p}");
        }
        assert!(!defines_primitive(b"okay() { :; }\n"));
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("tlm-allow");
        t.write("spira/test-old.sh", "want() { :; }\n");
        t.write("spira/test-fixed.sh", "echo\n");
        t.write(ALLOW_FILE, "# header\nspira/test-old.sh\nspira/test-fixed.sh\n");
        assert_eq!(
            run(&t, &["spira/test-old.sh", "spira/test-fixed.sh"]).unwrap(),
            vec!["testlib-migrated: spira-lint/testlib-migrated-allow:3: lists spira/test-fixed.sh, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test]
    fn nested_and_non_suite_files_are_out_of_scope() {
        let t = TempDir::new("tlm-scope");
        t.write("spira/sub/test-x.sh", "ok() { :; }\n");
        t.write("spira/lib.sh", "ok() { :; }\n");
        assert_eq!(run(&t, &["spira/sub/test-x.sh", "spira/lib.sh"]), Err(LintError::EmptyScope));
    }
}
