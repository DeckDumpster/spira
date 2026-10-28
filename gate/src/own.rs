//! Which corpus judges a tree: the harness's `spira/test-*.sh` suites on the round VM, or the
//! suites a registered repository declares for itself in `.spira/testenv.toml`.

use std::path::Path;

pub const DECL: &str = ".spira/testenv.toml";
pub const HARNESS_IMAGE: &str = "spira/testenv/Containerfile";

pub fn declares_own_testenv(tree: &Path) -> bool {
    tree.join(DECL).is_file() && !tree.join(HARNESS_IMAGE).exists()
}

/// The command a registered repository declares as its own full CI entry point (`ci_entry` in
/// the declaration): what a round VM runs so the round covers what CI covers.
pub fn ci_entry(tree: &Path) -> Option<String> {
    let text = std::fs::read_to_string(tree.join(DECL)).ok()?;
    let doc: toml::Value = spira_config::parse_foreign(&text).ok()?;
    doc.get("ci_entry")?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(String::from)
}

/// The suite names testenv would accept for `tree`: a repository's declared tiers, else the
/// files of the harness's own `spira/` directory. `Err` when the declaration cannot be read.
pub fn known_suites(tree: &Path) -> Result<Vec<String>, String> {
    if declares_own_testenv(tree) {
        let text = std::fs::read_to_string(tree.join(DECL)).map_err(|e| format!("{DECL}: {e}"))?;
        let doc: toml::Value = spira_config::parse_foreign(&text).map_err(|e| format!("{DECL}: {e}"))?;
        return Ok(doc.get("tier").and_then(|t| t.as_table()).map(|t| t.keys().cloned().collect()).unwrap_or_default());
    }
    let mut names: Vec<String> = std::fs::read_dir(tree.join("spira"))
        .map(|d| d.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with("test-") && n.ends_with(".sh")).collect())
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree(files: &[&str]) -> testkit::TempDir {
        let d = testkit::TempDir::new("gate-own");
        for f in files {
            fs::create_dir_all(d.join(f).parent().unwrap()).unwrap();
            fs::write(d.join(f), "").unwrap();
        }
        d
    }

    #[test]
    fn a_tree_with_only_a_declaration_judges_itself() {
        assert!(declares_own_testenv(&tree(&[DECL])));
    }

    #[test]
    fn the_harness_image_makes_it_the_harness() {
        assert!(!declares_own_testenv(&tree(&[DECL, HARNESS_IMAGE, "x"])));
    }

    #[test]
    fn ci_entry_is_the_declared_top_level_command_and_absent_when_undeclared() {
        let d = tree(&[DECL]);
        assert_eq!(ci_entry(&d), None);
        fs::write(d.join(DECL), "ci_entry = \" deploy/ci.sh all \"\n[tier.a]\n").unwrap();
        assert_eq!(ci_entry(&d).as_deref(), Some("deploy/ci.sh all"));
        fs::write(d.join(DECL), "ci_entry = \"\"\n").unwrap();
        assert_eq!(ci_entry(&d), None);
    }

    #[test]
    fn an_undeclared_tree_is_not_its_own() {
        assert!(!declares_own_testenv(&tree(&["a", "b"])));
    }
}
