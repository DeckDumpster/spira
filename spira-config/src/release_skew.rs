//! Is this process running from the release `current` names? A process born under an old
//! release that spawns successors or re-renders units keeps that release in force forever.
//! Both answers come from the filesystem alone: a release root's parent holds `current`.

use std::fs;
use std::path::{Path, PathBuf};

pub const CURRENT: &str = "current";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skew {
    pub own: PathBuf,
    pub current: PathBuf,
}

impl Skew {
    pub fn describe(&self) -> String {
        format!("running from release {} but current is {}", self.own.display(), self.current.display())
    }
}

/// `Some` only when `own_release` sits beside a `current` symlink that resolves elsewhere.
/// No `current`, or a root that does not resolve, is not skew: a checkout is not a release.
pub fn skew(own_release: &Path) -> Option<Skew> {
    let own = fs::canonicalize(own_release).ok()?;
    let current = fs::canonicalize(own.parent()?.join(CURRENT)).ok()?;
    (own != current).then_some(Skew { own, current })
}

/// `path` with every entry under `skew.own` moved under `skew.current`.
pub fn repoint_path(path: &str, skew: &Skew) -> String {
    path.replace(skew.own.to_string_lossy().as_ref(), skew.current.to_string_lossy().as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn releases() -> (testkit::TempDir, PathBuf) {
        let t = testkit::TempDir::new("release-skew");
        let d = t.path().to_path_buf();
        fs::create_dir_all(d.join("aaa")).unwrap();
        fs::create_dir_all(d.join("bbb")).unwrap();
        std::os::unix::fs::symlink("bbb", d.join(CURRENT)).unwrap();
        (t, d)
    }

    #[test]
    fn old_release_beside_a_newer_current_is_skewed() {
        let (_t, d) = releases();
        let s = skew(&d.join("aaa")).expect("aaa is not current");
        assert!(s.own.ends_with("aaa") && s.current.ends_with("bbb"));
        let p = repoint_path(&format!("{}/aaa/bin:/usr/bin", fs::canonicalize(&d).unwrap().display()), &s);
        assert!(p.contains("/bbb/bin") && !p.contains("/aaa/"));
    }

    #[test]
    fn the_current_release_and_a_non_release_are_not_skewed() {
        let (_t, d) = releases();
        assert_eq!(skew(&d.join("bbb")), None);
        assert_eq!(skew(&d.join("missing")), None);
        fs::remove_file(d.join(CURRENT)).unwrap();
        assert_eq!(skew(&d.join("aaa")), None);
    }
}
