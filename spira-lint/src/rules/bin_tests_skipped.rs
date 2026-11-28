//! `bin-tests-skipped` — a `[[bin]]` declared `test = false` holds no test: cargo test and the
//! round's unit tier leave such a target out, so a test in it never runs and nothing says so.

use std::collections::BTreeSet;

use crate::rust_test::{mod_file, shape, test_sites};
use crate::{Finding, LintError, Rule, Tree};

pub struct BinTestsSkipped;

const NAME: &str = "bin-tests-skipped";
const ROOT: &str = "Cargo.toml";

const HINT: &str = "remove `test = false`: this bin's source holds tests, and cargo test / the \
round's unit tier skip them while it is set (the flag belongs only on a bin with no test)";

/// The first test, as (fn name, file), in the crate rooted at `root` and the files it loads.
/// Test-gated modules are followed too: their tests are skipped like any other.
fn first_test(tree: &Tree, root: &str) -> Result<Option<(String, String)>, LintError> {
    if tree.entry(root).is_none() {
        return Err(LintError::Refused(format!("{root}: bin source is not in the walk")));
    }
    let mut seen = BTreeSet::new();
    let mut todo = vec![(root.to_string(), true)];
    while let Some((file, is_root)) = todo.pop() {
        if !seen.insert(file.clone()) {
            continue;
        }
        let Some(text) = tree.text_of(&file) else { continue };
        if let Some(site) = test_sites(text.as_bytes()).into_iter().next() {
            return Ok(Some((site.fn_name, file)));
        }
        // A crate root resolves `mod x;` beside itself, whatever it is called.
        let decl = match (is_root, file.rsplit_once('/')) {
            (false, _) => file.clone(),
            (true, Some((dir, _))) => format!("{dir}/main.rs"),
            (true, None) => "main.rs".to_string(),
        };
        for (name, path) in shape(text.as_bytes()).mods() {
            for f in mod_file(&decl, name, path) {
                if tree.entry(&f).is_some() {
                    todo.push((f, false));
                }
            }
        }
    }
    Ok(None)
}

/// The 1-based line of the `test` key in the `nth` `[[bin]]` table.
fn test_key_line(manifest: &str, nth: usize) -> Option<usize> {
    let mut bins = 0;
    let mut inside = false;
    for (i, line) in manifest.lines().enumerate() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line.starts_with("[[bin]]") && {
                bins += 1;
                bins == nth + 1
            };
        } else if inside && line.split_once('=').is_some_and(|(k, _)| k.trim() == "test") {
            return Some(i + 1);
        }
    }
    None
}

fn parse(path: &str, text: &str) -> Result<toml::Table, LintError> {
    text.parse().map_err(|e| LintError::Refused(format!("{path}: {e}")))
}

impl Rule for BinTestsSkipped {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &crate::Entry) -> bool {
        e.path == ROOT
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        crate::scope(tree, self)?;
        let root = parse(ROOT, &tree.read_text(ROOT))?;
        let members: Vec<&str> = root
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
            .map(|m| m.iter().filter_map(|m| m.as_str()).collect())
            .unwrap_or_default();
        if members.is_empty() {
            return Err(LintError::Refused(format!("{ROOT} has no [workspace] members — nothing to judge")));
        }

        let mut out = Vec::new();
        for dir in members {
            let manifest_path = format!("{dir}/Cargo.toml");
            let text = tree.read_text(&manifest_path);
            let manifest = parse(&manifest_path, &text)?;
            let bins = manifest.get("bin").and_then(|b| b.as_array()).into_iter().flatten();
            for (nth, bin) in bins.enumerate() {
                if bin.get("test").and_then(|t| t.as_bool()) != Some(false) {
                    continue;
                }
                let source = bin.get("path").and_then(|p| p.as_str()).unwrap_or("src/main.rs");
                let name = bin.get("name").and_then(|n| n.as_str()).unwrap_or(dir);
                if let Some((test_fn, file)) = first_test(tree, &format!("{dir}/{source}"))? {
                    out.push(Finding {
                        rule: NAME,
                        path: manifest_path.clone(),
                        line: test_key_line(&text, nth),
                        message: format!("bin {name} is `test = false` but holds tests that never run, first {test_fn} in {file}"),
                    });
                }
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        HINT
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(BinTestsSkipped)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const MAIN: &str = "demo/src/main.rs";
    const SKIPPED: &str = "[[bin]]\nname = \"other\"\npath = \"src/other.rs\"\n\n[[bin]]\nname = \"demo\"\ntest = false\n";
    const A_TEST: &str = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn unrun() {}\n}\n";

    fn run(tag: &str, manifest: &str, files: &[(&str, &str)]) -> Result<Vec<String>, LintError> {
        let t = TempDir::new(tag);
        let base = [(ROOT, "[workspace]\nmembers = [\"demo\"]\n"), ("demo/Cargo.toml", manifest), ("demo/src/other.rs", "fn main() {}\n")];
        let all: Vec<(&str, &str)> = base.into_iter().chain(files.iter().copied()).collect();
        for (path, text) in &all {
            t.write(path, text);
        }
        let tree = Tree::from_paths(t.path(), all.iter().map(|(p, _)| *p), std::iter::empty::<&str>());
        BinTestsSkipped.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test_id(t7dh70v)]
    #[test]
    fn a_skipped_bin_with_a_test_in_its_root_is_one_finding_at_the_test_key() {
        let got = run("binskip-root", SKIPPED, &[(MAIN, A_TEST)]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("bin-tests-skipped: demo/Cargo.toml:7:"), "{got:?}");
        assert!(got[0].contains("bin demo ") && got[0].contains("unrun in demo/src/main.rs"), "{got:?}");
    }

    #[test_id(t0seadb)]
    #[test]
    fn a_test_in_a_module_the_skipped_bin_loads_is_found() {
        let files = [(MAIN, "mod cli;\nfn main() {}\n"), ("demo/src/cli.rs", A_TEST)];
        let got = run("binskip-mod", SKIPPED, &files).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("unrun in demo/src/cli.rs"), "{got:?}");
    }

    #[test_id(thulgwj)]
    #[test]
    fn a_crate_root_under_src_bin_loads_its_siblings_and_test_gated_modules() {
        let manifest = "[[bin]]\nname = \"w\"\npath = \"src/bin/w.rs\"\ntest = false\n";
        let files = [
            ("demo/src/bin/w.rs", "mod a;\nfn main() {}\n"),
            ("demo/src/bin/a.rs", "#[cfg(test)]\n#[path = \"a_tests.rs\"]\nmod tests;\n"),
            ("demo/src/bin/a_tests.rs", "#[test]\nfn unrun() {}\n"),
        ];
        let got = run("binskip-srcbin", manifest, &files).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("unrun in demo/src/bin/a_tests.rs"), "{got:?}");
    }

    #[test_id(ti646j4)]
    #[test]
    fn a_skipped_bin_with_no_test_is_clean() {
        let got = run("binskip-none", SKIPPED, &[(MAIN, "// #[test]\nfn main() { let _ = \"#[test]\"; }\n")]).unwrap();
        assert!(got.is_empty(), "{got:?}");
    }

    #[test_id(t1mdpfq)]
    #[test]
    fn a_bin_that_runs_its_tests_is_clean() {
        let got = run("binskip-on", "[[bin]]\nname = \"demo\"\n", &[(MAIN, A_TEST)]).unwrap();
        assert!(got.is_empty(), "{got:?}");
    }

    #[test_id(tx699sn)]
    #[test]
    fn a_skipped_bin_whose_source_is_not_in_the_walk_is_refused() {
        let got = run("binskip-gone", SKIPPED, &[]);
        assert!(matches!(&got, Err(LintError::Refused(m)) if m.contains(MAIN)), "{got:?}");
    }

    #[test_id(tlnujuk)]
    #[test]
    fn a_root_with_no_workspace_members_is_refused() {
        let t = TempDir::new("binskip-empty");
        t.write(ROOT, "[workspace]\nmembers = []\n");
        let tree = Tree::from_paths(t.path(), [ROOT], std::iter::empty::<&str>());
        assert!(matches!(BinTestsSkipped.check(&tree), Err(LintError::Refused(_))));
    }
}
