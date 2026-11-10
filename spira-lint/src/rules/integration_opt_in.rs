//! `integration-opt-in` — every `tests/` target is gated behind the `integration` feature.
//! Contract: DESIGN.md.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;

use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "integration-opt-in";
const FEATURE: &str = "integration";

#[derive(Default)]
pub struct IntegrationOptIn {
    checked: Cell<Option<usize>>,
}

/// The cargo targets under `<dir>/tests/`, relative to `dir`: `tests/x.rs` and `tests/x/main.rs`.
fn disk_targets(tree: &Tree, dir: &str) -> BTreeSet<String> {
    let prefix = if dir.is_empty() { "tests/".to_string() } else { format!("{dir}/tests/") };
    tree.entries
        .iter()
        .filter_map(|e| e.path.strip_prefix(&prefix))
        .filter_map(|rest| match rest.split('/').collect::<Vec<_>>()[..] {
            [f] if f.ends_with(".rs") => Some(format!("tests/{f}")),
            [d, "main.rs"] => Some(format!("tests/{d}/main.rs")),
            _ => None,
        })
        .collect()
}

fn finding(path: &str, message: String) -> Finding {
    Finding { rule: NAME, path: path.into(), line: None, message }
}

/// The findings for one manifest against the targets on disk.
fn judge(manifest: &str, text: &str, disk: &BTreeSet<String>) -> Vec<Finding> {
    let v: toml::Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(e) => return vec![finding(manifest, format!("does not parse: {e}"))],
    };
    let mut out = Vec::new();
    let auto = v.get("package").and_then(|p| p.get("autotests")).and_then(|a| a.as_bool());
    if auto != Some(false) {
        out.push(finding(manifest, "tests/ has targets but [package] does not set autotests = false".into()));
    }
    let mut declared = BTreeSet::new();
    for t in v.get("test").and_then(|t| t.as_array()).into_iter().flatten() {
        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let path = match t.get("path").and_then(|p| p.as_str()) {
            Some(p) => p.to_string(),
            None if disk.contains(&format!("tests/{name}.rs")) => format!("tests/{name}.rs"),
            None => format!("tests/{name}/main.rs"),
        };
        let gated = t
            .get("required-features")
            .and_then(|r| r.as_array())
            .is_some_and(|r| r.iter().any(|f| f.as_str() == Some(FEATURE)));
        if !gated {
            out.push(finding(manifest, format!("[[test]] {name} lacks required-features = [\"{FEATURE}\"]")));
        }
        if !disk.contains(&path) {
            out.push(finding(manifest, format!("[[test]] {name} names {path}, which is not a target on disk")));
        }
        declared.insert(path);
    }
    for missing in disk.difference(&declared) {
        out.push(finding(manifest, format!("{missing} has no [[test]]; with autotests = false cargo never builds it")));
    }
    out
}

impl Rule for IntegrationOptIn {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == "Cargo.toml" || e.path.ends_with("/Cargo.toml")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let mut out = Vec::new();
        let mut targets = 0;
        for e in crate::scope(tree, self)? {
            let dir = e.path.strip_suffix("Cargo.toml").unwrap_or("").trim_end_matches('/');
            let disk = disk_targets(tree, dir);
            if disk.is_empty() {
                continue;
            }
            let Some(c) = tree.content(e) else { continue };
            targets += disk.len();
            out.extend(judge(&e.path, &String::from_utf8_lossy(c), &disk));
        }
        if targets == 0 {
            return Err(LintError::Refused("no crate has a tests/ target — nothing to lint".into()));
        }
        if out.is_empty() {
            self.checked.set(Some(targets));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "integration test targets".into()))
    }

    fn hint(&self) -> &'static str {
        "plain `cargo test` runs unit tests only; an integration target runs when it is declared \
`[[test]]` with required-features = [\"integration\"] and the package sets autotests = false"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(IntegrationOptIn::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const GOOD: &str = "[package]\nname = \"a\"\nautotests = false\n\n[[test]]\nname = \"it\"\npath = \"tests/it/main.rs\"\nrequired-features = [\"integration\"]\n";

    fn run(manifest: &str, files: &[&str]) -> Result<Vec<String>, LintError> {
        let t = TempDir::new("iop");
        t.write("a/Cargo.toml", manifest);
        let mut all = vec!["a/Cargo.toml"];
        for f in files {
            t.write(f, "");
            all.push(f);
        }
        let tree = Tree::from_paths(t.path(), all, std::iter::empty::<&str>());
        IntegrationOptIn::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_gated_target_passes_and_is_counted() {
        assert_eq!(run(GOOD, &["a/tests/it/main.rs", "a/tests/it/helper.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_gate_on_the_wrong_feature_is_refused() {
        let bad = GOOD.replace("\"integration\"", "\"other\"");
        let r = run(&bad, &["a/tests/it/main.rs"]).unwrap();
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].contains("lacks required-features"), "{r:?}");
    }

    #[test]
    fn a_path_that_misses_the_target_is_refused_both_ways() {
        let bad = GOOD.replace("tests/it/main.rs", "tests/ti/main.rs");
        let r = run(&bad, &["a/tests/it/main.rs"]).unwrap();
        assert!(r.iter().any(|m| m.contains("not a target on disk")), "{r:?}");
        assert!(r.iter().any(|m| m.contains("has no [[test]]")), "{r:?}");
    }

    #[test]
    fn autotests_left_on_is_refused() {
        let bad = GOOD.replace("autotests = false\n", "");
        let r = run(&bad, &["a/tests/it/main.rs"]).unwrap();
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].contains("autotests = false"), "{r:?}");
    }

    #[test]
    fn a_flat_target_resolves_by_name_and_a_new_one_is_caught() {
        let m = "[package]\nname = \"a\"\nautotests = false\n\n[[test]]\nname = \"x\"\nrequired-features = [\"integration\"]\n";
        assert_eq!(run(m, &["a/tests/x.rs"]).unwrap(), Vec::<String>::new());
        let r = run(m, &["a/tests/x.rs", "a/tests/y.rs"]).unwrap();
        assert!(r.iter().any(|m| m.contains("tests/y.rs has no [[test]]")), "{r:?}");
    }

    #[test]
    fn cargo_lists_the_target_the_rule_accepts_as_gated() {
        let t = TempDir::new("iop-cargo");
        t.write("Cargo.toml", &format!("{GOOD}\n[features]\nintegration = []\n").replace("name = \"a\"", "name = \"a\"\nversion = \"0.0.0\"\nedition = \"2021\""));
        t.write("src/lib.rs", "");
        t.write("tests/it/main.rs", "");
        let out = std::process::Command::new("cargo")
            .args(["metadata", "--no-deps", "--offline", "--format-version", "1", "--manifest-path"])
            .arg(t.path().join("Cargo.toml"))
            .output()
            .expect("cargo runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let json = String::from_utf8_lossy(&out.stdout);
        assert!(json.contains("\"required-features\":[\"integration\"]"), "{json}");
        assert!(json.contains("\"name\":\"it\""), "{json}");
        let tree = Tree::from_paths(t.path(), ["Cargo.toml", "tests/it/main.rs"], std::iter::empty::<&str>());
        assert!(IntegrationOptIn::default().check(&tree).unwrap().is_empty());
    }

    #[test]
    fn a_tree_with_no_target_refuses_rather_than_pass() {
        assert!(run(GOOD, &[]).is_err());
    }
}
