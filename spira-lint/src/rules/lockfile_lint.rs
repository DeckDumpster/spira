//! `lockfile-lint` — a Cargo.lock version bump needs a matching Cargo.toml change. Ported
//! from `spira/lockfile-lint.sh` (sp-4kws1; ported by sp-ufbkh). Contract: DESIGN.md.
//!
//! The pinned toolchain cannot stop a Cargo.lock that an earlier, unpinned `cargo build`
//! already bumped — resolving a registry package to a version the pinned toolchain cannot
//! parse, with no Cargo.toml edit to review. For every package whose locked version rises
//! between the base and this tree, some Cargo.toml diff since the base must name it.
//!
//! The bash fence skipped with exit 0 when it had no base, no Cargo.lock here or none at the
//! base. Each of those is a refusal here: a fence that cannot compare has not compared.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use crate::{Entry, Finding, LintError, Rule, Tree};

const NAME: &str = "lockfile-lint";
const LOCK: &str = "Cargo.lock";

#[derive(Default)]
pub struct LockfileLint {
    /// Packages in this tree's lock, after a clean check.
    checked: Cell<Option<usize>>,
}

/// `name -> versions` over a Cargo.lock's `[[package]]` tables.
pub fn package_versions(text: &str) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let v: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in v.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
        if let (Some(n), Some(ver)) = (
            p.get("name").and_then(|x| x.as_str()),
            p.get("version").and_then(|x| x.as_str()),
        ) {
            out.entry(n.to_string()).or_default().insert(ver.to_string());
        }
    }
    Ok(out)
}

/// The first three dot-separated parts as integers; `None` when any is not one (a
/// pre-release), which the lint does not judge.
pub fn version_tuple(v: &str) -> Option<Vec<u64>> {
    v.split('.').take(3).map(|p| p.parse::<u64>().ok()).collect()
}

/// `(name, base versions, the new version)` for each bump no Cargo.toml diff names.
pub fn offenders(
    base: &BTreeMap<String, BTreeSet<String>>,
    head: &BTreeMap<String, BTreeSet<String>>,
    toml_diff: &str,
) -> Vec<(String, Vec<String>, String)> {
    let named = |name: &str| {
        regex::Regex::new(&format!(r"\b{}\b", regex::escape(name))).is_ok_and(|r| r.is_match(toml_diff))
    };
    let mut out = Vec::new();
    for (name, hv) in head {
        let Some(bv) = base.get(name) else { continue }; // a new package is not a bump
        for v in hv.difference(bv) {
            let Some(t) = version_tuple(v) else { continue };
            let bt: Vec<Vec<u64>> = bv.iter().filter_map(|b| version_tuple(b)).collect();
            if !bt.is_empty() && bt.iter().all(|b| *b < t) && !named(name) {
                out.push((name.clone(), bv.iter().cloned().collect(), v.clone()));
            }
        }
    }
    out
}

impl Rule for LockfileLint {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == LOCK
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let base = tree.base_commit()?;
        let head = std::fs::read_to_string(tree.root.join(LOCK))
            .map_err(|_| LintError::Refused(format!("no {LOCK} in this tree — nothing to lint")))?;
        let prior = tree
            .show(&base, LOCK)
            .ok_or_else(|| LintError::Refused(format!("no {LOCK} at the base {base} — nothing to compare a bump against")))?;
        let bad_lock = |which: &str, e: String| LintError::Refused(format!("{which} {LOCK} does not parse: {e}"));
        let head = package_versions(&head).map_err(|e| bad_lock("this tree's", e))?;
        let prior = package_versions(&prior).map_err(|e| bad_lock("the base's", e))?;
        if head.is_empty() {
            return Err(LintError::Refused(format!("{LOCK} locks no package — nothing to lint")));
        }
        let diff = tree
            .git(&["diff", &base, "--", "*Cargo.toml"])
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(LintError::Refused)?;
        let out: Vec<Finding> = offenders(&prior, &head, &diff)
            .into_iter()
            .map(|(n, bv, hv)| Finding {
                rule: NAME,
                path: LOCK.into(),
                line: None,
                message: format!("{n} {} -> {hv} with no matching Cargo.toml change", bv.join("/")),
            })
            .collect();
        if out.is_empty() {
            self.checked.set(Some(head.values().map(BTreeSet::len).sum()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "packages".into()))
    }

    fn hint(&self) -> &'static str {
        "a Cargo.lock bump has no matching Cargo.toml change — either the dependency change belongs in a Cargo.toml, or the lockfile drifted under an unpinned local toolchain and should be regenerated under rust-toolchain.toml"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn lock(serde: &str) -> String {
        format!(
            "version = 3\n\n[[package]]\nname = \"serde\"\nversion = \"{serde}\"\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n"
        )
    }

    fn repo() -> TempDir {
        let t = TempDir::new("lockfile");
        t.git_init();
        t.write("Cargo.toml", "[package]\nname = \"app\"\n[dependencies]\nfoo = \"1\"\n");
        t.write(LOCK, &lock("1.0.100"));
        t.git(&["add", "."]);
        t.git(&["commit", "-qm", "base"]);
        t.git(&["tag", "base"]);
        t
    }

    fn check(t: &TempDir, base: Option<&str>) -> crate::testutil::Checked {
        let tree = Tree::from_git(t.path()).unwrap().with_base(base.map(str::to_string));
        let r = LockfileLint::default();
        (r.check(&tree), r.checked())
    }

    #[test]
    fn an_unchanged_lock_is_clean_and_counts_its_packages() {
        let t = repo();
        let (out, checked) = check(&t, Some("base"));
        assert_eq!(out, Ok(vec![]));
        assert_eq!(checked, Some((2, "packages".into())));
    }

    #[test]
    fn a_bump_with_no_cargo_toml_change_is_a_finding() {
        let t = repo();
        t.write(LOCK, &lock("1.0.228"));
        let (out, checked) = check(&t, Some("base"));
        let out = out.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].message, "serde 1.0.100 -> 1.0.228 with no matching Cargo.toml change");
        assert_eq!(checked, None);
    }

    #[test]
    fn a_bump_named_by_a_cargo_toml_diff_is_clean() {
        let t = repo();
        t.write(LOCK, &lock("1.0.228"));
        t.write("Cargo.toml", "[package]\nname = \"app\"\n[dependencies]\nfoo = \"1\"\nserde = \"1.0.228\"\n");
        assert_eq!(check(&t, Some("base")).0, Ok(vec![]));
    }

    #[test]
    fn a_downgrade_a_new_package_and_a_prerelease_are_out_of_scope() {
        let t = repo();
        t.write(LOCK, &format!("{}\n[[package]]\nname = \"new\"\nversion = \"1.0.0\"\n", lock("1.0.1")));
        assert_eq!(check(&t, Some("base")).0, Ok(vec![]));
        t.write(LOCK, &lock("1.0.200-rc.1"));
        assert_eq!(check(&t, Some("base")).0, Ok(vec![]));
    }

    #[test]
    fn it_refuses_what_it_cannot_compare() {
        let t = repo();
        let refused = |r: Result<Vec<Finding>, LintError>, needle: &str| {
            assert!(matches!(&r, Err(LintError::Refused(m)) if m.contains(needle)), "{r:?}");
        };
        refused(check(&t, None).0, "SPIRA_GATE_BASE");
        refused(check(&t, Some("nope")).0, "does not resolve");
        t.remove(LOCK);
        refused(check(&t, Some("base")).0, "no Cargo.lock in this tree");
        t.write(LOCK, &lock("1.0.100"));
        t.git(&["rm", "-q", "--cached", LOCK]);
        t.git(&["commit", "-qm", "drop"]);
        refused(check(&t, Some("HEAD")).0, "no Cargo.lock at the base");
    }

    #[test]
    fn version_tuple_reads_three_integer_parts() {
        assert_eq!(version_tuple("1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(version_tuple("1.2.3-rc.1"), None);
        assert!(version_tuple("1.10.0") > version_tuple("1.9.9"));
    }
}
