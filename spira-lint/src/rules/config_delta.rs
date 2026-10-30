//! `config-delta` — a registry key added or dropped relative to the base is declared in
//! `spira/config-delta.toml`. Contract: DESIGN.md.

use std::collections::BTreeSet;

use crate::SyncCell as Cell;
use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "config-delta";
const DELTA: &str = "spira/config-delta.toml";
const DIRS: [&str; 2] = ["spira/conf.d", "spira/conf.toml.d"];

#[derive(Default)]
pub struct ConfigDelta {
    checked: Cell<Option<usize>>,
}

fn is_key_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub fn config_path(file: &str) -> String {
    format!("spira.{}", file.strip_prefix("SPIRA_").unwrap_or(file).to_ascii_lowercase())
}

/// Word pairs by which a release renamed its vocabulary: a key dropped under the first word
/// and added under the second is a rename, which carries the operator's value.
const RENAMES: &[(&str, &str)] = &[("aeons", "dwarves"), ("aeon", "dwarf"), ("summon", "call")]; // ffx-ok

fn renamed_pairs(text: &str) -> Result<Vec<(String, String)>, String> {
    let v: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    Ok(v.get("renamed")
        .and_then(|r| r.as_table())
        .map(|t| t.iter().filter_map(|(f, to)| to.as_str().map(|to| (f.clone(), to.to_string()))).collect())
        .unwrap_or_default())
}

/// `(added, removed)` declared by a delta's text; a `[renamed]` entry declares its target
/// added and its source removed.
pub fn declared(text: &str) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let (added, removed) = spira_config::parse_config_delta(text)?;
    let (mut added, mut removed): (BTreeSet<String>, BTreeSet<String>) = (added.into_iter().collect(), removed.into_iter().collect());
    for (from, to) in renamed_pairs(text)? {
        removed.insert(from);
        added.insert(to);
    }
    Ok((added, removed))
}

/// `(removed, added)` pairs where the removed key maps to the added key through [`RENAMES`]
/// yet no `[renamed]` entry carries the value.
pub fn unrenamed(text: &str) -> Result<Vec<(String, String)>, String> {
    let (added, removed) = spira_config::parse_config_delta(text)?;
    let carried: BTreeSet<String> = renamed_pairs(text)?.into_iter().map(|(f, _)| f).collect();
    let mut out = Vec::new();
    for r in removed.iter().filter(|r| !carried.contains(*r)) {
        for (old, new) in RENAMES {
            if r.contains(old) {
                let twin = r.replacen(old, new, 1);
                if added.contains(&twin) {
                    out.push((r.clone(), twin));
                }
            }
        }
    }
    Ok(out)
}

/// `(undeclared additions, undeclared removals)` between two key sets.
pub fn undeclared(
    base: &BTreeSet<String>,
    head: &BTreeSet<String>,
    added: &BTreeSet<String>,
    removed: &BTreeSet<String>,
) -> (Vec<String>, Vec<String>) {
    (
        head.difference(base).filter(|k| !added.contains(*k)).cloned().collect(),
        base.difference(head).filter(|k| !removed.contains(*k)).cloned().collect(),
    )
}

impl Rule for ConfigDelta {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        DIRS.iter().any(|d| e.path.starts_with(&format!("{d}/"))) || e.path == DELTA
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let base = tree.base_commit()?;
        let mut head = BTreeSet::new();
        let mut prior = BTreeSet::new();
        for d in DIRS {
            if let Ok(rd) = std::fs::read_dir(tree.root.join(d)) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if is_key_name(&n) && e.path().is_file() {
                        head.insert(config_path(&n));
                    }
                }
            }
            if let Ok(out) = tree.git(&["ls-tree", "--name-only", &base, &format!("{d}/")]) {
                for line in String::from_utf8_lossy(&out).lines() {
                    let n = line.rsplit('/').next().unwrap_or(line);
                    if is_key_name(n) {
                        prior.insert(config_path(n));
                    }
                }
            }
        }
        if head.is_empty() || prior.is_empty() {
            return Err(LintError::Refused(format!(
                "no config registry keys found ({} here, {} at the base) — nothing to compare",
                head.len(),
                prior.len()
            )));
        }
        let text = std::fs::read_to_string(tree.root.join(DELTA)).unwrap_or_default();
        let (added, removed) =
            declared(&text).map_err(|e| LintError::Refused(format!("{DELTA} does not parse: {e}")))?;
        let typed = spira_config::delta_added(&text).and_then(|a| spira_config::check_delta(&tree.root.join("spira"), &a));
        let (no_add, no_rm) = undeclared(&prior, &head, &added, &removed);
        let mut out = Vec::new();
        if let Err(e) = typed {
            out.push(Finding { rule: NAME, path: DELTA.into(), line: None, message: format!("{DELTA} does not load under the typed schema: {e}") });
        }
        for k in no_add {
            out.push(Finding {
                rule: NAME,
                path: DELTA.into(),
                line: None,
                message: format!("registry key {k} is added but {DELTA} has no entry for it under its config table"),
            });
        }
        for k in no_rm {
            out.push(Finding {
                rule: NAME,
                path: DELTA.into(),
                line: None,
                message: format!("registry key {k} is removed but {DELTA} does not list it under removed"),
            });
        }
        for (from, to) in unrenamed(&text).map_err(|e| LintError::Refused(format!("{DELTA} does not parse: {e}")))? {
            out.push(Finding {
                rule: NAME,
                path: DELTA.into(),
                line: None,
                message: format!("{from} is removed and {to} added without a [renamed] entry: the operator's value would be re-defaulted, not carried"),
            });
        }
        if out.is_empty() {
            self.checked.set(Some(head.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "registry keys".into()))
    }

    fn hint(&self) -> &'static str {
        "a release that adds or drops a registry key must declare it in spira/config-delta.toml ([spira] <key> = <value>, or removed = [\"spira.<key>\"]); otherwise activation has nothing to apply and the new release refuses the config in force"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ConfigDelta::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn repo() -> TempDir {
        let t = TempDir::new("cfgdelta");
        t.git_init();
        t.write("spira/conf.d/SPIRA_ALPHA", "TYPE=u32\n");
        t.write("spira/conf.d/SPIRA_BETA", "TYPE=string\n");
        t.write("spira/conf.toml.d/.keep", "");
        t.write("spira/testlib/complete.toml", "[spira]\nalpha = 1\nbeta = \"b\"\n");
        t.write(DELTA, "[spira]\nalpha = 1\n");
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "base"]);
        t.git(&["tag", "base"]);
        t
    }

    fn check(t: &TempDir) -> crate::testutil::Checked {
        let tree = Tree::from_git(t.path()).unwrap().with_base(Some("base".into()));
        let r = ConfigDelta::default();
        (r.check(&tree), r.checked())
    }

    #[test]
    fn an_unchanged_registry_is_clean_and_counts_keys() {
        let t = repo();
        let (out, checked) = check(&t);
        assert_eq!(out, Ok(vec![]));
        assert_eq!(checked, Some((2, "registry keys".into())));
    }

    #[test]
    fn an_added_key_without_an_entry_fails_and_names_it() {
        let t = repo();
        t.write("spira/conf.d/SPIRA_GAMMA_KEY", "TYPE=string\n");
        let out = check(&t).0.unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].message.contains("spira.gamma_key"), "{out:?}");
    }

    #[test]
    fn an_added_key_with_an_entry_passes_including_toml_only() {
        let t = repo();
        t.write("spira/conf.toml.d/SPIRA_GAMMA_KEY", "TYPE=string\n");
        t.write(DELTA, "[spira]\nalpha = 1\ngamma_key = \"x\"\n");
        assert_eq!(check(&t).0, Ok(vec![]));
    }

    #[test]
    fn a_removed_key_without_an_entry_fails_and_with_one_passes() {
        let t = repo();
        t.remove("spira/conf.d/SPIRA_BETA");
        let out = check(&t).0.unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].message.contains("spira.beta"), "{out:?}");
        t.write(DELTA, "removed = [\"spira.beta\"]\n[spira]\nalpha = 1\n");
        assert_eq!(check(&t).0, Ok(vec![]));
    }

    #[test]
    fn a_removal_and_addition_that_differ_by_a_rename_word_need_a_renamed_entry() {
        let t = repo();
        t.write("spira/conf.d/SPIRA_MAX_DWARVES", "TYPE=string\n");
        t.remove("spira/conf.d/SPIRA_BETA");
        t.write("spira/conf.d/SPIRA_MAX_AEONS", "TYPE=string\n"); // ffx-ok
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "base2"]);
        t.git(&["tag", "-f", "base"]);
        t.remove("spira/conf.d/SPIRA_MAX_AEONS"); // ffx-ok
        t.write(DELTA, "removed = [\"spira.max_aeons\"]\n[spira]\nmax_dwarves = 4\n"); // ffx-ok
        t.git(&["add", "-A"]);
        t.git(&["rm", "-q", "--cached", "-r", "--ignore-unmatch", "spira/conf.d/SPIRA_BETA"]);
        let out = check(&t).0.unwrap();
        assert!(out.iter().any(|f| f.message.contains("without a [renamed] entry")), "{out:?}");
        t.write(DELTA, "[renamed]\n\"spira.max_aeons\" = \"spira.max_dwarves\"\n"); // ffx-ok
        let out = check(&t).0.unwrap();
        assert!(!out.iter().any(|f| f.message.contains("max_aeons")), "{out:?}"); // ffx-ok
    }

    #[test]
    fn a_placeholder_path_in_the_delta_fails_the_lint() {
        let t = repo();
        let control = check(&t).0;
        assert_eq!(control, Ok(vec![]));
        t.write(DELTA, "[spira]\nalpha = \"/placeholder/run/x.json\"\n");
        match check(&t).0 {
            Err(LintError::Refused(m)) => assert!(m.contains("placeholder path"), "{m}"),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_value_of_the_wrong_type_fails_the_lint_naming_the_key() {
        let t = repo();
        assert_eq!(check(&t).0, Ok(vec![]));
        t.write(DELTA, "[spira]\nalpha = \"3600\"\n");
        let out = check(&t).0.unwrap();
        assert!(out.len() == 1 && out[0].message.contains("spira.alpha") && out[0].message.contains("u32"), "{out:?}");
    }

    #[test]
    fn it_refuses_without_a_base() {
        let t = repo();
        let tree = Tree::from_git(t.path()).unwrap();
        assert!(matches!(ConfigDelta::default().check(&tree), Err(LintError::Refused(_))));
    }
}
