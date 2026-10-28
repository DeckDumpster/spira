//! `config-fixture` — every registry key is declared in the complete test fixture and the
//! key history. Contract: DESIGN.md.

use std::collections::BTreeSet;

use crate::SyncCell as Cell;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "config-fixture";
const FIXTURE: &str = "spira-config/tests/fixtures/complete.toml";
const HISTORY: &str = "spira-config/schema/spira-key-history.txt";
const DIRS: [&str; 2] = ["spira/conf.d", "spira/conf.toml.d"];

#[derive(Default)]
pub struct ConfigFixture {
    checked: Cell<Option<usize>>,
}

fn is_key_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn bare_key(file: &str) -> String {
    file.strip_prefix("SPIRA_").unwrap_or(file).to_ascii_lowercase()
}

/// The keys of the fixture's `[spira]` table.
pub fn fixture_keys(text: &str) -> Result<BTreeSet<String>, String> {
    let v: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    Ok(v.get("spira").and_then(|s| s.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default())
}

/// `(required keys missing from the fixture, keys missing from the history)`, each sorted. The
/// resolve test requires only `spira/conf.d` keys in the fixture; toml-only keys are optional.
pub fn missing(
    required: &BTreeSet<String>,
    keys: &BTreeSet<String>,
    fixture: &BTreeSet<String>,
    history: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    (
        required.difference(fixture).cloned().collect(),
        keys.difference(history).cloned().collect(),
    )
}

impl Rule for ConfigFixture {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        DIRS.iter().any(|d| e.path.starts_with(&format!("{d}/"))) || e.path == FIXTURE || e.path == HISTORY
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let mut keys = BTreeSet::new();
        let mut conf_d = BTreeSet::new();
        for d in DIRS {
            if let Ok(rd) = std::fs::read_dir(tree.root.join(d)) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if is_key_name(&n) && e.path().is_file() {
                        keys.insert(bare_key(&n));
                        if d == DIRS[0] {
                            conf_d.insert(bare_key(&n));
                        }
                    }
                }
            }
        }
        if keys.is_empty() {
            return Err(LintError::Refused("no config registry keys found — nothing to compare".into()));
        }
        let read = |p: &str| {
            std::fs::read_to_string(tree.root.join(p)).map_err(|e| LintError::Refused(format!("cannot read {p}: {e}")))
        };
        let fixture = fixture_keys(&read(FIXTURE)?)
            .map_err(|e| LintError::Refused(format!("{FIXTURE} does not parse: {e}")))?;
        let history: BTreeSet<String> =
            read(HISTORY)?.lines().filter(|l| !l.is_empty()).map(str::to_string).collect();
        let (no_fixture, no_history) = missing(&conf_d, &keys, &fixture, &history);
        let mut out = Vec::new();
        for k in no_fixture {
            out.push(Finding {
                rule: NAME,
                path: FIXTURE.into(),
                line: None,
                message: format!("registry key {k} has no entry under [spira] in {FIXTURE}"),
            });
        }
        for k in no_history {
            out.push(Finding {
                rule: NAME,
                path: HISTORY.into(),
                line: None,
                message: format!("registry key {k} is not listed in {HISTORY}"),
            });
        }
        if out.is_empty() {
            self.checked.set(Some(keys.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "registry keys".into()))
    }

    fn hint(&self) -> &'static str {
        "a registry key added under spira/conf.d must also be declared under [spira] in spira-config/tests/fixtures/complete.toml and listed in spira-config/schema/spira-key-history.txt; without them every config-reading test in the round refuses the config as not declared"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(ConfigFixture::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn repo() -> TempDir {
        let t = TempDir::new("cfgfixture");
        t.git_init();
        t.write("spira/conf.d/SPIRA_ALPHA", "TYPE=string\n");
        t.write("spira/conf.toml.d/SPIRA_BETA_KEY", "TYPE=string\n");
        t.write(FIXTURE, "[spira]\nalpha = \"a\"\n[repo.x]\ngamma = 1\n");
        t.write(HISTORY, "alpha\nbeta_key\nretired\n");
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "base"]);
        t
    }

    fn check(t: &TempDir) -> crate::testutil::Checked {
        let tree = Tree::from_git(t.path()).unwrap();
        let r = ConfigFixture::default();
        (r.check(&tree), r.checked())
    }

    #[test]
    fn a_declared_registry_is_clean_and_counts_keys() {
        let (out, checked) = check(&repo());
        assert_eq!(out, Ok(vec![]));
        assert_eq!(checked, Some((2, "registry keys".into())));
    }

    #[test]
    fn a_key_without_a_fixture_entry_fails_and_names_key_and_file() {
        let t = repo();
        t.write("spira/conf.d/SPIRA_ROUND_NEW", "TYPE=u32\n");
        t.write(HISTORY, "alpha\nbeta_key\nround_new\n");
        let out = check(&t).0.unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].message.contains("round_new") && out[0].path == FIXTURE, "{out:?}");
    }

    #[test]
    fn a_key_without_a_history_entry_fails_and_names_key_and_file() {
        let t = repo();
        t.write("spira/conf.d/SPIRA_ROUND_NEW", "TYPE=u32\n");
        t.write(FIXTURE, "[spira]\nalpha = \"a\"\nbeta_key = 1\nround_new = 3\n");
        let out = check(&t).0.unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].message.contains("round_new") && out[0].path == HISTORY, "{out:?}");
    }

    #[test]
    fn a_toml_only_key_needs_the_history_but_not_the_fixture() {
        let t = repo();
        t.write("spira/conf.toml.d/SPIRA_TOML_ONLY", "TYPE=string\n");
        let out = check(&t).0.unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].message.contains("toml_only") && out[0].path == HISTORY, "{out:?}");
    }

    #[test]
    fn a_key_in_a_table_other_than_spira_does_not_count() {
        let t = repo();
        t.write("spira/conf.d/SPIRA_GAMMA", "TYPE=u32\n");
        t.write(HISTORY, "alpha\nbeta_key\ngamma\n");
        t.write(FIXTURE, "[spira]\nalpha = \"a\"\nbeta_key = 1\n[repo.x]\ngamma = 1\n");
        assert_eq!(check(&t).0.unwrap().len(), 1);
    }

    #[test]
    fn it_refuses_without_keys_or_fixture() {
        let t = repo();
        t.remove(FIXTURE);
        assert!(matches!(check(&t).0, Err(LintError::Refused(_))));
        let e = TempDir::new("cfgfixture-empty");
        e.git_init();
        e.write("README", "x");
        e.git(&["add", "."]);
        e.git(&["commit", "-qm", "x"]);
        assert!(matches!(check(&e).0, Err(LintError::Refused(_))));
    }
}
