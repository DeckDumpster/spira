//! `integration-opt-in` — no workspace member has an integration test target that a plain
//! `cargo test` builds. Contract: DESIGN.md.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;

use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "integration-opt-in";
const ROOT: &str = "Cargo.toml";
const FEATURE: &str = "integration";

#[derive(Default)]
pub struct IntegrationOptIn {
    checked: Cell<Option<usize>>,
}

/// Cargo's auto-discovered integration targets of the member at `dir`: `tests/*.rs` and
/// `tests/*/main.rs`, as paths relative to the member.
fn discovered(tree: &Tree, dir: &str) -> BTreeSet<String> {
    let pre = format!("{dir}/tests/");
    tree.entries
        .iter()
        .filter_map(|e| e.path.strip_prefix(&pre))
        .filter(|rest| match rest.split('/').collect::<Vec<_>>().as_slice() {
            [f] => f.ends_with(".rs"),
            [_, "main.rs"] => true,
            _ => false,
        })
        .map(|rest| format!("tests/{rest}"))
        .collect()
}

/// What is wrong with one member's manifest, as messages.
pub fn problems(manifest: &str, targets: &BTreeSet<String>) -> Result<Vec<String>, String> {
    let v: toml::Value = toml::from_str(manifest).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if v.get("features").and_then(|f| f.get(FEATURE)).is_none() {
        out.push(format!("declares no `{FEATURE}` feature, so `--features {FEATURE}` fails when this crate is selected"));
    }
    let tests: &[toml::Value] = v.get("test").and_then(|t| t.as_array()).map_or(&[], |a| a.as_slice());
    let mut declared = BTreeSet::new();
    for t in tests {
        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let gated = t
            .get("required-features")
            .and_then(|r| r.as_array())
            .is_some_and(|r| r.iter().any(|f| f.as_str() == Some(FEATURE)));
        if !gated {
            out.push(format!("[[test]] `{name}` lacks required-features = [\"{FEATURE}\"]"));
        }
        if let Some(p) = t.get("path").and_then(|p| p.as_str()) {
            declared.insert(p.to_string());
        } else {
            declared.insert(format!("tests/{name}.rs"));
        }
    }
    let auto = v.get("package").and_then(|p| p.get("autotests")).and_then(|a| a.as_bool()) != Some(false);
    if auto && !targets.is_empty() {
        out.push(format!("autotests is on, so {} is built by a plain `cargo test`; set `autotests = false` and declare each as a [[test]] with required-features = [\"{FEATURE}\"]", targets.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    for t in targets.difference(&declared) {
        out.push(format!("{t} has no [[test]] entry"));
    }
    Ok(out)
}

impl Rule for IntegrationOptIn {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == ROOT
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        crate::scope(tree, self)?;
        let root: toml::Value = toml::from_str(&tree.read_text(ROOT)).map_err(|e| LintError::Refused(format!("{ROOT} does not parse: {e}")))?;
        let members: BTreeSet<&str> = root
            .get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
            .map(|a| a.iter().filter_map(|m| m.as_str()).collect())
            .unwrap_or_default();
        if members.is_empty() {
            return Err(LintError::Refused(format!("{ROOT} lists no workspace members — nothing to check")));
        }
        let mut out = Vec::new();
        for m in &members {
            let path = format!("{m}/Cargo.toml");
            let text = tree.text_of(&path).ok_or_else(|| LintError::Refused(format!("workspace member {m} has no readable {path}")))?;
            let bad = problems(&text, &discovered(tree, m)).map_err(|e| LintError::Refused(format!("{path} does not parse: {e}")))?;
            out.extend(bad.into_iter().map(|message| Finding { rule: NAME, path: path.clone(), line: None, message }));
        }
        if out.is_empty() {
            self.checked.set(Some(members.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "workspace members".into()))
    }

    fn hint(&self) -> &'static str {
        "integration tests are opt-in: a plain `cargo test` runs unit tests only. In the crate's Cargo.toml add `autotests = false`, `[features] integration = []`, and for each tests/ target a `[[test]]` with `required-features = [\"integration\"]`; run it with `--features integration`."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(IntegrationOptIn::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const GOOD: &str = "[package]\nname = \"a\"\nautotests = false\n[features]\nintegration = []\n[[test]]\nname = \"it\"\npath = \"tests/it/main.rs\"\nrequired-features = [\"integration\"]\n";

    fn tree(t: &TempDir, manifest: &str, files: &[&str]) -> Result<Vec<String>, LintError> {
        t.write("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n");
        t.write("a/Cargo.toml", manifest);
        let mut all = vec!["Cargo.toml", "a/Cargo.toml"];
        all.extend_from_slice(files);
        let tr = Tree::from_paths(t.path(), all, std::iter::empty::<&str>());
        IntegrationOptIn::default().check(&tr).map(|v| v.iter().map(|f| f.message.clone()).collect())
    }

    #[test]
    fn a_gated_target_and_the_feature_pass() {
        let t = TempDir::new("ioo-good");
        assert_eq!(tree(&t, GOOD, &["a/tests/it/main.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_new_auto_discovered_target_is_refused() {
        let t = TempDir::new("ioo-auto");
        let m = "[package]\nname = \"a\"\n[features]\nintegration = []\n";
        let got = tree(&t, m, &["a/tests/new.rs"]).unwrap();
        assert!(got.iter().any(|g| g.contains("autotests is on") && g.contains("tests/new.rs")), "{got:?}");
    }

    #[test]
    fn a_target_declared_without_the_gate_is_refused() {
        let t = TempDir::new("ioo-ungated");
        let m = GOOD.replace("required-features = [\"integration\"]\n", "");
        let got = tree(&t, &m, &["a/tests/it/main.rs"]).unwrap();
        assert!(got.iter().any(|g| g.contains("lacks required-features")), "{got:?}");
    }

    #[test]
    fn a_target_missing_from_the_manifest_is_refused_even_with_autotests_off() {
        let t = TempDir::new("ioo-undeclared");
        let got = tree(&t, GOOD, &["a/tests/it/main.rs", "a/tests/extra.rs"]).unwrap();
        assert_eq!(got, vec!["tests/extra.rs has no [[test]] entry".to_string()]);
    }

    #[test]
    fn a_member_without_the_feature_is_refused() {
        let t = TempDir::new("ioo-nofeature");
        let got = tree(&t, "[package]\nname = \"a\"\n", &[]).unwrap();
        assert!(got.iter().any(|g| g.contains("no `integration` feature")), "{got:?}");
    }

    #[test]
    fn a_workspace_with_no_members_refuses() {
        let t = TempDir::new("ioo-empty");
        t.write("Cargo.toml", "[workspace]\nmembers = []\n");
        let tr = Tree::from_paths(t.path(), ["Cargo.toml"], std::iter::empty::<&str>());
        assert!(IntegrationOptIn::default().check(&tr).is_err());
    }
}
