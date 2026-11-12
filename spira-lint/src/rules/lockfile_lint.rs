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

use crate::SyncCell as Cell;
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

/// Why `cargo metadata --locked --offline` refuses this tree's lock, or `None` when the lock is
/// what the manifests resolve to — the check the forge's `--locked` build makes.
pub fn lock_drift(root: &std::path::Path) -> Result<Option<String>, String> {
    // batch-job: cargo resolves the whole workspace; 120 s hang guard
    let mut cmd = std::process::Command::new("timeout");
    cmd.args(["120", "cargo", "metadata", "--locked", "--offline", "--format-version", "1"])
        .current_dir(root);
    if let Some(home) = std::env::var_os("CARGO_HOME").filter(|h| !h.is_empty()) {
        cmd.env("CARGO_HOME", home);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if out.status.success() {
        return Ok(None);
    }
    if out.status.code() == Some(124) {
        return Err("cargo metadata --locked --offline: timed out after 120s".into());
    }
    refusal(&String::from_utf8_lossy(&out.stderr))
}

/// Reads a failed `cargo metadata`'s stderr: drift when cargo names the lock file, otherwise a
/// refusal carrying cargo's last `error` line — the warnings before it are not the reason.
pub fn refusal(stderr: &str) -> Result<Option<String>, String> {
    let stderr = stderr.trim();
    let error = stderr.lines().rev().find(|l| l.starts_with("error")).unwrap_or(stderr);
    if stderr.contains("lock file") {
        Ok(Some(error.to_string()))
    } else {
        Err(format!("cargo metadata --locked --offline failed for another reason: {error}"))
    }
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
        e.path == LOCK || e.path.ends_with("Cargo.toml")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let mut drift = Vec::new();
        if let Some(why) = lock_drift(&tree.root).map_err(LintError::Refused)? {
            drift.push(Finding {
                rule: NAME,
                path: LOCK.into(),
                line: None,
                message: format!("{LOCK} is not what the manifests resolve to (cargo metadata --locked --offline): {why}"),
            });
        }
        if !drift.is_empty() {
            return Ok(drift);
        }
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
        let out: Vec<Finding> = drift.into_iter().chain(offenders(&prior, &head, &diff)
            .into_iter()
            .map(|(n, bv, hv)| Finding {
                rule: NAME,
                path: LOCK.into(),
                line: None,
                message: format!("{n} {} -> {hv} with no matching Cargo.toml change", bv.join("/")),
            }))
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
        "if cargo says the lock needs updating, the manifests and Cargo.lock disagree: regenerate it (cargo update -w) and commit it with the manifest change. Otherwise a Cargo.lock bump has no matching Cargo.toml change — either the dependency change belongs in a Cargo.toml, or the lockfile drifted under an unpinned local toolchain and should be regenerated under rust-toolchain.toml"
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(LockfileLint::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;


    const LIB: &str = "";
    fn manifest(deps: &str) -> String {
        format!("[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\n{deps}")
    }
    const B_MANIFEST: &str = "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    const LOCK_WITH_B: &str = "version = 3\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\ndependencies = [\n \"b\",\n]\n\n[[package]]\nname = \"b\"\nversion = \"0.1.0\"\n";

    fn project(deps: &str) -> crate::testutil::TempDir {
        let t = crate::testutil::TempDir::new("lock-drift");
        t.write("a/Cargo.toml", &manifest(deps));
        t.write("a/src/lib.rs", LIB);
        t.write("b/Cargo.toml", B_MANIFEST);
        t.write("b/src/lib.rs", LIB);
        t.write("Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n");
        t.write("Cargo.lock", LOCK_WITH_B);
        t
    }

    #[test]
    fn a_lock_that_keeps_a_dependency_the_manifest_dropped_is_drift() {
        let t = project("");
        let why = lock_drift(t.path()).unwrap();
        assert!(why.is_some(), "the lock still lists b under a; --locked must refuse");
    }

    fn git(t: &crate::testutil::TempDir, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(t.path())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn check_reports_a_manifest_that_dropped_a_dependency_the_lock_keeps() {
        let t = project("b = { path = \"../b\" }\n");
        git(&t, &["init", "-q"]);
        git(&t, &["add", "-A"]);
        git(&t, &["commit", "-qm", "base"]);
        t.write("a/Cargo.toml", &manifest(""));
        let rule = LockfileLint::default();
        let tree = Tree::from_paths(t.path(), ["a/Cargo.toml"], Vec::<String>::new()).with_base(Some("HEAD".into()));
        let got = rule.check(&tree).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].message.contains("not what the manifests resolve to"), "{got:?}");
        assert_eq!(rule.checked(), None);
    }

    #[test]
    fn a_manifest_edit_alone_brings_the_rule_in() {
        let t = project("");
        let tree = Tree::from_paths(t.path(), ["a/Cargo.toml"], Vec::<String>::new());
        assert!(tree.entries.iter().any(|e| LockfileLint::default().applies_to(e)));
    }

    #[test]
    fn a_failure_after_warnings_reports_the_error_line_not_the_warnings() {
        let stderr = "warning: profiles for the non root package will be ignored\nwarning: another\nerror: no matching package named `x` found\n";
        let why = refusal(stderr).unwrap_err();
        assert!(why.contains("no matching package named `x`"), "{why}");
        assert!(!why.contains("profiles"), "{why}");
    }

    #[test]
    fn a_lock_file_error_after_warnings_is_drift_carrying_the_error_line() {
        let stderr = "warning: w\nerror: the lock file /r/Cargo.lock needs to be updated but --locked was passed\n";
        let why = refusal(stderr).unwrap().unwrap();
        assert!(why.starts_with("error: the lock file"), "{why}");
    }

    #[test]
    fn a_lock_that_matches_its_manifests_is_not_drift() {
        let t = project("b = { path = \"../b\" }\n");
        assert_eq!(lock_drift(t.path()).unwrap(), None);
    }

    #[test]
    fn version_tuple_reads_three_integer_parts() {
        assert_eq!(version_tuple("1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(version_tuple("1.2.3-rc.1"), None);
        assert!(version_tuple("1.10.0") > version_tuple("1.9.9"));
    }
}
