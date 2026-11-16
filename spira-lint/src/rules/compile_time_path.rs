//! `compile-time-path` — no `.rs` file fixes a path at compile time. Contract: DESIGN.md.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::{Entry, Finding, LintError, Rule, Tree};

#[derive(Default)]
pub struct CompileTimePath {
    scanned: AtomicUsize,
}

const NAME: &str = "compile-time-path";
const OWN_SOURCE: &str = "spira-lint/src/rules/compile_time_path.rs";

fn env_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"\b(?:option_)?env!\s*\(\s*"(CARGO_MANIFEST_DIR|OUT_DIR|CARGO_BIN_EXE_[^"]*)""#).expect("static regex")
    })
}

fn include_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\binclude(?:_str|_bytes)?!\s*\(").expect("static regex"))
}

fn literal_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#""(?:/home|/srv|/root|/Users)(?:/[^"\n]*)?(?:/spira/run/|/spira-releases/|/spira\b)[^"\n]*""#).expect("static regex"))
}

/// Byte ranges of the argument lists of `include!`-family calls; `code` has literals blanked
/// so a paren inside a string cannot unbalance the walk.
fn include_args(code: &[u8]) -> Vec<(usize, usize)> {
    include_re()
        .find_iter(code)
        .map(|m| {
            let mut depth = 1usize;
            let mut end = code.len();
            for (i, &b) in code.iter().enumerate().skip(m.end()) {
                match b {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            (m.end(), end)
        })
        .collect()
}

impl Rule for CompileTimePath {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("target/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        let mut scanned = 0;
        for e in files {
            if e.path == OWN_SOURCE {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            scanned += 1;
            let cl = rust::classify(src);
            let text = cl.without_comments();
            let skip = include_args(&cl.code_only());
            for c in env_re().captures_iter(&text) {
                let m = c.get(0).unwrap();
                if skip.iter().any(|(a, b)| *a <= m.start() && m.start() < *b) {
                    continue;
                }
                let message = if c[1].starts_with(b"CARGO_BIN_EXE_") {
                    "compile-time path: use testkit::bin_exe(name)".to_string()
                } else {
                    "compile-time path: read the tree at run time: spira_config::resolve::locate_home_for_process() for spira/…, checkout_root_for_process() for the checkout, testkit::manifest_dir() for this crate".to_string()
                };
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(cl.line_of(m.start())), message });
            }
            for m in literal_re().find_iter(&text) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(m.start())),
                    message: "a literal run-tree path: resolve it through spira-config".into(),
                });
            }
        }
        self.scanned.store(scanned, Ordering::Relaxed);
        out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        Some((self.scanned.load(Ordering::Relaxed), "rust files".to_string()))
    }

    fn hint(&self) -> &'static str {
        "A path baked in at compile time does not fail closed: when the builder's tree still \
exists, the test or binary silently reads that tree instead of the one it runs in. Resolve the \
path at run time through spira-config or testkit. include!/include_str!/include_bytes! \
arguments and CARGO_PKG_* are content, not paths, and are allowed."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(CompileTimePath::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(src: &str) -> Vec<String> {
        let src = &src.replace("@H", "/home").replace("@U", "/Users");
        let t = TempDir::new("ctp");
        t.write("a/src/lib.rs", src);
        let tree = Tree::from_paths(t.path(), ["a/src/lib.rs"], std::iter::empty::<&str>());
        CompileTimePath::default().check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn each_compile_time_path_shape_is_a_finding_at_its_line() {
        for src in [
            "fn f() {\n    let _ = Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"../systemd\");\n}\n",
            "fn f() {\n    let _ = concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src\");\n}\n",
            "fn f() {\n    let _ = env!(\"CARGO_BIN_EXE_x\");\n}\n",
            "fn f() {\n    let _ = option_env!(\"OUT_DIR\");\n}\n",
            "fn f() {\n    let _ = \"@H/someone/spira/run/x\";\n}\n",
            "fn f() {\n    let _ = \"/srv/spira-releases/x\";\n}\n",
            "fn f() {\n    let _ = \"/root/spira/run/x\";\n}\n",
            "fn f() {\n    let _ = \"@U/someone/spira/x\";\n}\n",
        ] {
            let got = run(src);
            assert_eq!(got.len(), 1, "{src}: {got:?}");
            assert!(got[0].starts_with("compile-time-path: a/src/lib.rs:2: "), "{got:?}");
        }
        assert!(run("fn f() { env!(\"CARGO_BIN_EXE_x\"); }\n")[0].contains("testkit::bin_exe(name)"));
        assert!(run("fn f() { \"/srv/spira/run/y\"; }\n")[0].contains("a literal run-tree path"));
    }

    #[test]
    fn content_and_non_paths_are_clean() {
        for src in [
            "const A: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/a.txt\"));\n",
            "include!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\n",
            "const V: &str = env!(\"CARGO_PKG_VERSION\");\n",
            "const H: &str = \"/home/x\";\n",
            "const R: &str = \"/r/spira-releases/x\";\n",
            "// env!(\"CARGO_MANIFEST_DIR\") and \"@H/someone/spira/run/x\"\n",
        ] {
            assert_eq!(run(src), Vec::<String>::new(), "{src}");
        }
    }

    #[test]
    fn an_env_read_after_an_include_call_is_still_a_finding() {
        let got = run("const A: &str = include_str!(\"a.txt\");\nconst B: &str = env!(\"CARGO_MANIFEST_DIR\");\n");
        assert_eq!(got.len(), 1);
        assert!(got[0].contains(":2:"), "{got:?}");
    }

    #[test]
    fn no_rust_files_is_a_refusal() {
        let t = TempDir::new("ctp-empty");
        t.write("a.sh", "x\n");
        let tree = Tree::from_paths(t.path(), ["a.sh"], std::iter::empty::<&str>());
        assert_eq!(CompileTimePath::default().check(&tree).map(|_| ()), Err(LintError::EmptyScope));
    }
}
