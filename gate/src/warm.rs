//! Cargo fingerprints are workspace-relative, so mtimes are the only obstacle to building a tree
//! in a directory another tree of nearly the same bytes built: [`normalize_mtimes`] ages the
//! tracked files that did not change.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

const AGED: Duration = Duration::from_secs(1_000_000_000);

/// Age every tracked file of `tree` that is unchanged from `built_from` to HEAD, and every
/// directory none of whose changes (or a descendant's) is in that diff: a build script's
/// `rerun-if-changed` on a directory reads the directory's own mtime too.
pub fn normalize_mtimes(tree: &Path, built_from: &str) -> Result<usize, String> {
    let git = |args: &[&str]| -> Result<Vec<u8>, String> {
        let o = Command::new("git").arg("-C").arg(tree).args(args).output() // batch-job: git over the gate tree, under the gate's own cap
            .map_err(|e| format!("git: {e}"))?;
        if o.status.success() { Ok(o.stdout) } else { Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim())) }
    };
    let changed = git(&["diff", "--name-only", "-z", "--no-renames", built_from, "HEAD"])?;
    let changed: std::collections::HashSet<&[u8]> = changed.split(|b| *b == 0).collect();
    let tracked = git(&["ls-files", "-z"])?;
    let old = UNIX_EPOCH + AGED;
    let mut aged = 0;
    let ancestors = |rel: &[u8]| -> Vec<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(rel)).ancestors().skip(1).filter(|a| !a.as_os_str().is_empty()).map(Path::to_path_buf).collect()
    };
    let changed_dirs: std::collections::HashSet<PathBuf> =
        changed.iter().filter(|r| !r.is_empty()).flat_map(|r| ancestors(r)).collect();
    let mut dirs = std::collections::BTreeSet::new();
    for rel in tracked.split(|b| *b == 0).filter(|r| !r.is_empty() && !changed.contains(r)) {
        use std::os::unix::ffi::OsStrExt;
        let p = tree.join(std::ffi::OsStr::from_bytes(rel));
        if !fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let f = fs::File::options().read(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.set_modified(old).map_err(|e| format!("{}: {e}", p.display()))?;
        aged += 1;
        dirs.extend(ancestors(rel).into_iter().filter(|d| !changed_dirs.contains(d)));
    }
    for d in dirs {
        let p = tree.join(&d);
        let f = fs::File::options().read(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.set_modified(old).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(aged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }
    fn mtime(p: &Path) -> SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }

    #[test]
    fn only_the_files_that_differ_from_the_built_commit_stay_new() {
        let t = testkit::TempDir::new("gate-warm-mtime");
        let tree = t.path();
        git(tree, &["init", "-q"]);
        fs::create_dir_all(tree.join("crate/src")).unwrap();
        fs::write(tree.join("crate/src/lib.rs"), "1").unwrap();
        fs::write(tree.join("crate/src/other.rs"), "1").unwrap();
        git(tree, &["add", "."]);
        git(tree, &["commit", "-qm", "base"]);
        let base = git(tree, &["rev-parse", "HEAD"]);
        fs::write(tree.join("crate/src/lib.rs"), "2").unwrap();
        git(tree, &["commit", "-qam", "change"]);
        let n = normalize_mtimes(tree, &base).unwrap();
        assert_eq!(n, 1);
        assert!(mtime(&tree.join("crate/src/other.rs")) < SystemTime::now() - Duration::from_secs(3600));
        assert!(mtime(&tree.join("crate/src/lib.rs")) > SystemTime::now() - Duration::from_secs(3600));
    }

    #[test]
    fn a_directory_holding_only_unchanged_files_is_aged_with_them() {
        let t = testkit::TempDir::new("gate-warm-dirs");
        let tree = t.path();
        git(tree, &["init", "-q"]);
        for d in ["crate/rules", "crate/src"] {
            fs::create_dir_all(tree.join(d)).unwrap();
            fs::write(tree.join(d).join("a.rs"), "1").unwrap();
        }
        git(tree, &["add", "."]);
        git(tree, &["commit", "-qm", "base"]);
        let base = git(tree, &["rev-parse", "HEAD"]);
        fs::write(tree.join("crate/src/b.rs"), "1").unwrap();
        git(tree, &["add", "."]);
        git(tree, &["commit", "-qm", "add"]);
        normalize_mtimes(tree, &base).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        assert!(mtime(&tree.join("crate/rules")) < old, "a rerun-if-changed directory must look as old as its files");
        assert!(mtime(&tree.join("crate/src")) > old, "a directory that gained a file changed");
    }

    /// A workspace crate built with different features is a different unit, so a donor built from
    /// a different `-p` selection leaves it, and everything above it, to rebuild. A dependent that
    /// asks a workspace crate for features makes the crate's own feature set depend on which
    /// dependents a gate selected.
    #[test]
    fn a_workspace_crates_features_do_not_depend_on_which_dependents_a_gate_selects() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
        let o = Command::new("cargo") // batch-job: reads the workspace manifest, no build
            .args(["metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path"])
            .arg(&manifest)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let meta: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        let pkgs = meta["packages"].as_array().unwrap();
        let names: Vec<&str> = pkgs.iter().map(|p| p["name"].as_str().unwrap()).collect();
        let mut asked: Vec<String> = pkgs
            .iter()
            .flat_map(|p| p["dependencies"].as_array().unwrap().iter().map(move |d| (p["name"].as_str().unwrap(), d)))
            .filter(|(_, d)| names.contains(&d["name"].as_str().unwrap()) && !d["features"].as_array().unwrap().is_empty())
            .map(|(from, d)| format!("{from} -> {}", d["name"].as_str().unwrap()))
            .collect();
        asked.sort();
        assert!(asked.is_empty(), "{asked:?}");
    }
}
