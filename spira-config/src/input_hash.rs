//! The hash of what a workspace crate builds from, so a test's results can be compared across
//! runs on an unchanged input.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Workspace-wide build inputs: a change to any of them changes every crate's build.
pub const ROOT_INPUTS: [&str; 5] = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rust-toolchain", ".cargo"];

/// A workspace member as the hash reads it.
#[derive(Clone, Debug, Default)]
pub struct Member {
    pub name: String,
    /// Relative to the workspace root, no trailing `/`; empty for a root package.
    pub dir: String,
    /// Workspace dependencies: normal, dev and build.
    pub deps: Vec<String>,
    /// Workspace dependencies that go into its binaries: normal and build.
    pub build_deps: Vec<String>,
    /// Workspace crates whose binaries its tests launch.
    pub test_bins: Vec<String>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// `seed` and everything reachable from it along `edges`.
fn closure<'m>(by_name: &BTreeMap<&str, &'m Member>, seed: &'m str, edges: fn(&Member) -> &[String]) -> BTreeSet<&'m str> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![seed];
    while let Some(m) = todo.pop().and_then(|n| by_name.get(n)) {
        if seen.insert(m.name.as_str()) {
            todo.extend(edges(m).iter().map(String::as_str).filter(|d| !seen.contains(d)));
        }
    }
    seen
}

/// Each member's input hash: sha256 over the sorted `<path> <oid>` lines of every directory in
/// its dependency closure, of what its test binaries are built from, and of [`ROOT_INPUTS`].
/// `ls_tree` is `git ls-tree -r -d <tree>` followed by `git ls-tree <tree>`.
pub fn input_hashes(members: &[Member], ls_tree: &str) -> BTreeMap<String, String> {
    let oids: BTreeMap<&str, &str> = ls_tree.lines().filter_map(|l| l.split_once('\t')).filter_map(|(meta, path)| Some((path, meta.split_whitespace().nth(2)?))).collect();
    let by_name: BTreeMap<&str, &Member> = members.iter().map(|m| (m.name.as_str(), m)).collect();
    members
        .iter()
        .map(|m| {
            let mut crates = closure(&by_name, &m.name, |m| &m.deps);
            for bin in &m.test_bins {
                crates.extend(closure(&by_name, bin, |m| &m.build_deps));
            }
            let mut paths: BTreeSet<&str> = ROOT_INPUTS.iter().copied().filter(|r| oids.contains_key(r)).collect();
            for dir in crates.iter().filter_map(|c| by_name.get(c)).map(|c| c.dir.as_str()) {
                if dir.is_empty() {
                    paths.extend(oids.keys().copied().filter(|p| !p.contains('/')));
                } else {
                    paths.insert(dir);
                }
            }
            let lines: String = paths.iter().map(|p| format!("{p} {}\n", oids.get(p).copied().unwrap_or("-"))).collect();
            (m.name.clone(), sha256_hex(lines.as_bytes()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(name: &str, dir: &str, deps: &[&str], build_deps: &[&str], test_bins: &[&str]) -> Member {
        let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect();
        Member { name: name.into(), dir: dir.into(), deps: v(deps), build_deps: v(build_deps), test_bins: v(test_bins) }
    }

    /// `app` builds from `lib` (which builds from `base`), tests with `kit`, and launches
    /// `tool`, which builds from `toollib` and tests with `toolkit`; `other` is unrelated.
    fn ws() -> Vec<Member> {
        vec![
            m("app", "app", &["lib", "kit"], &["lib"], &["tool"]),
            m("lib", "libs/lib", &["base"], &["base"], &[]),
            m("base", "base", &[], &[], &[]),
            m("kit", "kit", &[], &[], &[]),
            m("tool", "tool", &["toollib", "toolkit"], &["toollib"], &[]),
            m("toollib", "toollib", &[], &[], &[]),
            m("toolkit", "toolkit", &[], &[], &[]),
            m("other", "other", &[], &[], &[]),
        ]
    }

    const PATHS: [&str; 11] = ["app", "libs", "libs/lib", "base", "kit", "tool", "toollib", "toolkit", "other", "Cargo.lock", "docs"];

    fn ls_tree(changed: &str) -> String {
        PATHS
            .iter()
            .map(|p| {
                let kind = if *p == "Cargo.lock" { "100644 blob" } else { "040000 tree" };
                let oid = if *p == changed { "new" } else { "old" };
                format!("{kind} {oid}-{p}\t{p}\n")
            })
            .collect()
    }

    fn hash_of(krate: &str, changed: &str) -> String {
        input_hashes(&ws(), &ls_tree(changed))[krate].clone()
    }

    #[test_id(tbvcp4u)]
    #[test]
    fn a_change_to_anything_the_crate_builds_from_changes_its_hash() {
        let before = hash_of("app", "");
        for dir in ["app", "libs/lib", "base", "kit", "tool", "toollib", "Cargo.lock"] {
            assert_ne!(hash_of("app", dir), before, "{dir}");
        }
    }

    #[test_id(t12i4cu)]
    #[test]
    fn a_change_outside_the_closure_leaves_the_hash_equal() {
        let before = hash_of("app", "");
        for dir in ["other", "docs", "toolkit"] {
            assert_eq!(hash_of("app", dir), before, "{dir}");
        }
        assert_ne!(hash_of("other", "other"), hash_of("other", ""), "positive control: the change is visible to its own crate");
    }

    #[test_id(tfgoqvb)]
    #[test]
    fn the_same_inputs_hash_the_same_and_every_member_has_a_hash() {
        let a = input_hashes(&ws(), &ls_tree(""));
        assert_eq!(a, input_hashes(&ws(), &ls_tree("")));
        assert_eq!(a.len(), ws().len());
        assert!(a.values().all(|h| h.len() == 64), "{a:?}");
    }
}
