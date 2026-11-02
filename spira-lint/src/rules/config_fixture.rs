//! `config-fixture` — every registry key is declared in the complete test fixture and the
//! key history. Contract: DESIGN.md.

use std::collections::BTreeSet;

use crate::SyncCell as Cell;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "config-fixture";
const FIXTURE: &str = "spira/testlib/complete.toml";
const HISTORY: &str = "spira/conf.key-history";
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
        "a registry key added under spira/conf.d must also be declared under [spira] in spira/testlib/complete.toml and listed in spira/conf.key-history; without them every config-reading test in the round refuses the config as not declared"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(ConfigFixture::default())]
}
