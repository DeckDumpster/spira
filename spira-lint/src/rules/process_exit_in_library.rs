//! `process-exit-in-library` — `process::exit` is called only from `fn main`. Anywhere else it
//! is a library function deciding the fate of whatever process hosts it: a failing unit test
//! that reaches it kills the whole test binary instead of failing one test, and a caller that
//! could have handled the error never sees it. The function returns a `Result` and `main`
//! chooses the exit code.
//!
//! "Is this test code" is [`crate::rust_test`], shared with the other Rust rules.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ProcessExitInLibrary;

const NAME: &str = "process-exit-in-library";
const ALLOW_FILE: &str = "spira-lint/process-exit-in-library-allow";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

/// `(name, body start, body end)` of every `fn` with a body, in `code` (comments and
/// literals already blanked).
fn fn_bodies(code: &[u8]) -> Vec<(String, usize, usize)> {
    static FN: OnceLock<Regex> = OnceLock::new();
    let mut out = Vec::new();
    for c in re(&FN, r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)").captures_iter(code) {
        let name = String::from_utf8_lossy(&c[1]).into_owned();
        let mut i = c.get(0).map_or(0, |m| m.end());
        while i < code.len() && code[i] != b'{' && code[i] != b';' {
            i += 1;
        }
        if i >= code.len() || code[i] == b';' {
            continue;
        }
        let start = i;
        let mut depth = 0usize;
        while i < code.len() {
            match code[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        out.push((name, start, i));
    }
    out
}

impl Rule for ProcessExitInLibrary {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static CALL: OnceLock<Regex> = OnceLock::new();
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
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
        let call = re(&CALL, r"process\s*::\s*exit\s*\(");
        let mut out = Vec::new();
        let mut offenders = BTreeSet::new();
        for (e, s) in &shapes {
            let Some(src) = tree.content(e) else { continue };
            if whole.contains(&e.path) {
                continue;
            }
            let cl = rust::classify(src);
            let code = cl.code_only();
            let bodies = fn_bodies(&code);
            for m in call.find_iter(&code) {
                let at = m.start();
                if s.regions.iter().any(|(a, b)| *a <= at && at < *b) {
                    continue;
                }
                let innermost = bodies.iter().filter(|(_, a, b)| *a <= at && at <= *b).min_by_key(|(_, a, b)| b - a);
                if innermost.is_some_and(|(n, _, _)| n == "main") {
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
                    message: "process::exit outside fn main — return a Result (or an exit code) and let main exit".into(),
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
        "process::exit in a function other than main ends the whole process: a unit test that \
reaches it kills the test binary and hides every other result, and no caller can handle the \
failure. Return Result<_, String> (or the exit code) up to main, which prints and exits. Files \
not yet converted are listed in spira-lint/process-exit-in-library-allow, which only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ProcessExitInLibrary.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    const EXIT: &str = concat!("process", "::exit(1)");

    #[test]
    fn exit_outside_main_is_caught_and_main_comments_strings_and_tests_pass() {
        let t = TempDir::new("pexit-fail");
        t.write(ALLOW_FILE, "# shrink-only\n");
        t.write(
            "a/src/lib.rs",
            &format!("pub fn f() {{\n    std::{EXIT};\n}}\n// {EXIT}\nconst S: &str = \"{EXIT}\";\n#[cfg(test)]\nmod tests {{\n    fn t() {{ {EXIT}; }}\n}}\n"),
        );
        t.write("a/src/main.rs", &format!("fn helper() {{\n    {EXIT};\n}}\nfn main() {{\n    let c = || {EXIT};\n    {EXIT};\n}}\n"));
        let got = run(&t, &["a/src/lib.rs", "a/src/main.rs"]).unwrap();
        let msg = "process::exit outside fn main — return a Result (or an exit code) and let main exit";
        assert_eq!(got, vec![format!("{NAME}: a/src/lib.rs:2: {msg}"), format!("{NAME}: a/src/main.rs:2: {msg}")]);
    }

    #[test]
    fn a_listed_exception_passes_and_a_stale_one_is_refused() {
        let t = TempDir::new("pexit-allow");
        t.write("b/src/old.rs", &format!("fn f() {{ {EXIT}; }}\n"));
        t.write("b/src/fixed.rs", "fn f() {}\n");
        t.write(ALLOW_FILE, "# header\nb/src/old.rs\nb/src/fixed.rs\n");
        assert_eq!(
            run(&t, &["b/src/old.rs", "b/src/fixed.rs"]).unwrap(),
            vec![format!("{NAME}: {ALLOW_FILE}:3: lists b/src/fixed.rs, which no longer needs an exception — remove the line (the list only shrinks)")]
        );
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("pexit-empty");
        t.write("a.sh", "x\n");
        assert_eq!(run(&t, &["a.sh"]), Err(LintError::EmptyScope));
    }
}
