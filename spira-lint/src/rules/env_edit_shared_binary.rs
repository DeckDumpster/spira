//! `env-edit-shared-binary` — a test that edits the process environment through `testkit::env`
//! never shares a test binary with a test that runs without the environment lock.
//!
//! "Is this test code" is [`crate::rust_test`], shared with `env-set-var-leak`.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct EnvEditSharedBinary;

const NAME: &str = "env-edit-shared-binary";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static regex"))
}

struct Func {
    name: String,
    at: usize,
    body: (usize, usize),
}

fn body_end(code: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, &b) in code.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    code.len()
}

fn mentions(code: &[u8], needle: &[u8]) -> bool {
    code.windows(needle.len()).any(|w| w == needle)
}

fn called(body: &[u8], name: &str) -> bool {
    let pat = format!(r"(?:^|[^A-Za-z0-9_]){}\s*\(", regex::escape(name));
    Regex::new(&pat).expect("escaped name").is_match(body)
}

/// `<crate>/tests/<dir>/…` at any crate depth: a module of a multi-file target.
fn in_multi_file_target(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    parts.iter().skip(1).position(|p| *p == "tests").is_some_and(|i| parts.len() > i + 3)
}

impl Rule for EnvEditSharedBinary {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("testkit/") && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static FUNC: OnceLock<Regex> = OnceLock::new();
        static TEST_ATTR: OnceLock<Regex> = OnceLock::new();
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);
        let func = re(&FUNC, r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)[^{;]*\{");
        let test_attr = re(&TEST_ATTR, r"#\s*\[\s*test\s*\]");
        let mut out = Vec::new();
        for (e, _) in &shapes {
            if !e.path.contains("/tests/") || !whole.contains(&e.path) {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let code = cl.code_only();
            let Some(edit) = code.windows(b"testkit::env(".len()).position(|w| w == b"testkit::env(") else { continue };
            if in_multi_file_target(&e.path) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(edit)),
                    message: "testkit::env edit in a module of a multi-file test binary — move the edit to its own single-file [[test]] target".into(),
                });
                continue;
            }
            let funcs: Vec<Func> = func
                .captures_iter(&code)
                .map(|c| {
                    let m = c.get(0).expect("whole match");
                    let name = c.get(1).expect("name");
                    Func {
                        name: String::from_utf8_lossy(name.as_bytes()).into_owned(),
                        at: name.start(),
                        body: (m.end() - 1, body_end(&code, m.end() - 1)),
                    }
                })
                .collect();
            let mut covered: BTreeSet<usize> = (0..funcs.len())
                .filter(|&i| {
                    let b = &code[funcs[i].body.0..funcs[i].body.1];
                    mentions(b, b"testkit::env(") || mentions(b, b"testkit::env_read()")
                })
                .collect();
            loop {
                let more: Vec<usize> = (0..funcs.len())
                    .filter(|i| !covered.contains(i))
                    .filter(|&i| {
                        let b = &code[funcs[i].body.0..funcs[i].body.1];
                        covered.iter().any(|&j| called(b, &funcs[j].name))
                    })
                    .collect();
                if more.is_empty() {
                    break;
                }
                covered.extend(more);
            }
            for attr in test_attr.find_iter(&code) {
                let Some(i) = funcs.iter().position(|f| f.at >= attr.end()) else { continue };
                if !covered.contains(&i) {
                    out.push(Finding {
                        rule: NAME,
                        path: e.path.clone(),
                        line: Some(cl.line_of(funcs[i].at)),
                        message: format!("test `{}` runs without the environment lock in a binary whose tests edit the environment — begin it with `let _env = testkit::env_read();`", funcs[i].name),
                    });
                }
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "cargo runs a binary's tests on parallel threads of one process, and testkit::env holds \
ENV_LOCK only against tests that also take it: a test that takes neither testkit::env nor \
testkit::env_read reads a home, PATH or config another thread has swapped, and fails in a way no \
rerun reproduces. In a single-file target, give every other test `let _env = testkit::env_read();` \
as its first line. A test that reaches an editing helper is covered by it and takes no env_read of \
its own: ENV_LOCK is not reentrant for a reader, so env_read held while a helper takes env \
deadlocks. In a multi-file target (`<crate>/tests/<dir>/…`) no module may edit: move the edit to \
its own `[[test]]` target."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(EnvEditSharedBinary)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        EnvEditSharedBinary.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    const EDIT: &str = "#[test]\nfn edits() {\n    let _e = testkit::env(&[(\"A\", Some(\"1\"))]);\n}\n";

    #[test]
    fn an_edit_in_a_module_of_a_multi_file_binary_is_one_finding() {
        let t = TempDir::new("envshared-multi");
        t.write("a/tests/it/x.rs", EDIT);
        let got = run(&t, &["a/tests/it/x.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("a/tests/it/x.rs:3"), "{got:?}");
    }

    #[test]
    fn an_unlocked_test_beside_an_editing_one_is_named() {
        let t = TempDir::new("envshared-single");
        t.write("a/tests/t.rs", &format!("{EDIT}\n#[test]\nfn reads() {{\n    let _ = 1;\n}}\n"));
        let got = run(&t, &["a/tests/t.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("a/tests/t.rs:7") && got[0].contains("`reads`"), "{got:?}");
    }

    #[test]
    fn a_sibling_holding_env_read_or_reaching_an_editing_helper_passes() {
        let t = TempDir::new("envshared-covered");
        t.write(
            "a/tests/t.rs",
            &format!(
                "{EDIT}\n#[test]\nfn reads() {{\n    let _env = testkit::env_read();\n}}\n\nfn helper() {{\n    let _e = testkit::env(&[]);\n}}\n\nfn outer() {{\n    helper();\n}}\n\n#[test]\nfn through_helper() {{\n    outer();\n}}\n"
            ),
        );
        assert_eq!(run(&t, &["a/tests/t.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn an_edit_named_only_in_a_comment_or_a_string_passes() {
        let t = TempDir::new("envshared-prose");
        t.write("a/tests/it/x.rs", "// testkit::env(&[]) is how\nconst S: &str = \"testkit::env(\";\n#[test]\nfn t() {}\n");
        t.write("a/tests/y.rs", "/* testkit::env( */\n#[test]\nfn a() {}\n#[test]\nfn b() {}\n");
        assert_eq!(run(&t, &["a/tests/it/x.rs", "a/tests/y.rs"]).unwrap(), Vec::<String>::new());
        t.write("a/tests/y.rs", "fn f() { let _e = testkit::env(&[]); }\n#[test]\nfn a() {}\n#[test]\nfn b() {}\n");
        assert_eq!(run(&t, &["a/tests/y.rs"]).unwrap().len(), 2);
    }

    #[test]
    fn a_nested_crate_path_is_still_a_multi_file_target() {
        let t = TempDir::new("envshared-nested");
        t.write("spira/sim/tests/it/x.rs", EDIT);
        t.write("spira/sim/tests/one.rs", &format!("{EDIT}"));
        let got = run(&t, &["spira/sim/tests/it/x.rs", "spira/sim/tests/one.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("spira/sim/tests/it/x.rs"), "{got:?}");
    }
}
