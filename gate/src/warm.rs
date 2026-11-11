//! Warm unit builds: a finished gate hands its tmpfs `dwarf` build directory to a small pool
//! and the next gate adopts one. Cargo fingerprints are workspace-relative, so mtimes are the
//! only obstacle; [`normalize_mtimes`] ages unchanged tracked files. Anything uncertain
//! discards the directory: a cold build is slow, a wrongly warm one is a wrong verdict.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The pool's directory under the target root. No gate tree has this name.
pub const POOL: &str = "warm-dwarf";
/// Donated directories kept; older ones are removed as new ones arrive.
pub const KEEP: usize = 2;
const BUILT_FROM: &str = "built-from";
const BUILD_DIR: &str = "dwarf";
const AGED: Duration = Duration::from_secs(1_000_000_000);

fn entries(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<(SystemTime, PathBuf)> = fs::read_dir(root.join(POOL))
        .map(|rd| {
            rd.flatten()
                .filter(|e| !e.file_name().to_string_lossy().contains(".claimed"))
                .map(|e| (fs::metadata(e.path()).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH), e.path()))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

/// Bytes held by the pool, for the target root's cap.
pub fn size(root: &Path) -> u64 {
    crate::target::size_of(&root.join(POOL))
}

/// Remove every pooled directory (the root is short of room).
pub fn shed(root: &Path) {
    let _ = fs::remove_dir_all(root.join(POOL));
}

/// Move `dest` (a gate tree's finished `dwarf` directory) into the pool, recorded as built from
/// `commit`. A directory with no compiled dependencies, or no commit, is not worth keeping.
pub fn donate(root: &Path, dest: &Path, commit: &str) -> bool {
    if commit.is_empty() || !dest.join("deps").is_dir() {
        return false;
    }
    let pool = root.join(POOL);
    if fs::create_dir_all(&pool).is_err() {
        return false;
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let entry = pool.join(format!("{nanos}-{}", std::process::id()));
    if fs::create_dir(&entry).is_err() || fs::rename(dest, entry.join(BUILD_DIR)).is_err() {
        let _ = fs::remove_dir_all(&entry);
        return false;
    }
    if fs::write(entry.join(BUILT_FROM), commit).is_err() {
        let _ = fs::remove_dir_all(&entry);
        return false;
    }
    let all = entries(root);
    for old in all.iter().take(all.len().saturating_sub(KEEP)) {
        let _ = fs::remove_dir_all(old);
    }
    true
}

/// Fill the empty directory `dest` from the newest pooled one; the commit it was built from.
/// None when `dest` holds anything already or the pool has nothing to give. Claiming renames the
/// entry first, so two gates never take the same one.
pub fn adopt(root: &Path, dest: &Path) -> Option<String> {
    if fs::read_dir(dest).ok()?.next().is_some() {
        return None;
    }
    for entry in entries(root).into_iter().rev() {
        let claimed = PathBuf::from(format!("{}.claimed-{}", entry.display(), std::process::id()));
        if fs::rename(&entry, &claimed).is_err() {
            continue;
        }
        let commit = fs::read_to_string(claimed.join(BUILT_FROM)).unwrap_or_default().trim().to_string();
        let moved = !commit.is_empty()
            && fs::remove_dir(dest).is_ok()
            && fs::rename(claimed.join(BUILD_DIR), dest).is_ok();
        let _ = fs::remove_dir_all(&claimed);
        if moved {
            return Some(commit);
        }
        let _ = fs::create_dir_all(dest);
    }
    None
}

/// Age every tracked file of `tree` that is unchanged from `built_from` to HEAD.
pub fn normalize_mtimes(tree: &Path, built_from: &str) -> Result<usize, String> {
    let git = |args: &[&str]| -> Result<Vec<u8>, String> {
        let o = Command::new("git").arg("-C").arg(tree).args(args).output() // batch-job: git over the gate tree, under the gate's own cap
            .map_err(|e| format!("git: {e}"))?;
        if o.status.success() { Ok(o.stdout) } else { Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim())) }
    };
    let changed = git(&["diff", "--name-only", "-z", "--no-renames", built_from, "HEAD"])?;
    let changed: std::collections::HashSet<&[u8]> = changed.split(|b| *b == 0).collect();
    let tracked = git(&["ls-files", "-z"])?;
    let old = UNIX_EPOCH + AGED;
    let mut aged = 0;
    for rel in tracked.split(|b| *b == 0).filter(|r| !r.is_empty() && !changed.contains(r)) {
        use std::os::unix::ffi::OsStrExt;
        let p = tree.join(std::ffi::OsStr::from_bytes(rel));
        if !fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let f = fs::File::options().read(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.set_modified(old).map_err(|e| format!("{}: {e}", p.display()))?;
        aged += 1;
    }
    Ok(aged)
}

/// The gate's side of adoption: fill `dest`, then age `tree`'s files. A failure empties `dest`.
/// The line for the trial's log.
pub fn warm_start(root: &Path, dest: &Path, tree: &Path) -> Option<String> {
    let commit = adopt(root, dest)?;
    match normalize_mtimes(tree, &commit) {
        Ok(n) => Some(format!("gate: build directory warm from {commit} ({n} unchanged files aged)")),
        Err(e) => {
            let _ = fs::remove_dir_all(dest);
            let _ = fs::create_dir_all(dest);
            Some(format!("gate: warm build directory from {commit} discarded, building cold ({e})"))
        }
    }
}

/// The gate's side of donation, for a finished `tree` whose `target/dwarf` links to the tmpfs.
pub fn donate_tree(tree: &Path) -> bool {
    let Ok(dest) = fs::read_link(tree.join("target").join(BUILD_DIR)) else { return false };
    let Some(root) = dest.parent().and_then(Path::parent).filter(|_| !crate::target::is_warm(&dest)) else { return false };
    let out = Command::new("git").arg("-C").arg(tree).args(["rev-parse", "HEAD"]).output(); // batch-job: local rev-parse in the gate tree
    let commit = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    donate(root, &dest, &commit)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }
    fn mtime(p: &Path) -> SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }
    fn built(dir: &Path) {
        fs::create_dir_all(dir.join("deps")).unwrap();
        fs::write(dir.join("deps/libx.rlib"), "x").unwrap();
    }

    #[test]
    fn a_donated_build_directory_is_adopted_by_the_next_empty_one_and_only_once() {
        let t = testkit::TempDir::new("gate-warm-adopt");
        let root = t.path();
        let (a, b, c) = (root.join("tree-a/dwarf"), root.join("tree-b/dwarf"), root.join("tree-c/dwarf"));
        built(&a);
        assert!(donate(root, &a, "abc123"));
        assert!(!a.exists());
        fs::create_dir_all(&b).unwrap();
        assert_eq!(adopt(root, &b).as_deref(), Some("abc123"));
        assert!(b.join("deps/libx.rlib").is_file());
        fs::create_dir_all(&c).unwrap();
        assert_eq!(adopt(root, &c), None, "the entry was taken");
    }

    #[test]
    fn a_directory_that_already_holds_output_is_never_overwritten_by_adoption() {
        let t = testkit::TempDir::new("gate-warm-busy");
        let root = t.path();
        let (a, b) = (root.join("a/dwarf"), root.join("b/dwarf"));
        built(&a);
        donate(root, &a, "c1");
        built(&b);
        assert_eq!(adopt(root, &b), None);
        assert_eq!(entries(root).len(), 1);
    }

    #[test]
    fn nothing_built_or_no_commit_is_not_donated() {
        let t = testkit::TempDir::new("gate-warm-empty");
        let root = t.path();
        let a = root.join("a/dwarf");
        fs::create_dir_all(&a).unwrap();
        assert!(!donate(root, &a, "c1"));
        built(&a);
        assert!(!donate(root, &a, ""));
        assert!(a.join("deps").is_dir());
    }

    #[test]
    fn the_pool_keeps_only_the_newest_entries() {
        let t = testkit::TempDir::new("gate-warm-keep");
        let root = t.path();
        for i in 0..KEEP + 2 {
            let d = root.join(format!("t{i}/dwarf"));
            built(&d);
            assert!(donate(root, &d, &format!("c{i}")));
            let e = entries(root).pop().unwrap();
            fs::File::open(&e).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(1000 + i as u64)).unwrap();
        }
        assert_eq!(entries(root).len(), KEEP);
    }

    #[test]
    fn only_the_files_that_differ_from_the_built_commit_stay_new() {
        let t = testkit::TempDir::new("gate-warm-mtime");
        let tree = t.path();
        git(tree, &["init", "-q"]);
        fs::create_dir_all(tree.join("crate/src")).unwrap();
        fs::write(tree.join("crate/src/lib.rs"), "1").unwrap();
        fs::write(tree.join("crate/src/other.rs"), "1").unwrap();
        git(tree, &["add", "."]);
        git(tree, &["commit", "-qm", "base"]);
        let base = git(tree, &["rev-parse", "HEAD"]);
        fs::write(tree.join("crate/src/lib.rs"), "2").unwrap();
        git(tree, &["commit", "-qam", "change"]);
        let n = normalize_mtimes(tree, &base).unwrap();
        assert_eq!(n, 1);
        assert!(mtime(&tree.join("crate/src/other.rs")) < SystemTime::now() - Duration::from_secs(3600));
        assert!(mtime(&tree.join("crate/src/lib.rs")) > SystemTime::now() - Duration::from_secs(3600));
    }

    #[test]
    fn an_unknown_built_commit_discards_the_adopted_directory() {
        let t = testkit::TempDir::new("gate-warm-unknown");
        let root = t.path().join("root");
        let tree = t.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        git(&tree, &["init", "-q"]);
        fs::write(tree.join("f"), "1").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-qm", "base"]);
        let (a, b) = (root.join("a/dwarf"), root.join("b/dwarf"));
        built(&a);
        donate(&root, &a, "0123456789012345678901234567890123456789");
        fs::create_dir_all(&b).unwrap();
        let line = warm_start(&root, &b, &tree).unwrap();
        assert!(line.contains("discarded"), "{line}");
        assert!(b.is_dir() && fs::read_dir(&b).unwrap().next().is_none());
    }

    #[test]
    fn a_finished_tree_donates_the_directory_its_link_points_at() {
        let t = testkit::TempDir::new("gate-warm-tree");
        let root = t.path().join("root");
        let tree = t.path().join("tree");
        fs::create_dir_all(tree.join("target")).unwrap();
        git(&tree, &["init", "-q"]);
        fs::write(tree.join("f"), "1").unwrap();
        git(&tree, &["add", "."]);
        git(&tree, &["commit", "-qm", "base"]);
        let dest = root.join("name/dwarf");
        built(&dest);
        std::os::unix::fs::symlink(&dest, tree.join("target/dwarf")).unwrap();
        assert!(donate_tree(&tree));
        assert_eq!(entries(&root).len(), 1);
        assert!(!dest.exists());
    }
}
