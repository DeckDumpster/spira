//! `config-registry-runtime` — the config key registry is read at run time, never compiled in:
//! no `build.rs` names it and no `include_str!`/`include_bytes!` embeds it. Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::SyncCell as Cell;
use crate::{Entry, Finding, LintError, Rule, Tree};

#[derive(Default)]
pub struct ConfigRegistryRuntime {
    checked: Cell<Option<usize>>,
}

const NAME: &str = "config-registry-runtime";
const OWN_SOURCE: &str = "spira-lint/src/rules/config_registry_runtime.rs";
const REGISTRY_FILES: [&str; 5] = ["conf.d", "conf.toml.d", "conf.key-history", "complete.toml", "config-delta.toml"];

fn embed_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"include_(?:str|bytes)!\s*\(").expect("static regex"))
}

/// The text between the parenthesis that ends at `open` and its match.
fn balanced(code: &[u8], open: usize) -> &[u8] {
    let mut depth = 1usize;
    for (i, b) in code[open..].iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &code[open..open + i];
                }
            }
            _ => {}
        }
    }
    &code[open..]
}

fn rerun_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"rerun-if-changed").expect("static regex"))
}

fn names_registry(text: &[u8]) -> Option<&'static str> {
    let s = String::from_utf8_lossy(text);
    REGISTRY_FILES.iter().copied().find(|n| s.contains(n))
}

/// `(byte offset, what)` of each compiled-in registry file in one Rust source.
fn scan(src: &[u8], is_build_script: bool) -> Vec<(usize, String)> {
    let code = rust::classify(src).without_comments();
    let mut hits = Vec::new();
    for m in embed_re().find_iter(&code) {
        if let Some(name) = names_registry(balanced(&code, m.end())) {
            hits.push((m.start(), format!("embeds {name} into the binary")));
        }
    }
    if is_build_script && rerun_re().is_match(&code) {
        let at = rerun_re().find(&code).unwrap().start();
        if let Some(name) = names_registry(&code) {
            hits.push((at, format!("a build script reruns on {name}")));
        }
    }
    hits
}

impl Rule for ConfigRegistryRuntime {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !e.path.starts_with("target/") && e.path != OWN_SOURCE
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(src) = tree.content(e) else { continue };
            let is_build = e.path == "build.rs" || e.path.ends_with("/build.rs");
            let cl = rust::classify(src);
            for (at, what) in scan(src, is_build) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(at)),
                    message: format!("{what}: the registry is read at run time (spira_config::registry), so adding a key must not rebuild a crate"),
                });
            }
        }
        self.checked.set(Some(files.len()));
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "rust sources".into()))
    }

    fn hint(&self) -> &'static str {
        "Read the registry with spira_config::registry::for_process() and a fixture with fs::read_to_string at run time; \
a compiled-in copy changes every dependent crate's fingerprint and the gate's tool key whenever a key is added."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(ConfigRegistryRuntime::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ConfigRegistryRuntime::default().check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn a_planted_include_str_of_the_registry_is_found() {
        let t = TempDir::new("crr-include");
        t.write("a/src/lib.rs", "const K: &str = include_str!(\"../../spira/conf.d/SPIRA_RUN\");\n");
        t.write("b/src/lib.rs", "const K: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../spira/testlib/complete.toml\"));\n");
        let got = run(&t, &["a/src/lib.rs", "b/src/lib.rs"]);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].contains("a/src/lib.rs:1") && got[0].contains("conf.d"), "{got:?}");
        assert!(got[1].contains("complete.toml"), "{got:?}");
    }

    #[test]
    fn a_planted_rerun_if_changed_on_the_registry_is_found() {
        let t = TempDir::new("crr-rerun");
        t.write("a/build.rs", "fn main() {\n    println!(\"cargo:rerun-if-changed=../spira/conf.d\");\n}\n");
        let got = run(&t, &["a/build.rs", "a/src/lib.rs"]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("a/build.rs:2") && got[0].contains("conf.d"), "{got:?}");
    }

    #[test]
    fn unrelated_embeds_a_comment_and_runtime_reads_are_clean() {
        let t = TempDir::new("crr-clean");
        t.write("a/build.rs", "fn main() {\n    println!(\"cargo:rerun-if-changed=build.rs\");\n}\n");
        t.write(
            "a/src/lib.rs",
            "// include_str!(\"../spira/conf.d/SPIRA_RUN\")\nconst T: &str = include_str!(\"template.txt\");\n\
             fn f() { let _ = std::fs::read_to_string(\"../spira/testlib/complete.toml\"); }\n",
        );
        assert!(run(&t, &["a/build.rs", "a/src/lib.rs"]).is_empty());
    }
}
