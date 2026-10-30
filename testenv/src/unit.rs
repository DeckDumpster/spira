//! `testenv --suites unit-tests <tip> <repo>`: the Rust unit tests of the crates a tip's diff
//! affects, run in the fixture container by `spira/unit-tests.sh`. The host computes the crate
//! list (the container has no history) and leaves it in the tree for the script.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The name callers use; the script that runs it is [`SCRIPT`].
pub const NAME: &str = "unit-tests";
pub const SCRIPT: &str = "unit-tests.sh";
pub const CRATES_REL: &str = "target/unit-tests.crates";
pub const BINS_REL: &str = "target/unit-tests.bins";

pub fn resolve(list: &str) -> String {
    list.split(',').map(|n| if n.trim() == NAME { SCRIPT } else { n }).collect::<Vec<_>>().join(",")
}

pub fn selected(names: &[String]) -> bool {
    names.iter().any(|n| n == SCRIPT)
}

pub fn crates_path(tree: &Path) -> PathBuf {
    tree.join(CRATES_REL)
}

pub fn bins_path(tree: &Path) -> PathBuf {
    tree.join(BINS_REL)
}

/// Workspace crates whose binaries the selected crates' tests launch, declared as
/// `[package.metadata.spira] test-bins = [..]`; sorted, each once.
pub fn test_bins(metadata_json: &str, crates: &[String]) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(metadata_json).map_err(|e| format!("cargo metadata: {e}"))?;
    let mut out = std::collections::BTreeSet::new();
    for p in v["packages"].as_array().into_iter().flatten() {
        if !p["name"].as_str().is_some_and(|n| crates.iter().any(|c| c == n)) {
            continue;
        }
        let name = p["name"].as_str().unwrap_or_default();
        match &p["metadata"]["spira"]["test-bins"] {
            serde_json::Value::Null => {}
            serde_json::Value::Array(a) => {
                for b in a {
                    let b = b.as_str().ok_or_else(|| format!("{name}: test-bins must be strings"))?;
                    if !v["packages"].as_array().into_iter().flatten().any(|q| q["name"] == b) {
                        return Err(format!("{name}: test-bins names {b}, which is not a workspace crate"));
                    }
                    out.insert(b.to_string());
                }
            }
            _ => return Err(format!("{name}: test-bins must be an array")),
        }
    }
    Ok(out.into_iter().collect())
}

fn output(cmd: &mut Command) -> Result<Vec<u8>, String> {
    // batch-job: part of a testenv trial, bounded by the trial deadline
    let o = cmd.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(o.stdout)
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// Writes the affected crates (touched plus every transitive dependent), one per line; an
/// empty file means the diff touches no crate.
pub fn write_crates(repo: &Path, base: &str, rev: &str, tree: &Path) -> Result<Vec<String>, String> {
    let diff = output(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["diff", "--raw", "-z", "--no-renames", &format!("{base}...{rev}")])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE"),
    )
    .map_err(|e| format!("git diff {base}...{rev}: {e}"))?;
    let meta = output(Command::new("cargo").current_dir(tree).args(["metadata", "--format-version", "1", "--no-deps", "--offline"]))
        .map_err(|e| format!("cargo metadata: {e}"))?;
    let crates = gate::compose::select_crates(&diff, &String::from_utf8_lossy(&meta), true)?;
    let path = crates_path(tree);
    let body: String = crates.iter().map(|c| format!("{c}\n")).collect();
    std::fs::create_dir_all(path.parent().unwrap_or(tree))
        .and_then(|_| std::fs::write(&path, body))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let bins: String = test_bins(&String::from_utf8_lossy(&meta), &crates)?.iter().map(|c| format!("{c}\n")).collect();
    let path = bins_path(tree);
    std::fs::write(&path, bins).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(crates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caller_name_resolves_to_the_script_and_other_names_pass_through() {
        assert_eq!(resolve("unit-tests"), "unit-tests.sh");
        assert_eq!(resolve("test-a.sh, unit-tests"), "test-a.sh,unit-tests.sh");
        assert_eq!(resolve("test-a.sh"), "test-a.sh");
        assert!(selected(&["unit-tests.sh".to_string()]));
        assert!(!selected(&["test-a.sh".to_string()]));
    }

    const META: &str = r#"{"packages":[
        {"name":"rt","metadata":{"spira":{"test-bins":["lc"]}}},
        {"name":"lc","metadata":null},
        {"name":"plain","metadata":null},
        {"name":"bad","metadata":{"spira":{"test-bins":["ghost"]}}}]}"#;

    #[test]
    fn a_selected_crate_declaring_test_bins_names_them_and_one_declaring_nothing_adds_none() {
        assert_eq!(test_bins(META, &["rt".into(), "plain".into()]).unwrap(), ["lc"]);
        assert!(test_bins(META, &["plain".into()]).unwrap().is_empty());
        assert!(test_bins(META, &["lc".into()]).unwrap().is_empty());
    }

    #[test]
    fn a_test_bin_that_is_not_a_workspace_crate_is_refused() {
        assert!(test_bins(META, &["bad".into()]).unwrap_err().contains("ghost"));
    }

    #[test]
    fn the_spira_runtime_manifest_declares_spira_lc_and_the_crates_that_launch_nothing_declare_none() {
        let meta = output(Command::new("cargo").current_dir(env!("CARGO_MANIFEST_DIR")).args(["metadata", "--format-version", "1", "--no-deps", "--offline"])).unwrap();
        let meta = String::from_utf8_lossy(&meta);
        assert_eq!(test_bins(&meta, &["spira-runtime".into()]).unwrap(), ["spira-lc"]);
        assert!(test_bins(&meta, &["testenv".into()]).unwrap().is_empty());
    }
}
