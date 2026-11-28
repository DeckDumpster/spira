//! Removes what the retired `kit-bump` binary left in the run directory.

use std::path::{Path, PathBuf};

const RETIRED: [&str; 2] = ["kit-bump.conf", "kit-bump.state"];

/// The retired files that were present under `run` and are now gone.
pub fn remove_retired(run: &Path) -> Result<Vec<PathBuf>, String> {
    let mut removed = Vec::new();
    for p in RETIRED.map(|f| run.join(f)) {
        match std::fs::remove_file(&p) {
            Ok(()) => removed.push(p),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot remove {}: {e}", p.display())),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tmh2u6n)]
    #[test]
    fn the_retired_kit_bump_files_are_removed_and_nothing_else_is() {
        let d = testkit::TempDir::new("kit-bump-retired");
        let names = || {
            let mut n: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
            n.sort();
            n
        };
        for f in ["kit-bump.conf", "kit-bump.state", "kit-bump.log"] {
            std::fs::write(d.path().join(f), "x\n").unwrap();
        }
        assert_eq!(remove_retired(d.path()).unwrap().len(), 2);
        assert_eq!(names(), ["kit-bump.log"]);
        assert_eq!(remove_retired(d.path()).unwrap(), Vec::<PathBuf>::new());
        assert_eq!(names(), ["kit-bump.log"]);
    }
}
