//! Which paths a gate tool is built from (DESIGN.md "Tools from the active release").
//!
//! A gate run uses the active release's tools unless the tree under test changes the tools' own
//! source. What a tool's binary depends on is the workspace packages in the dependency closure of
//! the `bin` packages (normal and build dependencies, not dev) and the workspace-wide build
//! inputs: root `Cargo.toml`, `Cargo.lock`, the toolchain pin, `.cargo/`.

use crate::compose::Member;
use std::collections::BTreeSet;

/// Workspace-wide build inputs: a change to any of them changes every tool.
pub const ROOT_INPUTS: [&str; 5] = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rust-toolchain", ".cargo"];

/// The workspace members the `pkgs` binaries are built from: each package and, transitively,
/// its normal and build dependencies. Err names a package that is not a workspace member.
pub fn closure<'m>(members: &'m [Member], pkgs: &[String]) -> Result<Vec<&'m Member>, String> {
    let mut seen = BTreeSet::new();
    let mut todo: Vec<String> = pkgs.to_vec();
    while let Some(n) = todo.pop() {
        if seen.contains(&n) {
            continue;
        }
        let m = members
            .iter()
            .find(|m| m.name == n)
            .ok_or_else(|| format!("{n} is not a workspace member"))?;
        todo.extend(m.build_deps.iter().cloned());
        seen.insert(n);
    }
    Ok(members.iter().filter(|m| seen.contains(&m.name)).collect())
}

/// `rel` lies inside one of `dirs` (or is one), or is a root input.
pub fn covered(rel: &str, dirs: &[String]) -> bool {
    ROOT_INPUTS.iter().any(|r| rel == *r || rel.starts_with(&format!("{r}/")))
        || dirs.iter().any(|d| d.is_empty() || rel == d || rel.starts_with(&format!("{d}/")))
}

/// The first of `changed` that a tool's build reads, if any.
pub fn first_touched<'c>(changed: &'c [String], dirs: &[String]) -> Option<&'c str> {
    changed.iter().map(String::as_str).find(|c| covered(c, dirs))
}
