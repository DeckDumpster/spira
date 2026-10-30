//! `e2e-test-marked` — a test that looks up a sibling binary in the target directory is
//! end-to-end and every test in its file is named `e2e_*`. Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct E2eTestMarked;

const NAME: &str = "e2e-test-marked";
const PREFIX: &str = "e2e_";
const MESSAGE: &str = "looks up a binary in the target directory, so its tests are end-to-end: name every #[test] in this file `e2e_*` (CARGO_BIN_EXE_* is the unit-tier way to reach the package's own binary)";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

fn applies(path: &str) -> bool {
    path.ends_with(".rs") && !path.split('/').any(|c| c == "target" || c == "fixtures")
}

fn lookup_line(src: &[u8], regions: &[(usize, usize)], whole: bool) -> Option<usize> {
    static EXE: OnceLock<Regex> = OnceLock::new();
    static DIR: OnceLock<Regex> = OnceLock::new();
    let cl = rust::classify(src);
    let code = cl.code_only();
    let text = cl.without_comments();
    let in_test = |at: usize| whole || regions.iter().any(|(a, b)| *a <= at && at < *b);
    let reaches_own = text.windows(14).any(|w| w == b"CARGO_BIN_EXE_");
    let exe = re(&EXE, r"current_exe\(\)[\s\S]{0,160}?\.parent\(\)");
    let dir = re(&DIR, r#"Command::new\([^;]*"[^"]*target/[a-z-]+/[A-Za-z]"#);
    let mut at: Vec<usize> = dir.find_iter(&text).map(|m| m.start()).filter(|a| code[*a] == b'C').collect();
    if !reaches_own {
        at.extend(exe.find_iter(&code).filter(|m| !code[m.start()..(m.end() + 120).min(code.len())].windows(9).any(|w| w == b"file_name")).map(|m| m.start()));
    }
    at.into_iter().filter(|a| in_test(*a)).min().map(|a| cl.line_of(a))
}

fn every_test_is_marked(src: &[u8]) -> bool {
    static TEST_FN: OnceLock<Regex> = OnceLock::new();
    let code = rust::classify(src).code_only();
    let tests = re(&TEST_FN, r"#\s*\[\s*test\s*\]\s*(?:#\s*\[[^\]]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)");
    tests.captures_iter(&code).all(|c| c[1].starts_with(PREFIX.as_bytes()))
}

impl Rule for E2eTestMarked {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        applies(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, crate::rust_test::Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);
        let mut out = Vec::new();
        for (e, s) in &shapes {
            let Some(src) = tree.content(e) else { continue };
            let Some(line) = lookup_line(src, &s.regions, whole.contains(&e.path)) else { continue };
            if !every_test_is_marked(src) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: MESSAGE.to_string() });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "The gate's unit phase excludes tests named `e2e_*` (nextest filter); rounds run them with \
the workspace built. A test that execs a sibling binary from target/ fails in the gate's unit \
build, which builds only the touched crates."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(E2eTestMarked)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        E2eTestMarked.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn an_unmarked_test_reading_spira_lc_from_target_is_flagged() {
        let t = TempDir::new("e2e-red");
        t.write("a/tests/x.rs", "#[test]\nfn reads() {\n    let out = std::process::Command::new(\"target/debug/spira-lc\").output();\n    drop(out);\n}\n");
        t.write("a/tests/y.rs", "fn bin() -> std::path::PathBuf {\n    std::env::current_exe().unwrap().parent().unwrap().join(\"spira-lc\")\n}\n#[test]\nfn reads() { bin(); }\n");
        assert_eq!(
            run(&t, &["a/tests/x.rs", "a/tests/y.rs"]).unwrap(),
            vec![format!("e2e-test-marked: a/tests/x.rs:3: {MESSAGE}"), format!("e2e-test-marked: a/tests/y.rs:2: {MESSAGE}")]
        );
    }

    #[test]
    fn marked_tests_own_binaries_and_production_code_pass() {
        let t = TempDir::new("e2e-green");
        t.write("a/tests/x.rs", "fn bin() -> std::path::PathBuf {\n    std::env::current_exe().unwrap().parent().unwrap().join(\"spira-lc\")\n}\n#[test]\nfn e2e_reads() { bin(); }\n");
        t.write("a/tests/own.rs", "#[test]\nfn runs_itself() { let _ = env!(\"CARGO_BIN_EXE_a\"); }\n");
        t.write("a/src/lib.rs", "pub fn f() { let _ = std::env::current_exe().unwrap().parent(); }\n");
        assert_eq!(run(&t, &["a/tests/x.rs", "a/tests/own.rs", "a/src/lib.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn an_unrelated_crate_name_literal_does_not_exempt_a_sibling_lookup() {
        let t = TempDir::new("e2e-literal");
        t.write("a/tests/x.rs", "#[test]\nfn reads() {\n    let _ = \"a\";\n    std::env::current_exe().unwrap().parent().unwrap().join(\"spira-lc\");\n}\n");
        assert_eq!(run(&t, &["a/tests/x.rs"]).unwrap(), vec![format!("e2e-test-marked: a/tests/x.rs:4: {MESSAGE}")]);
    }

    #[test]
    fn no_candidate_files_is_a_refusal() {
        let t = TempDir::new("e2e-empty");
        t.write("a.txt", "x\n");
        assert_eq!(run(&t, &["a.txt"]), Err(LintError::EmptyScope));
    }
}
