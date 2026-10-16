//! `one-runtime` — a repository holds source and test files for its declared language
//! runtime only. Contract: DESIGN.md.
//!
//! The repository declares `runtime = "<name>"` (and optionally `build_tools = [..]` for a
//! frontend's toolchain) in `.spira/testenv.toml`. A repository that declares no runtime is
//! not yet under the rule. There is no allowlist and no exception list: a second-runtime file
//! is ported or deleted.

use std::collections::BTreeSet;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct OneRuntime;

const NAME: &str = "one-runtime";
const DECL: &str = ".spira/testenv.toml";

/// Shell glue is no runtime and is not listed.
const EXTENSIONS: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("py", "python"),
    ("pyw", "python"),
    ("js", "node"),
    ("mjs", "node"),
    ("cjs", "node"),
    ("jsx", "node"),
    ("ts", "node"),
    ("tsx", "node"),
    ("java", "java"),
    ("kt", "java"),
    ("kts", "java"),
    ("scala", "java"),
    ("go", "go"),
    ("rb", "ruby"),
    ("pl", "perl"),
    ("php", "php"),
];

const SHEBANGS: &[(&str, &str)] =
    &[("python", "python"), ("node", "node"), ("ruby", "ruby"), ("perl", "perl"), ("php", "php")];

/// The runtime a file's name or shebang puts it in; `None` for shell, data and prose.
pub fn runtime_of(path: &str, content: &[u8]) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            return EXTENSIONS.iter().find(|(e, _)| *e == ext).map(|(_, r)| *r);
        }
        _ => {}
    }
    let first = content.split(|b| *b == b'\n').next().unwrap_or(&[]);
    let first = String::from_utf8_lossy(first);
    let first = first.strip_prefix("#!")?;
    SHEBANGS.iter().find(|(k, _)| first.contains(k)).map(|(_, r)| *r)
}

struct Declared {
    runtime: String,
    allowed: BTreeSet<String>,
}

fn declared(tree: &Tree) -> Result<Option<Declared>, LintError> {
    let Some(e) = tree.entries.iter().find(|e| e.path == DECL) else { return Ok(None) };
    let Some(content) = tree.content(e) else { return Ok(None) };
    let bad = |reason: String| LintError::BadAllow { file: DECL.into(), line: 0, reason };
    let v: toml::Value =
        String::from_utf8_lossy(content).parse().map_err(|e: toml::de::Error| bad(e.to_string()))?;
    let Some(rt) = v.get("runtime") else { return Ok(None) };
    let runtime = rt
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("runtime must be a non-empty string".into()))?;
    let mut allowed = BTreeSet::from([runtime.clone()]);
    if let Some(b) = v.get("build_tools") {
        let arr = b.as_array().ok_or_else(|| bad("build_tools must be an array of strings".into()))?;
        for t in arr {
            let t = t.as_str().ok_or_else(|| bad("build_tools must be an array of strings".into()))?;
            allowed.insert(t.trim().to_ascii_lowercase());
        }
    }
    Ok(Some(Declared { runtime, allowed }))
}

impl Rule for OneRuntime {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path != DECL
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let Some(d) = declared(tree)? else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for e in tree.entries.iter().filter(|e| e.path != DECL) {
            let Some(content) = tree.content(e) else { continue };
            let Some(found) = runtime_of(&e.path, content) else { continue };
            if !d.allowed.contains(found) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: None,
                    message: format!("{found} file in a repository declaring runtime {:?}", d.runtime),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A repository uses one language runtime (law-one-language-runtime-per-repo): port the \
file to the declared runtime or delete it. There is no allowlist. The only override is the \
declaration itself: `runtime` in .spira/testenv.toml, or `build_tools` for a frontend \
toolchain that is the build tool of the same service."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir) -> Vec<String> {
        let tree = Tree::from_git(t.path()).unwrap();
        OneRuntime.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    const DECL_RUST: &str = "runtime = \"rust\"\nimage = \"x\"\n[tier.a]\nargv = [\"true\"]\n";

    #[test]
    fn second_runtime_file_is_seen_red_and_withdrawn() {
        let t = TempDir::new("onert");
        t.git_init();
        t.write(".spira/testenv.toml", DECL_RUST);
        t.write("src/main.rs", "fn main() {}\n");
        t.write("ci/glue.sh", "#!/usr/bin/env bash\ntrue\n");
        t.git(&["add", "."]);
        assert!(run(&t).is_empty());

        t.write("tests/x.py", "print(1)\n");
        t.write("tools/run", "#!/usr/bin/env python3\nprint(1)\n");
        t.git(&["add", "."]);
        let got = run(&t);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().any(|l| l.contains("tests/x.py") && l.contains("python")));
        assert!(got.iter().any(|l| l.contains("tools/run")));

        t.remove("tests/x.py");
        t.remove("tools/run");
        t.git(&["add", "-A"]);
        assert!(run(&t).is_empty());
    }

    #[test]
    fn no_allowlist_every_unlisted_path_fails() {
        let t = TempDir::new("onert");
        t.git_init();
        t.write(".spira/testenv.toml", DECL_RUST);
        t.write("lake/job.py", "print(1)\n");
        t.git(&["add", "."]);
        assert_eq!(run(&t).len(), 1);
    }

    #[test]
    fn build_tools_admit_a_frontend_toolchain_only() {
        let t = TempDir::new("onert");
        t.git_init();
        t.write(".spira/testenv.toml", &format!("build_tools = [\"node\"]\n{DECL_RUST}"));
        t.write("web/app.ts", "export {}\n");
        t.write("web/gen.py", "print(1)\n");
        t.git(&["add", "."]);
        let got = run(&t);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("web/gen.py"));
    }

    #[test]
    fn undeclared_runtime_is_not_under_the_rule_and_bad_decl_refuses() {
        let t = TempDir::new("onert");
        t.git_init();
        t.write("a.py", "print(1)\n");
        t.write("b.rs", "fn main() {}\n");
        t.git(&["add", "."]);
        assert!(run(&t).is_empty());

        t.write(".spira/testenv.toml", "runtime = 3\n");
        t.git(&["add", "."]);
        let tree = Tree::from_git(t.path()).unwrap();
        assert!(matches!(OneRuntime.check(&tree), Err(LintError::BadAllow { .. })));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(OneRuntime)]
}
