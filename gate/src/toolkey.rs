//! Tools keyed by their source closure (DESIGN.md "Tools keyed by their source closure").
//!
//! A gate tree's `bin` tools (gate.steps) used to be reused only by a later trial of the very
//! same git tree id (sp-g9f3t), so every new branch rebuilt all of them from an empty target
//! directory — 20-35 s on a quiet host, 100-300 s under load, of a 300 s gate. What a tool's
//! binary depends on is far smaller than the tree: the workspace packages in the dependency
//! closure of the `bin` packages (normal and build dependencies, not dev), the workspace-wide
//! build inputs (root `Cargo.toml`, `Cargo.lock`, the toolchain pin, `.cargo/`), the build
//! recipe (profile and packages) — and any file a build script or an `include_*!` reads from
//! outside those packages (spira-config's build script reads `spira/conf.d`).
//!
//! The key has two parts. The BASE key hashes the recipe, the root inputs' git object ids and
//! the closure packages' git tree ids, all read from the tree under test — known before any
//! build. The INPUTS manifest names every other in-tree file the last build of those tools
//! actually read (cargo's dep-info and the build scripts' `rerun-if-changed`), each with its
//! object id; it is only known after a build, so it is stored beside the binaries and checked
//! against the tree under test before they are reused. An entry is `<base>-<hash(manifest)>`
//! under the repository's store; a hit needs the base key AND every manifest line to match.
//! Anything that cannot be computed is a miss — the tools are then built from the tree, as
//! before — never a guess.

use crate::compose::Member;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// `<run>/<STORE_DIR>/<repo>/` holds the entries.
pub const STORE_DIR: &str = "gate-tools";
/// An entry's manifest of out-of-closure inputs: `<path>\t<object id or ->` per line.
pub const INPUTS: &str = "INPUTS";
/// An entry's stamp: its own name, written last before the atomic rename.
pub const KEY: &str = "KEY";
/// Entries kept per repository (least recently used beyond this are removed on publish).
pub const KEEP: usize = 8;
/// Workspace-wide build inputs: a change to any of them changes every tool's key.
pub const ROOT_INPUTS: [&str; 5] = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rust-toolchain", ".cargo"];
const SCHEMA: &str = "gate-tools v1";

/// What a trial needs to look up and publish its tools in the shared store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shared {
    /// `<run>/gate-tools/<repo>`.
    pub store: PathBuf,
    /// The base key (hex sha256).
    pub base: String,
    /// The closure packages' directories, relative to the tree root.
    pub dirs: Vec<String>,
}

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

/// The base key: the recipe, then `root <path> <oid>` and `pkg <name> <dir> <tree id>` lines
/// in a fixed order. Every caller passes the ids read from the tree under test.
pub fn base_key(recipe: &str, roots: &[(String, String)], pkgs: &[(String, String, String)]) -> String {
    let mut s = format!("{SCHEMA}\nrecipe {recipe}\n");
    let mut roots = roots.to_vec();
    roots.sort();
    for (p, id) in roots {
        s.push_str(&format!("root {p} {id}\n"));
    }
    let mut pkgs = pkgs.to_vec();
    pkgs.sort();
    for (n, d, id) in pkgs {
        s.push_str(&format!("pkg {n} {d} {id}\n"));
    }
    hex(s.as_bytes())
}

fn hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// `rel` lies inside one of `dirs` (or is one), or is a root input: the base key covers it.
pub fn covered(rel: &str, dirs: &[String]) -> bool {
    ROOT_INPUTS.iter().any(|r| rel == *r || rel.starts_with(&format!("{r}/")))
        || dirs.iter().any(|d| d.is_empty() || rel == d || rel.starts_with(&format!("{d}/")))
}

/// The prerequisites of a cargo dep-info file (`<target>: <dep> <dep> …`, spaces in a path
/// escaped as `\ `), in order.
pub fn dep_info_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        // The target's own path ends at the first unescaped `: `.
        let Some(i) = find_unescaped(line, ": ").or_else(|| line.strip_suffix(':').map(|l| l.len())) else {
            continue;
        };
        let rest = line.get(i + 1..).unwrap_or("");
        let mut cur = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => {
                    cur.push(' ');
                    chars.next();
                }
                ' ' => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

fn find_unescaped(s: &str, pat: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = s[from..].find(pat) {
        let at = from + i;
        if at == 0 || s.as_bytes()[at - 1] != b'\\' {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// The paths a build script's `output` names with `cargo:rerun-if-changed=` (or `cargo::`).
pub fn rerun_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("cargo:rerun-if-changed=").or_else(|| l.strip_prefix("cargo::rerun-if-changed=")))
        .map(str::to_string)
        .collect()
}

/// `abs` relative to `tree`, `..` and `.` resolved lexically; None when it lies outside the
/// tree (a registry crate — `Cargo.lock` covers it), or under its `target/` (build output,
/// derived from the inputs that are named).
pub fn relative(tree: &Path, abs: &str) -> Option<String> {
    let p = Path::new(abs);
    let p = if p.is_absolute() { p.to_path_buf() } else { tree.join(p) };
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    let norm = PathBuf::from(format!("/{}", parts.join("/")));
    let rel = norm.strip_prefix(tree).ok()?.to_string_lossy().into_owned();
    if rel.is_empty() || rel == "target" || rel.starts_with("target/") {
        return None;
    }
    Some(rel)
}

/// The manifest text: `<path>\t<id>` lines, sorted, one per path.
pub fn manifest(inputs: &[(String, String)]) -> String {
    let set: BTreeSet<&(String, String)> = inputs.iter().collect();
    set.into_iter().map(|(p, id)| format!("{p}\t{id}\n")).collect()
}

/// Parse [`manifest`] text; None for any line that is not `<path>\t<id>`.
pub fn parse_manifest(text: &str) -> Option<Vec<(String, String)>> {
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (p, id) = l.split_once('\t')?;
            (!p.is_empty() && !id.is_empty() && !id.contains('\t')).then(|| (p.to_string(), id.to_string()))
        })
        .collect()
}

/// An entry's name: the base key and a hash of its manifest.
pub fn entry_name(base: &str, manifest: &str) -> String {
    format!("{base}-{}", &hex(manifest.as_bytes())[..16])
}
