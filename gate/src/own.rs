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

pub const BUILD_TIER: &str = "build";

/// Whether the declaration has a `[tier.build]`: the repository's compile, run by the gate as an
/// uncharged `build` phase so the capped `gate` phase times only its tests.
pub fn build_tier(tree: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(tree.join(DECL)) else { return false };
    let Ok(doc) = spira_config::parse_foreign::<toml::Value>(&text) else { return false };
    declares_own_testenv(tree) && doc.get("tier").and_then(|t| t.get(BUILD_TIER)).is_some()
}

/// The suite names testenv would accept for `tree`: a repository's declared tiers, else the
/// files of the harness's own `spira/` directory. `Err` when the declaration cannot be read.
pub fn known_suites(tree: &Path) -> Result<Vec<String>, String> {
    if declares_own_testenv(tree) {
        let text = std::fs::read_to_string(tree.join(DECL)).map_err(|e| format!("{DECL}: {e}"))?;
        let doc: toml::Value = spira_config::parse_foreign(&text).map_err(|e| format!("{DECL}: {e}"))?;
        return Ok(doc.get("tier").and_then(|t| t.as_table()).map(|t| t.keys().filter(|k| *k != BUILD_TIER).cloned().collect()).unwrap_or_default());
    }
    let mut names: Vec<String> = std::fs::read_dir(tree.join("spira"))
        .map(|d| d.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with("test-") && n.ends_with(".sh")).collect())
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

/// The tiers a bare testenv run of `tree` runs: the declaration's non-empty `default`, else every
/// declared tier but `build`. `Err` when the declaration cannot be read.
pub fn default_tiers(tree: &Path) -> Result<Vec<String>, String> {
    if declares_own_testenv(tree) {
        let text = std::fs::read_to_string(tree.join(DECL)).map_err(|e| format!("{DECL}: {e}"))?;
        let doc: toml::Value = spira_config::parse_foreign(&text).map_err(|e| format!("{DECL}: {e}"))?;
        let declared: Vec<String> = doc.get("default").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        if !declared.is_empty() {
            return Ok(declared);
        }
    }
    known_suites(tree)
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

    #[test_id(tictz8l)]
    #[test]
    fn build_tier_is_declared_by_a_build_tier_and_is_not_a_suite() {
        let d = testkit::TempDir::new("gate-own-build");
        fs::create_dir_all(d.join(DECL).parent().unwrap()).unwrap();
        fs::write(d.join(DECL), "[tier.unit]\ncmd = \"x\"\n[tier.build]\ncmd = \"y\"\n").unwrap();
        assert!(build_tier(&d));
        assert_eq!(known_suites(&d).unwrap(), vec!["unit".to_string()]);
        fs::write(d.join(DECL), "[tier.unit]\ncmd = \"x\"\n").unwrap();
        assert!(!build_tier(&d));
    }

    #[test_id(tngynt5)]
    #[test]
    fn a_tree_with_only_a_declaration_judges_itself() {
        assert!(declares_own_testenv(&tree(&[DECL])));
    }

    #[test_id(tttuwss)]
    #[test]
    fn the_harness_image_makes_it_the_harness() {
        assert!(!declares_own_testenv(&tree(&[DECL, HARNESS_IMAGE, "x"])));
    }

    #[test_id(t83y4vk)]
    #[test]
    fn an_undeclared_tree_is_not_its_own() {
        assert!(!declares_own_testenv(&tree(&["a", "b"])));
    }

    fn declared(body: &str) -> testkit::TempDir {
        let d = testkit::TempDir::new("gate-own-default");
        fs::create_dir_all(d.join(DECL).parent().unwrap()).unwrap();
        fs::write(d.join(DECL), body).unwrap();
        d
    }

    #[test_id(t07irkl)]
    #[test]
    fn a_declared_default_is_the_default_tiers() {
        let d = declared("default = [\"gate\"]\n[tier.gate]\ncmd = \"x\"\n[tier.other]\ncmd = \"y\"\n[tier.build]\ncmd = \"z\"\n");
        assert_eq!(default_tiers(&d).unwrap(), vec!["gate".to_string()]);
    }

    #[test_id(tyx7vsd)]
    #[test]
    fn without_a_default_the_default_tiers_are_the_declared_tiers_but_build() {
        let d = declared("[tier.gate]\ncmd = \"x\"\n[tier.build]\ncmd = \"z\"\n");
        assert_eq!(default_tiers(&d).unwrap(), vec!["gate".to_string()]);
    }
}
