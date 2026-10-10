//! `tmp-leak` — test code gets its scratch space from `testkit::TempDir`, never from a bare
//! `temp_dir()` it may forget to remove (sp-qgfdi). Contract: DESIGN.md.
//!
//! "Is this test code" is [`crate::rust_test`], shared with `script-callers` (sp-9y0gf) so
//! the two rules cannot define it two different ways.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct TmpLeak;

const NAME: &str = "tmp-leak";
const ALLOW_FILE: &str = "spira-lint/tmp-leak-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

impl Rule for TmpLeak {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("testkit/") && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static CALL: OnceLock<Regex> = OnceLock::new();
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();

        // Whole-file test code: by path, by inner #![cfg(test)], and by module fixpoint.
        let whole = whole_test_files(tree, &shapes);

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
        let call = re(&CALL, r"(?:^|[^A-Za-z0-9_])(temp_dir)\s*\(\s*\)");
        let mut out = Vec::new();
        let mut offenders = BTreeSet::new();
        for (e, s) in &shapes {
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let code = cl.code_only();
            let file_is_test = whole.contains(&e.path);
            for c in call.captures_iter(&code) {
                let at = c.get(1).map_or(0, |m| m.start());
                if !(file_is_test || s.regions.iter().any(|(a, b)| *a <= at && at < *b)) {
                    continue;
                }
                offenders.insert(e.path.clone());
                if allow.iter().any(|(_, p)| p == &e.path) {
                    continue;
                }
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(at)),
                    message: "temp_dir() in test code — take a testkit::TempDir, which removes itself on drop".into(),
                });
            }
        }
        for (line, p) in &allow {
            if !offenders.contains(p) {
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
        "A test's scratch directory under the system temp dir outlives the test unless something \
removes it, and /tmp is a tmpfs with a fixed inode budget (sp-qgfdi: one workspace run leaked \
157 entries, inode exhaustion 2026-09-29). Use testkit::TempDir::new(tag), which removes the \
directory on drop, panics included. spira-lint/tmp-leak-allow only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TmpLeak.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    // The fixtures spell the call through concat! so this file carries no call in code.
    const CALL: &str = concat!("std::env::", "temp_dir", "()");

    fn leaking_helper() -> String {
        format!(
            "fn dir(tag: &str) -> std::path::PathBuf {{\n    let d = {CALL}.join(tag);\n    std::fs::create_dir_all(&d).unwrap();\n    d\n}}\n"
        )
    }

    /// The failing fixture: the leak in each place test code lives.
    #[test]
    fn a_bare_temp_dir_in_test_code_is_caught_wherever_test_code_lives() {
        let t = TempDir::new("tmpleak-fail");
        t.write(ALLOW_FILE, "# shrink-only\n");
        // 1. inside #[cfg(test)] mod tests { … }
        t.write(
            "a/src/lib.rs",
            &format!("pub fn f() {{}}\n\n#[cfg(test)]\nmod tests {{\n    {}}}\n", leaking_helper()),
        );
        // 2. an integration test
        t.write("b/tests/it.rs", &leaking_helper());
        // 3. an out-of-line test module, declared under #[cfg(test)], and a module it declares
        t.write("c/src/main.rs", "fn main() {}\n#[cfg(test)]\nmod testutil;\n");
        t.write("c/src/testutil.rs", &format!("mod deeper;\n{}", leaking_helper()));
        t.write("c/src/testutil/deeper.rs", &leaking_helper());
        // 4. #[path] and a cfg(all(test, …)) fn
        t.write(
            "d/src/x.rs",
            &format!(
                "#[cfg(test)]\n#[path = \"x_tests.rs\"]\nmod t;\n#[cfg(all(test, unix))]\nfn h() {{ let _ = {CALL}; }}\n"
            ),
        );
        t.write("d/src/x_tests.rs", &leaking_helper());
        // 5. a file that is test code by its own #![cfg(test)]
        t.write("e/src/helpers.rs", &format!("#![cfg(test)]\n{}", leaking_helper()));
        let got = run(
            &t,
            &["a/src/lib.rs", "b/tests/it.rs", "c/src/main.rs", "c/src/testutil.rs", "c/src/testutil/deeper.rs", "d/src/x.rs", "d/src/x_tests.rs", "e/src/helpers.rs"],
        )
        .unwrap();
        let msg = "temp_dir() in test code — take a testkit::TempDir, which removes itself on drop";
        assert_eq!(
            got,
            vec![
                format!("tmp-leak: a/src/lib.rs:6: {msg}"),
                format!("tmp-leak: b/tests/it.rs:2: {msg}"),
                format!("tmp-leak: c/src/testutil.rs:3: {msg}"),
                format!("tmp-leak: c/src/testutil/deeper.rs:2: {msg}"),
                format!("tmp-leak: d/src/x.rs:5: {msg}"),
                format!("tmp-leak: d/src/x_tests.rs:2: {msg}"),
                format!("tmp-leak: e/src/helpers.rs:3: {msg}"),
            ]
        );
    }

    /// The passing fixture: testkit in test code, and temp_dir() where it is not test code,
    /// is in a comment or string, or sits under cfg(not(test)).
    #[test]
    fn testkit_and_non_test_code_pass() {
        let t = TempDir::new("tmpleak-pass");
        t.write(ALLOW_FILE, "");
        t.write(
            "a/src/lib.rs",
            &format!(
                "pub fn scratch() -> std::path::PathBuf {{ {CALL} }}\n\
#[cfg(not(test))]\nfn prod() {{ let _ = {CALL}; }}\n\
mod real;\n\
#[cfg(test)]\nmod tests {{\n    // never {CALL} in a test\n    const S: &str = \"{CALL}\";\n    #[test]\n    fn t() {{ let d = testkit::TempDir::new(\"a\"); let _ = d.join(\"x\"); }}\n}}\n\
pub fn after() -> std::path::PathBuf {{ {CALL} }}\n"
            ),
        );
        t.write("a/src/real.rs", &leaking_helper());
        t.write("testkit/src/lib.rs", &leaking_helper());
        assert_eq!(run(&t, &["a/src/lib.rs", "a/src/real.rs", "testkit/src/lib.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("tmpleak-allow");
        t.write("b/tests/old.rs", &leaking_helper());
        t.write("b/tests/fixed.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "# header\nb/tests/old.rs\nb/tests/fixed.rs\n");
        assert_eq!(
            run(&t, &["b/tests/old.rs", "b/tests/fixed.rs"]).unwrap(),
            vec!["tmp-leak: spira-lint/tmp-leak-allow:3: lists b/tests/fixed.rs, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("tmpleak-empty");
        t.write("a.sh", "x\n");
        assert_eq!(run(&t, &["a.sh", "testkit/src/lib.rs"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(TmpLeak),
    ]
}
