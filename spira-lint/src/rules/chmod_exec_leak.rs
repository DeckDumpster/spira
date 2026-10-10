//! `chmod-exec-leak` — test code makes an executable test fixture through
//! `testkit::write_exe`, never a bare `fs::write` + `set_permissions(..., 0o7xx)` it may
//! run while this process still has a write-fd open on it (sp-os3of).
//!
//! "Is this test code" is [`crate::rust_test`], shared with `tmp-leak` and
//! `script-callers` so no rule defines it a different way.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ChmodExecLeak;

const NAME: &str = "chmod-exec-leak";
const ALLOW_FILE: &str = "spira-lint/chmod-exec-leak-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

impl Rule for ChmodExecLeak {
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
        // `Permissions::from_mode(0o7xx)` — the owner-executable octal literal that marks
        // "make this freshly-written file executable", never the `0o5xx`/`0o4xx` modes a
        // test uses to simulate a read-only or non-executable file (an unrelated concern,
        // never this call's job).
        let call = re(&CALL, r"(?:^|[^A-Za-z0-9_])(from_mode)\s*\(\s*0o7[0-7]{2}\s*\)");
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
                    message: "Permissions::from_mode(0o7xx) in test code — take testkit::write_exe(path, body), which writes and chmods through a child process so no concurrent fork can inherit a write-fd on the file and leave it ETXTBSY".into(),
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
        "fs::write(path, body) opens path for writing, and a thread elsewhere in the same test \
binary that forks WHILE that write-fd is open inherits a duplicate of it — the kernel then \
refuses to exec the file (ETXTBSY) until that unrelated child closes it or execs, however long \
that takes (sp-os3of: the third occurrence, after sp-xtdqi-3's spira-config and doctor). \
testkit::write_exe writes through a dedicated child process instead, so this process never \
itself holds a write-fd on the fixture, and chmods it 0o755 in the same call. A chmod alone, on \
a file already written some other way, is not this rule's concern — list it in \
spira-lint/chmod-exec-leak-allow, which only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ChmodExecLeak.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    // The fixtures spell the call through concat! so this file carries no call in code.
    const FROM_MODE: &str = concat!("from_", "mode");

    fn leaking_helper() -> String {
        format!(
            "fn exe(p: &std::path::Path) {{\n    std::fs::write(p, \"x\").unwrap();\n    std::fs::set_permissions(p, std::fs::Permissions::{FROM_MODE}(0o755)).unwrap();\n}}\n"
        )
    }

    /// The failing fixture: the leak in each place test code lives.
    #[test]
    fn a_planted_chmod_exec_in_test_code_is_caught_wherever_test_code_lives() {
        let t = TempDir::new("chmodleak-fail");
        t.write(ALLOW_FILE, "# shrink-only\n");
        // 1. inside #[cfg(test)] mod tests { … }
        t.write(
            "a/src/lib.rs",
            &format!("pub fn f() {{}}\n\n#[cfg(test)]\nmod tests {{\n    {}}}\n", leaking_helper()),
        );
        // 2. an integration test
        t.write("b/tests/it.rs", &leaking_helper());
        // 3. an out-of-line test module, declared under #[cfg(test)]
        t.write("c/src/main.rs", "fn main() {}\n#[cfg(test)]\nmod testutil;\n");
        t.write("c/src/testutil.rs", &leaking_helper());
        // 4. a file that is test code by its own #![cfg(test)]
        t.write("d/src/helpers.rs", &format!("#![cfg(test)]\n{}", leaking_helper()));
        let got = run(&t, &["a/src/lib.rs", "b/tests/it.rs", "c/src/main.rs", "c/src/testutil.rs", "d/src/helpers.rs"]).unwrap();
        let msg = "Permissions::from_mode(0o7xx) in test code — take testkit::write_exe(path, body), which writes and chmods through a child process so no concurrent fork can inherit a write-fd on the file and leave it ETXTBSY";
        assert_eq!(
            got,
            vec![
                format!("chmod-exec-leak: a/src/lib.rs:7: {msg}"),
                format!("chmod-exec-leak: b/tests/it.rs:3: {msg}"),
                format!("chmod-exec-leak: c/src/testutil.rs:3: {msg}"),
                format!("chmod-exec-leak: d/src/helpers.rs:4: {msg}"),
            ]
        );
    }

    /// The passing fixture: testkit::write_exe in test code, production code even with the
    /// literal mode, a non-executable mode (0o555/0o644), a variable mode (not a literal),
    /// and the pattern sitting in a comment or a string.
    #[test]
    fn write_exe_and_non_matching_modes_pass() {
        let t = TempDir::new("chmodleak-pass");
        t.write(ALLOW_FILE, "");
        t.write(
            "a/src/lib.rs",
            &format!(
                "pub fn install(p: &std::path::Path) {{\n    std::fs::write(p, \"x\").unwrap();\n    std::fs::set_permissions(p, std::fs::Permissions::{FROM_MODE}(0o755)).unwrap();\n}}\n\
#[cfg(test)]\nmod tests {{\n    // never a bare {FROM_MODE}(0o755) in a test\n    const S: &str = \"{FROM_MODE}(0o755)\";\n    #[test]\n    fn t() {{\n        let d = testkit::TempDir::new(\"a\");\n        testkit::write_exe(&d.join(\"x\"), \"#!/bin/sh\\n\");\n        let mode = 0o755;\n        let _ = std::fs::Permissions::{FROM_MODE}(mode);\n        std::fs::set_permissions(&d.join(\"x\"), std::fs::Permissions::{FROM_MODE}(0o555)).unwrap();\n    }}\n}}\n"
            ),
        );
        t.write("testkit/src/lib.rs", &leaking_helper());
        assert_eq!(run(&t, &["a/src/lib.rs", "testkit/src/lib.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("chmodleak-allow");
        t.write("b/tests/old.rs", &leaking_helper());
        t.write("b/tests/fixed.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "# header\nb/tests/old.rs\nb/tests/fixed.rs\n");
        assert_eq!(
            run(&t, &["b/tests/old.rs", "b/tests/fixed.rs"]).unwrap(),
            vec!["chmod-exec-leak: spira-lint/chmod-exec-leak-allow:3: lists b/tests/fixed.rs, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("chmodleak-empty");
        t.write("a.sh", "x\n");
        assert_eq!(run(&t, &["a.sh", "testkit/src/lib.rs"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ChmodExecLeak),
    ]
}
