//! `testenv --suites unit-tests <tip> <repo>`: the Rust unit tests of the crates a tip's diff
//! affects, run in the fixture container by `spira/unit-tests.sh`. The host computes the crate
//! list (the container has no history) and leaves it in the tree for the script.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The name callers use; the script that runs it is [`SCRIPT`].
pub const NAME: &str = "unit-tests";
pub const SCRIPT: &str = "unit-tests.sh";
pub const CRATES_REL: &str = "target/unit-tests.crates";

pub fn resolve(list: &str) -> String {
    list.split(',').map(|n| if n.trim() == NAME { SCRIPT } else { n }).collect::<Vec<_>>().join(",")
}

pub fn selected(names: &[String]) -> bool {
    names.iter().any(|n| n == SCRIPT)
}

pub fn crates_path(tree: &Path) -> PathBuf {
    tree.join(CRATES_REL)
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
}
