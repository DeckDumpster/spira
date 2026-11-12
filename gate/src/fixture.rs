//! `gate fixture-config --tree <dir> [--base <rev>]`: the tree's complete test fixture config
//! must resolve against every key the tree's registry reads, and against the registry the
//! landing ref (`--base`) reads — a round runs the code in force against the tree's fixture,
//! so a key the landing ref still requires cannot leave it. A refusal names the key, instead
//! of surfacing as a harness fault when a round computes its image.

use spira_config::resolve::{default_conf_d, resolve, ResolveInput};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

pub const FIXTURE: &str = "spira/testlib/complete.toml";
pub const FENCE: &str = "fixture-config";

/// The number of registry keys the fixture resolved, or the refusal naming the key it cannot.
pub fn check(tree: &Path, base: Option<&str>) -> Result<usize, String> {
    let fixture = tree.join(FIXTURE);
    let doc = spira_config::load(&fixture).map_err(|e| format!("{FIXTURE}: {e}"))?;
    let home = tree.join("spira");
    let env = BTreeMap::new();
    let resolve_against = |conf_d: &Path, what: &str| {
        resolve(ResolveInput { env: &env, home: &home, repo: tree, toml: Some(&doc), conf_d })
            .map_err(|e| format!("{FIXTURE} does not resolve against {what}: {e}"))
    };
    let resolved = resolve_against(&default_conf_d(&home), "this tree's config keys")?;
    if let Some(base) = base {
        let scratch = std::env::temp_dir().join(format!("gate-fixture-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).map_err(|e| format!("scratch for the landing ref's keys: {e}"))?;
        let archive = Command::new("timeout").args(["5", "git", "-C"]).arg(tree).args(["archive", base, "spira/conf.d"]).output()
            .map_err(|e| format!("git archive {base}: {e}"))?;
        if !archive.status.success() {
            return Err(format!("git archive {base} spira/conf.d: {}", String::from_utf8_lossy(&archive.stderr).trim()));
        }
        let mut tar = Command::new("timeout").args(["5", "tar", "-x"]).arg("-C").arg(&scratch).stdin(std::process::Stdio::piped()).spawn()
            .map_err(|e| format!("tar: {e}"))?;
        std::io::Write::write_all(&mut tar.stdin.take().unwrap(), &archive.stdout).map_err(|e| format!("tar: {e}"))?;
        let unpacked = tar.wait().map_err(|e| format!("tar: {e}"))?.success();
        let landed = unpacked.then(|| resolve_against(&scratch.join("spira/conf.d"), &format!("the landing ref {base}'s config keys (a round runs that code against this fixture)")));
        let _ = std::fs::remove_dir_all(&scratch);
        landed.unwrap_or_else(|| Err(format!("could not unpack the landing ref {base}'s config keys")))?;
    }
    Ok(resolved.values.len())
}

pub fn main(args: &[String]) -> i32 {
    let (tree, base) = match args {
        [flag, tree] if flag == "--tree" => (tree, None),
        [f1, tree, f2, base] if f1 == "--tree" && f2 == "--base" => (tree, Some(base.as_str())),
        _ => {
            eprintln!("usage: gate fixture-config --tree <dir> [--base <rev>]");
            return 2;
        }
    };
    match check(Path::new(tree), base) {
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
        assert!(check(t.path(), None).unwrap() > 0);
    }

    #[test]
    fn a_tip_that_retires_a_fixture_key_the_tree_still_reads_is_refused_naming_it() {
        let t = copy_tree();
        let f = t.path().join(FIXTURE);
        let text = std::fs::read_to_string(&f).unwrap();
        let kept: String = text.lines().filter(|l| !l.starts_with("archivist_every ")).map(|l| format!("{l}\n")).collect();
        assert_ne!(text.len(), kept.len(), "the planted offender must exist");
        std::fs::write(&f, kept).unwrap();
        let e = check(t.path(), None).unwrap_err();
        assert!(e.contains("SPIRA_ARCHIVIST_EVERY"), "{e}");
    }

    #[test]
    fn a_tip_that_retires_a_fixture_key_the_landing_ref_still_requires_is_refused_naming_it() {
        let t = copy_tree();
        testkit::git(t.path(), &["init", "-q", "-b", "main"]);
        testkit::git(t.path(), &["add", "-A"]);
        testkit::git(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "landing ref"]);
        let key = t.path().join("spira/conf.d/SPIRA_ARCHIVIST_EVERY");
        let f = t.path().join(FIXTURE);
        let text = std::fs::read_to_string(&f).unwrap();
        let kept: String = text.lines().filter(|l| !l.starts_with("archivist_every ")).map(|l| format!("{l}\n")).collect();
        assert_ne!(text.len(), kept.len(), "the planted offender must exist");
        std::fs::write(&f, kept).unwrap();
        std::fs::remove_file(&key).unwrap();
        check(t.path(), None).expect("the tip alone is consistent: it retired the key from both");
        let e = check(t.path(), Some("main")).unwrap_err();
        assert!(e.contains("SPIRA_ARCHIVIST_EVERY") && e.contains("landing ref"), "{e}");
    }
}
