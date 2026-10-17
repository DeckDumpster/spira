//! Which corpus judges a tree: the harness's `spira/test-*.sh` suites on the round VM, or the
//! suites a registered repository declares for itself in `.spira/testenv.toml`.

use std::path::Path;

pub const DECL: &str = ".spira/testenv.toml";
pub const HARNESS_IMAGE: &str = "spira/testenv/Containerfile";

pub fn declares_own_testenv(tree: &Path) -> bool {
    tree.join(DECL).is_file() && !tree.join(HARNESS_IMAGE).exists()
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
        assert!(declares_own_testenv(&*tree(&[DECL])));
    }

    #[test]
    fn the_harness_image_makes_it_the_harness() {
        assert!(!declares_own_testenv(&*tree(&[DECL, HARNESS_IMAGE, "x"])));
    }

    #[test]
    fn an_undeclared_tree_is_not_its_own() {
        assert!(!declares_own_testenv(&*tree(&["a", "b"])));
    }
}
