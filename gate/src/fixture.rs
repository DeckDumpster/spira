//! `gate fixture-config --tree <dir>`: the tree's complete test fixture config must resolve
//! against every key the tree's registry reads. A tip that retires a key from the fixture
//! while the merged tree still reads it is refused here, naming the key, instead of
//! surfacing as a harness fault when a round computes its image.

use spira_config::resolve::{default_conf_d, resolve, ResolveInput};
use std::collections::BTreeMap;
use std::path::Path;

pub const FIXTURE: &str = "spira/testlib/complete.toml";
pub const FENCE: &str = "fixture-config";

/// The number of registry keys the fixture resolved, or the refusal naming the key it cannot.
pub fn check(tree: &Path) -> Result<usize, String> {
    let fixture = tree.join(FIXTURE);
    let doc = spira_config::load(&fixture).map_err(|e| format!("{FIXTURE}: {e}"))?;
    let home = tree.join("spira");
    let env = BTreeMap::new();
    let resolved = resolve(ResolveInput { env: &env, home: &home, repo: tree, toml: Some(&doc), conf_d: &default_conf_d(&home) })
        .map_err(|e| format!("{FIXTURE} does not resolve against this tree's config keys: {e}"))?;
    Ok(resolved.values.len())
}

pub fn main(args: &[String]) -> i32 {
    let tree = match args {
        [flag, tree] if flag == "--tree" => tree,
        _ => {
            eprintln!("usage: gate fixture-config --tree <dir>");
            return 2;
        }
    };
    match check(Path::new(tree)) {
        Ok(n) => {
            println!("fence: {FENCE} checked {n} keys");
            0
        }
        Err(e) => {
            eprintln!("gate fixture-config: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_tree() -> testkit::TempDir {
        let t = testkit::TempDir::new("gate-fixture-config");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spira");
        let dst = t.path().join("spira");
        std::fs::create_dir_all(dst.join("testlib")).unwrap();
        std::fs::copy(src.join("testlib/complete.toml"), dst.join("testlib/complete.toml")).unwrap();
        std::fs::create_dir_all(dst.join("conf.d")).unwrap();
        for e in std::fs::read_dir(src.join("conf.d")).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), dst.join("conf.d").join(e.file_name())).unwrap();
        }
        t
    }

    #[test]
    fn the_shipped_fixture_resolves_every_key() {
        let t = copy_tree();
        assert!(check(t.path()).unwrap() > 0);
    }

    #[test]
    fn a_tip_that_retires_a_fixture_key_the_tree_still_reads_is_refused_naming_it() {
        let t = copy_tree();
        let f = t.path().join(FIXTURE);
        let text = std::fs::read_to_string(&f).unwrap();
        let kept: String = text.lines().filter(|l| !l.starts_with("archivist_every ")).map(|l| format!("{l}\n")).collect();
        assert_ne!(text.len(), kept.len(), "the planted offender must exist");
        std::fs::write(&f, kept).unwrap();
        let e = check(t.path()).unwrap_err();
        assert!(e.contains("SPIRA_ARCHIVIST_EVERY"), "{e}");
    }
}
