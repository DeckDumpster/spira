//! `env-set-var-leak` — test code edits the process environment through `testkit::env`,
//! never a bare `env::set_var` / `env::remove_var`: tests share one process, so an edit
//! another thread reads mid-flight makes a correct test fail once in a hundred runs.
//!
//! "Is this test code" is [`crate::rust_test`], shared with `tmp-leak` and
//! `script-callers` so no rule defines it a different way.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct EnvSetVarLeak;

const NAME: &str = "env-set-var-leak";
const ALLOW_FILE: &str = "spira-lint/env-set-var-leak-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

impl Rule for EnvSetVarLeak {
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
        let call = re(&CALL, r"(?:^|[^A-Za-z0-9_])(set_var|remove_var)\s*\(");
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
                    message: "set_var/remove_var in test code — take testkit::env(&[(key, Some(value) | None)]), which serializes the edit against every other test thread and restores on drop".into(),
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
        "cargo runs a binary's tests on parallel threads of one process, and the environment is \
process-global: a test that sets SPIRA_BD (or PATH) while another reads it makes the second fail \
intermittently, in a way no rerun reproduces. testkit::env(&[(\"K\", Some(\"v\")), (\"U\", None)]) \
holds one lock for the guard's lifetime and restores every key on drop, even on panic; a test \
that only reads the environment takes testkit::env_read(). Files not yet converted are listed in \
spira-lint/env-set-var-leak-allow, which only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        EnvSetVarLeak.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    const SET: &str = concat!("set", "_var");
    const REMOVE: &str = concat!("remove", "_var");

    fn leaking() -> String {
        format!("fn t() {{\n    std::env::{SET}(\"A\", \"1\");\n    env::{REMOVE}(\"A\");\n}}\n")
    }

    #[test]
    fn a_planted_env_edit_in_test_code_is_caught_wherever_test_code_lives() {
        let t = TempDir::new("envleak-fail");
        t.write(ALLOW_FILE, "# shrink-only\n");
        t.write("a/src/lib.rs", &format!("pub fn f() {{}}\n\n#[cfg(test)]\nmod tests {{\n    {}}}\n", leaking()));
        t.write("b/tests/it.rs", &leaking());
        t.write("c/src/main.rs", "fn main() {}\n#[cfg(test)]\nmod testutil;\n");
        t.write("c/src/testutil.rs", &leaking());
        let got = run(&t, &["a/src/lib.rs", "b/tests/it.rs", "c/src/main.rs", "c/src/testutil.rs"]).unwrap();
        let lines: Vec<(String, String)> = got
            .iter()
            .map(|g| {
                let mut p = g.splitn(3, ": ");
                p.next();
                (p.next().unwrap().to_string(), p.next().unwrap().to_string())
            })
            .collect();
        let paths: Vec<&str> = lines.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec!["a/src/lib.rs:6", "a/src/lib.rs:7", "b/tests/it.rs:2", "b/tests/it.rs:3", "c/src/testutil.rs:2", "c/src/testutil.rs:3"]
        );
        assert!(lines.iter().all(|(_, m)| m.starts_with("set_var/remove_var in test code")), "{lines:?}");
    }

    #[test]
    fn production_code_testkit_comments_and_strings_pass() {
        let t = TempDir::new("envleak-pass");
        t.write(ALLOW_FILE, "");
        t.write(
            "a/src/lib.rs",
            &format!(
                "pub fn install() {{\n    std::env::{SET}(\"A\", \"1\");\n}}\n#[cfg(test)]\nmod tests {{\n    // never {SET}(\"A\", \"1\") here\n    const S: &str = \"{REMOVE}(\";\n    #[test]\n    fn t() {{\n        let _e = testkit::env(&[(\"A\", Some(\"1\"))]);\n    }}\n}}\n"
            ),
        );
        t.write("testkit/src/lib.rs", &leaking());
        assert_eq!(run(&t, &["a/src/lib.rs", "testkit/src/lib.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("envleak-allow");
        t.write("b/tests/old.rs", &leaking());
        t.write("b/tests/fixed.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "# header\nb/tests/old.rs\nb/tests/fixed.rs\n");
        assert_eq!(
            run(&t, &["b/tests/old.rs", "b/tests/fixed.rs"]).unwrap(),
            vec!["env-set-var-leak: spira-lint/env-set-var-leak-allow:3: lists b/tests/fixed.rs, which no longer needs an exception — remove the line (the list only shrinks)"]
        );
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("envleak-empty");
        t.write("a.sh", "x\n");
        assert_eq!(run(&t, &["a.sh", "testkit/src/lib.rs"]), Err(LintError::EmptyScope));
    }
}
