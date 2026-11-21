//! The key a branch name is filed under in `run/` counters, and the removal of its
//! per-branch no-verdict files.

use std::path::Path;

/// `printf '%s' "$br" | tr -c 'A-Za-z0-9._-' '-'`.
pub fn branch_key(br: &str) -> String {
    br.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' })
        .collect()
}

/// Removes `<run>/noverdict/<key>-*` for `branch`; `-` closes the key, so a branch whose
/// name merely extends this one keeps its counters. Returns the names removed.
pub fn clear_noverdict(run: &Path, branch: &str) -> Vec<String> {
    let prefix = format!("{}-", branch_key(branch));
    let mut removed = Vec::new();
    if let Ok(rd) = std::fs::read_dir(run.join("noverdict")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && std::fs::remove_file(e.path()).is_ok() {
                removed.push(name);
            }
        }
    }
    removed.sort();
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tgqxoww)]
    #[test]
    fn clearing_a_branch_keeps_the_counters_of_a_branch_that_extends_its_name() {
        let t = testkit::TempDir::new("branch-key");
        let run = t.path().to_path_buf();
        let dir = run.join("noverdict");
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["spira-sp-a-conflict", "spira-sp-a-conflict.asked", "spira-sp-ab-conflict"] {
            std::fs::write(dir.join(f), "1").unwrap();
        }
        let gone = clear_noverdict(&run, "spira/sp-a");
        assert_eq!(gone, ["spira-sp-a-conflict", "spira-sp-a-conflict.asked"]);
        assert!(dir.join("spira-sp-ab-conflict").exists());
    }

    #[test_id(te6r4zo)]
    #[test]
    fn key_replaces_path_separators() {
        assert_eq!(branch_key("spira/sp-a.1"), "spira-sp-a.1");
    }
}
