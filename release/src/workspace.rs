//! The workspace's own `[[bin]]` targets — the one reader. `release build` copies exactly
//! these into `bin/`; `queue`'s deploy checks a round's build against the same list.

use std::fs;
use std::path::Path;

/// Every binary name the workspace's own `[[bin]]` targets declare, read from
/// `<worktree>/Cargo.toml`'s `[workspace] members` and each member's own `Cargo.toml`:
/// its explicit `[[bin]]` tables, or (when it has none) the package name when
/// `src/main.rs` exists (cargo's own default). THE SOURCE OF TRUTH, not the Makefile's
/// `install` list — that list is hand-maintained prose and had already drifted as of
/// 2026-09-29 (missing reconciler-alert, lifecycle-guard, spira-lc and test-plan, all
/// already built and already running). A list that must be remembered and edited by hand
/// for every new binary crate is exactly the failure mode this bead exists to close: the
/// workspace's own manifests cannot go stale relative to what cargo actually builds.
pub fn expected_bins(worktree: &Path) -> Result<Vec<String>, String> {
    let root_toml = worktree.join("Cargo.toml");
    let root = read_toml(&root_toml)?;
    let members = root
        .get("workspace")
        .and_then(|v| v.get("members"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("{} has no [workspace].members array", root_toml.display()))?;
    let mut names = Vec::new();
    for m in members {
        let rel = m.as_str().ok_or_else(|| format!("{} has a non-string workspace member", root_toml.display()))?;
        let dir = worktree.join(rel);
        let manifest = dir.join("Cargo.toml");
        let doc = read_toml(&manifest)?;
        let bins = doc.get("bin").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if bins.is_empty() {
            if dir.join("src").join("main.rs").is_file() {
                let name = doc
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| format!("{} has src/main.rs but no [package].name", manifest.display()))?;
                names.push(name.to_string());
            }
        } else {
            for b in &bins {
                let name = b.get("name").and_then(|n| n.as_str()).ok_or_else(|| format!("{} has a [[bin]] with no name", manifest.display()))?;
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn read_toml(path: &Path) -> Result<toml::Value, String> {
    let src = fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    src.parse::<toml::Value>().map_err(|e| format!("cannot parse {}: {e}", path.display()))
}
