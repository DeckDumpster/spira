//! Warm unit builds: a finished gate hands its tmpfs `debug` build directory to a small pool
//! and the next gate adopts one. Cargo fingerprints are workspace-relative, so mtimes are the
//! only obstacle; [`normalize_mtimes`] ages unchanged tracked files. Anything uncertain
//! discards the directory: a cold build is slow, a wrongly warm one is a wrong verdict.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The pool's directory under the target root. No gate tree has this name.
pub const POOL: &str = "warm-debug";
/// Donated directories kept; older ones are removed as new ones arrive.
pub const KEEP: usize = 2;
const BUILT_FROM: &str = "built-from";
const BUILT_AT: &str = "built-at";
const BUILD_DIR: &str = "debug";
const AGED: Duration = Duration::from_secs(1_000_000_000);

fn repo_pool(root: &Path, repo: &str) -> PathBuf {
    let name: String = repo.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'-' { (b as char).to_string() } else { format!("_{b:02x}") }).collect();
    root.join(POOL).join(if name.is_empty() { "_".into() } else { name })
}

fn entries(root: &Path, repo: &str) -> Vec<PathBuf> {
    let mut v: Vec<(SystemTime, PathBuf)> = fs::read_dir(repo_pool(root, repo))
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

/// Bytes held by the pool (every repository's), for the target root's cap.
pub fn size(root: &Path) -> u64 {
    crate::target::size_of(&root.join(POOL))
}

/// Remove every pooled directory (the root is short of room).
pub fn shed(root: &Path) {
    let _ = fs::remove_dir_all(root.join(POOL));
}

/// Every repository's unclaimed pooled entries except those under `skip`, oldest first.
fn pooled_oldest_first(root: &Path, skip: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<(SystemTime, PathBuf)> = fs::read_dir(root.join(POOL))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| Some(p.as_path()) != skip && p.is_dir()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .flat_map(|d| fs::read_dir(d).map(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>()).unwrap_or_default())
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().contains(".claimed")))
        .map(|p| (fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH), p))
        .collect();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

/// Remove the oldest unclaimed pooled entry of any repository; false when there is none.
pub fn evict_oldest(root: &Path) -> bool {
    pooled_oldest_first(root, None).first().is_some_and(|p| fs::remove_dir_all(p).is_ok())
}

/// Move `dest` (a gate tree's finished `debug` directory) into the pool, recorded as built from
/// `commit` in `tree`. A directory with no compiled dependencies, or no commit, is not worth keeping.
pub fn donate(root: &Path, repo: &str, dest: &Path, commit: &str, tree: &Path) -> bool {
    if commit.is_empty() || !dest.join("deps").is_dir() {
        return false;
    }
    let pool = repo_pool(root, repo);
    if fs::create_dir_all(&pool).is_err() {
        return false;
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let entry = pool.join(format!("{nanos}-{}", std::process::id()));
    if fs::create_dir(&entry).is_err() || fs::rename(dest, entry.join(BUILD_DIR)).is_err() {
        let _ = fs::remove_dir_all(&entry);
        return false;
    }
    if fs::write(entry.join(BUILT_FROM), commit).is_err() || fs::write(entry.join(BUILT_AT), tree.as_os_str().as_encoded_bytes()).is_err() {
        let _ = fs::remove_dir_all(&entry);
        return false;
    }
    let all = entries(root, repo);
    for old in all.iter().take(all.len().saturating_sub(KEEP)) {
        let _ = fs::remove_dir_all(old);
    }
    true
}

/// Fill the empty directory `dest` from the newest pooled one; the commit it was built from.
/// None when `dest` holds anything already or the pool has nothing to give. Claiming renames the
/// entry first, so two gates never take the same one. The commit and the tree path it was built in.
pub fn adopt(root: &Path, repo: &str, dest: &Path) -> Option<(String, PathBuf)> {
    if fs::read_dir(dest).ok()?.next().is_some() {
        return None;
    }
    for entry in entries(root, repo).into_iter().rev() {
        let claimed = PathBuf::from(format!("{}.claimed-{}", entry.display(), std::process::id()));
        if fs::rename(&entry, &claimed).is_err() {
            continue;
        }
        let commit = fs::read_to_string(claimed.join(BUILT_FROM)).unwrap_or_default().trim().to_string();
        let at = fs::read_to_string(claimed.join(BUILT_AT)).unwrap_or_default();
        let moved = !commit.is_empty()
            && !at.is_empty()
            && fs::remove_dir(dest).is_ok()
            && fs::rename(claimed.join(BUILD_DIR), dest).is_ok();
        let _ = fs::remove_dir_all(&claimed);
        if moved {
            return Some((commit, PathBuf::from(at)));
        }
        let _ = fs::create_dir_all(dest);
    }
    None
}

/// Remove every unit of `dir` (a `debug` build directory) whose compile read an environment
/// variable holding `built_in` — `env!("CARGO_MANIFEST_DIR")` bakes the tree's path into a
/// binary, and cargo's fingerprints are tree-relative so would call it fresh in another tree.
/// Reads rustc's `env-dep:` lines in `deps/*.d`. Returns the units removed.
pub fn purge_path_bound_units(dir: &Path, built_in: &Path) -> usize {
    let tree = built_in.to_string_lossy().into_owned();
    let home = built_in.file_name().map(|n| format!("/{}/", n.to_string_lossy()));
    let Ok(rd) = fs::read_dir(dir.join("deps")) else { return 0 };
    let mut hashes = std::collections::BTreeSet::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "d") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let bound = text.lines().filter(|l| l.starts_with("# env-dep:")).any(|l| l.contains(&tree) || home.as_deref().is_some_and(|h| l.contains(h)));
        if bound {
            if let Some(h) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.rsplit_once('-')).map(|(_, h)| h.to_string()) {
                hashes.insert(h);
            }
        }
    }
    for h in &hashes {
        let suffix = format!("-{h}");
        for sub in ["deps", ".fingerprint", "build"] {
            let Ok(rd) = fs::read_dir(dir.join(sub)) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let stem = name.split('.').next().unwrap_or(&name);
                if stem.ends_with(&suffix) {
                    let _ = fs::remove_dir_all(e.path()).or_else(|_| fs::remove_file(e.path()));
                }
            }
        }
    }
    hashes.len()
}

/// Age every tracked file of `tree` that is unchanged from `built_from` to HEAD, and every
/// directory none of whose changes (or a descendant's) is in that diff: a build script's
/// `rerun-if-changed` on a directory reads the directory's own mtime too.
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
    let ancestors = |rel: &[u8]| -> Vec<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(rel)).ancestors().skip(1).filter(|a| !a.as_os_str().is_empty()).map(Path::to_path_buf).collect()
    };
    let changed_dirs: std::collections::HashSet<PathBuf> =
        changed.iter().filter(|r| !r.is_empty()).flat_map(|r| ancestors(r)).collect();
    let mut dirs = std::collections::BTreeSet::new();
    for rel in tracked.split(|b| *b == 0).filter(|r| !r.is_empty() && !changed.contains(r)) {
        use std::os::unix::ffi::OsStrExt;
        let p = tree.join(std::ffi::OsStr::from_bytes(rel));
        if !fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let f = fs::File::options().read(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.set_modified(old).map_err(|e| format!("{}: {e}", p.display()))?;
        aged += 1;
        dirs.extend(ancestors(rel).into_iter().filter(|d| !changed_dirs.contains(d)));
    }
    for d in dirs {
        let p = tree.join(&d);
        let f = fs::File::options().read(true).open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.set_modified(old).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(aged)
}

pub const WARM_FROM: &str = "gate: build directory warm from ";
pub const COLD_OPEN: &str = "building cold (";
pub const BUILDER_IN: &str = "gate: build in ";

/// What [`warm_start`] did with the entry it adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adopted {
    From(String),
    Discarded,
}

/// The gate's side of adoption: fill `dest`, then age `tree`'s files. A failure empties `dest`.
/// `None` when nothing was there to adopt; otherwise what happened, with the line for the trial's log.
pub fn warm_start(root: &Path, repo: &str, dest: &Path, tree: &Path) -> Option<(Adopted, String)> {
    let (commit, built_in) = adopt(root, repo, dest)?;
    match normalize_mtimes(tree, &commit) {
        Ok(n) => {
            let bound = if built_in == tree { 0 } else { purge_path_bound_units(dest, &built_in) };
            let line = format!("{WARM_FROM}{commit} ({n} unchanged files aged, {bound} units rebuilt for their baked-in path)");
            Some((Adopted::From(commit), line))
        }
        Err(e) => {
            let _ = fs::remove_dir_all(dest);
            let _ = fs::create_dir_all(dest);
            Some((Adopted::Discarded, format!("gate: warm build directory from {commit} discarded, {COLD_OPEN}{e})")))
        }
    }
}

/// Entries the previous release left directly under the pool, before it was per repository.
fn remove_unscoped(root: &Path) {
    let Ok(rd) = fs::read_dir(root.join(POOL)) else { return };
    for e in rd.flatten() {
        if e.path().join(BUILT_FROM).exists() || e.file_name().to_string_lossy().contains(".claimed") {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// Hold the whole pool at or under `cap_bytes`: trim `repo`'s newest entry of what no build can
/// reuse, drop `repo`'s older entries while over, then the other repositories' oldest, and empty
/// the pool when `repo`'s newest alone is still over. The line for the gate's log, with the
/// pool's size before and after.
pub fn bound(root: &Path, repo: &str, cap_bytes: u64) -> String {
    remove_unscoped(root);
    let before = size(root);
    let now = SystemTime::now();
    if let Some(newest) = entries(root, repo).pop() {
        spira_config::build_dir::trim(&newest, now, now);
    }
    let others = || pooled_oldest_first(root, Some(&repo_pool(root, repo)));
    let mut older: Vec<PathBuf> = entries(root, repo).into_iter().rev().skip(1).collect();
    older.extend(others());
    for old in older {
        if size(root) <= cap_bytes {
            break;
        }
        let _ = fs::remove_dir_all(old);
    }
    if size(root) > cap_bytes {
        shed(root);
    }
    let after = size(root);
    let emptied = if before > 0 && after == 0 { ", emptied (over the cap)" } else { "" };
    format!("gate: warm build directory {} MiB before trimming, {} MiB after (cap {} MiB){emptied}", before >> 20, after >> 20, cap_bytes >> 20)
}

/// The gate's side of donation, for a finished `tree` whose `target/debug` links to the tmpfs;
/// the pool is then held under `cap_mib`. The line for the gate's log when something was donated.
pub fn donate_tree(tree: &Path, repo: &str, cap_mib: u64) -> Option<String> {
    let dest = fs::read_link(tree.join("target").join(BUILD_DIR)).ok()?;
    let root = dest.parent().and_then(Path::parent).filter(|_| !crate::target::is_warm(&dest) && !crate::target::is_pooled(&dest))?;
    let out = Command::new("git").arg("-C").arg(tree).args(["rev-parse", "HEAD"]).output(); // batch-job: local rev-parse in the gate tree
    let commit = out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    donate(root, repo, &dest, &commit, tree).then(|| bound(root, repo, cap_mib << 20))
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

    #[test_id(tytg7je)]
    #[test]
    fn evict_oldest_removes_one_unclaimed_entry_of_any_repository_oldest_first() {
        let t = testkit::TempDir::new("gate-warm-evict");
        let root = t.path();
        assert!(!evict_oldest(root), "an absent pool has nothing to evict");
        for (i, repo) in ["a", "b"].into_iter().enumerate() {
            let d = root.join(format!("t{i}/debug"));
            built(&d);
            assert!(donate(root, repo, &d, "c", Path::new("t")));
        }
        let (ea, eb) = (entries(root, "a")[0].clone(), entries(root, "b")[0].clone());
        let claimed = PathBuf::from(format!("{}.claimed-1", ea.display()));
        fs::rename(&ea, &claimed).unwrap();
        assert!(evict_oldest(root));
        assert!(!eb.exists() && claimed.is_dir(), "the claimed entry is never evicted");
        assert!(!evict_oldest(root));
    }

    #[test_id(tco6vnu)]
    #[test]
    fn a_donated_build_directory_is_adopted_by_the_next_empty_one_and_only_once() {
        let t = testkit::TempDir::new("gate-warm-adopt");
        let root = t.path();
        let (a, b, c) = (root.join("tree-a/debug"), root.join("tree-b/debug"), root.join("tree-c/debug"));
        built(&a);
        assert!(donate(root, "r", &a, "abc123", Path::new("t")));
        assert!(!a.exists());
        fs::create_dir_all(&b).unwrap();
        assert_eq!(adopt(root, "r", &b), Some(("abc123".to_string(), PathBuf::from("t"))));
        assert!(b.join("deps/libx.rlib").is_file());
        fs::create_dir_all(&c).unwrap();
        assert_eq!(adopt(root, "r", &c), None, "the entry was taken");
    }

    #[test_id(t2xgggo)]
    #[test]
    fn a_directory_donated_under_one_repository_is_adopted_only_under_that_one() {
        let t = testkit::TempDir::new("gate-warm-repos");
        let root = t.path();
        let (d, b, a) = (root.join("t/debug"), root.join("b/debug"), root.join("a/debug"));
        built(&d);
        assert!(donate(root, "a", &d, "c1", Path::new("t")));
        fs::create_dir_all(&b).unwrap();
        assert_eq!(adopt(root, "b", &b), None);
        fs::create_dir_all(&a).unwrap();
        assert_eq!(adopt(root, "a", &a), Some(("c1".to_string(), PathBuf::from("t"))));
    }

    #[test_id(teom45d)]
    #[test]
    fn the_cap_covers_every_repositorys_entries_and_the_previous_layout_is_removed() {
        let t = testkit::TempDir::new("gate-warm-cap-repos");
        let root = t.path();
        for (i, repo) in ["a", "b"].into_iter().enumerate() {
            let d = root.join(format!("t{i}/debug"));
            fill(&d, 2);
            assert!(donate(root, repo, &d, "c", Path::new("t")));
        }
        let old = root.join(POOL).join("1-2");
        fill(&old.join(BUILD_DIR), 1);
        fs::write(old.join(BUILT_FROM), "c").unwrap();
        bound(root, "b", 3 << 20);
        assert!(!old.exists(), "an entry directly under the pool is the previous layout");
        assert!(size(root) <= 3 << 20, "the cap held across repositories");
        assert_eq!(entries(root, "b").len(), 1, "the donating repository's entry is the one kept");
        assert!(entries(root, "a").is_empty());
    }

    #[test_id(tj6n0fo)]
    #[test]
    fn repository_names_that_could_alias_or_escape_get_pools_of_their_own() {
        let root = Path::new("r");
        let pools: std::collections::HashSet<PathBuf> = ["a/b", "a_b", "..", ".", "", "a"].iter().map(|n| repo_pool(root, n)).collect();
        assert_eq!(pools.len(), 6);
        assert!(pools.iter().all(|p| p.parent() == Some(&root.join(POOL))));
    }

    #[test_id(tlgh2qq)]
    #[test]
    fn bounding_never_removes_an_entry_another_gate_has_claimed() {
        let t = testkit::TempDir::new("gate-warm-claimed");
        let root = t.path();
        for i in 0..2 {
            let d = root.join(format!("t{i}/debug"));
            fill(&d, 2);
            assert!(donate(root, "a", &d, "c", Path::new("t")));
        }
        let entry = entries(root, "a").pop().unwrap();
        let claimed = PathBuf::from(format!("{}.claimed-1", entry.display()));
        fs::rename(&entry, &claimed).unwrap();
        bound(root, "b", 3 << 20);
        assert!(claimed.is_dir());
        assert!(entries(root, "a").is_empty(), "the unclaimed entry made way");
    }

    #[test_id(t3rdtrd)]
    #[test]
    fn a_pooled_entry_without_its_build_path_is_not_adopted() {
        let t = testkit::TempDir::new("gate-warm-no-built-at");
        let root = t.path();
        let (a, b) = (root.join("tree-a/debug"), root.join("tree-b/debug"));
        built(&a);
        assert!(donate(root, "r", &a, "abc123", Path::new("t")));
        let entry = entries(root, "r").pop().unwrap();
        fs::remove_file(entry.join(BUILT_AT)).unwrap();
        fs::create_dir_all(&b).unwrap();
        assert_eq!(adopt(root, "r", &b), None);
        assert!(!b.join("deps/libx.rlib").exists());
    }

    #[test_id(t81i9r0)]
    #[test]
    fn a_directory_that_already_holds_output_is_never_overwritten_by_adoption() {
        let t = testkit::TempDir::new("gate-warm-busy");
        let root = t.path();
        let (a, b) = (root.join("a/debug"), root.join("b/debug"));
        built(&a);
        donate(root, "r", &a, "c1", Path::new("t"));
        built(&b);
        assert_eq!(adopt(root, "r", &b), None);
        assert_eq!(entries(root, "r").len(), 1);
    }

    #[test_id(tkbcjt3)]
    #[test]
    fn nothing_built_or_no_commit_is_not_donated() {
        let t = testkit::TempDir::new("gate-warm-empty");
        let root = t.path();
        let a = root.join("a/debug");
        fs::create_dir_all(&a).unwrap();
        assert!(!donate(root, "r", &a, "c1", Path::new("t")));
        built(&a);
        assert!(!donate(root, "r", &a, "", Path::new("t")));
        assert!(a.join("deps").is_dir());
    }

    #[test_id(tmvn42r)]
    #[test]
    fn the_pool_keeps_only_the_newest_entries() {
        let t = testkit::TempDir::new("gate-warm-keep");
        let root = t.path();
        for i in 0..KEEP + 2 {
            let d = root.join(format!("t{i}/debug"));
            built(&d);
            assert!(donate(root, "r", &d, &format!("c{i}"), Path::new("t")));
            let e = entries(root, "r").pop().unwrap();
            fs::File::open(&e).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(1000 + i as u64)).unwrap();
        }
        assert_eq!(entries(root, "r").len(), KEEP);
    }

    #[test_id(tiidygn)]
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
        // clock-safe: compares file mtimes against a stamp set an hour back; no duration is measured
        assert!(mtime(&tree.join("crate/src/other.rs")) < SystemTime::now() - Duration::from_secs(3600));
        // clock-safe: compares file mtimes against a stamp set an hour back; no duration is measured
        assert!(mtime(&tree.join("crate/src/lib.rs")) > SystemTime::now() - Duration::from_secs(3600));
    }

    #[test_id(thog2k3)]
    #[test]
    fn a_directory_holding_only_unchanged_files_is_aged_with_them() {
        let t = testkit::TempDir::new("gate-warm-dirs");
        let tree = t.path();
        git(tree, &["init", "-q"]);
        for d in ["crate/rules", "crate/src"] {
            fs::create_dir_all(tree.join(d)).unwrap();
            fs::write(tree.join(d).join("a.rs"), "1").unwrap();
        }
        git(tree, &["add", "."]);
        git(tree, &["commit", "-qm", "base"]);
        let base = git(tree, &["rev-parse", "HEAD"]);
        fs::write(tree.join("crate/src/b.rs"), "1").unwrap();
        git(tree, &["add", "."]);
        git(tree, &["commit", "-qm", "add"]);
        normalize_mtimes(tree, &base).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        // clock-safe: compares file mtimes against a stamp set an hour back; no duration is measured
        assert!(mtime(&tree.join("crate/rules")) < old, "a rerun-if-changed directory must look as old as its files");
        // clock-safe: compares file mtimes against a stamp set an hour back; no duration is measured
        assert!(mtime(&tree.join("crate/src")) > old, "a directory that gained a file changed");
    }

    #[test_id(t0detp5)]
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
        let (a, b) = (root.join("a/debug"), root.join("b/debug"));
        built(&a);
        donate(&root, "r", &a, "0123456789012345678901234567890123456789", Path::new("t"));
        fs::create_dir_all(&b).unwrap();
        let (adopted, line) = warm_start(&root, "r", &b, &tree).unwrap();
        assert_eq!(adopted, Adopted::Discarded, "{line}");
        assert!(line.contains("discarded"), "{line}");
        assert!(b.is_dir() && fs::read_dir(&b).unwrap().next().is_none());
    }

    #[test_id(tcr032r)]
    #[test]
    fn only_units_whose_compile_read_the_donors_path_are_purged() {
        let t = testkit::TempDir::new("gate-warm-purge");
        let d = t.path().join("debug");
        for (unit, env) in [("bound-aaaa", "CARGO_MANIFEST_DIR=/w/.gate.h.a/bound"), ("plain-bbbb", "CARGO_PKG_NAME=plain"), ("out-cccc", "OUT_DIR=/root/.gate.h.a/debug/build/out")] {
            fs::create_dir_all(d.join(".fingerprint").join(unit)).unwrap();
            fs::create_dir_all(d.join("deps")).unwrap();
            let h = unit.rsplit('-').next().unwrap();
            fs::write(d.join(format!("deps/{}.d", unit)), format!("deps/x: src/lib.rs\n\n# env-dep:{env}\n")).unwrap();
            fs::write(d.join(format!("deps/lib{}-{h}.rlib", unit.split('-').next().unwrap())), "x").unwrap();
        }
        assert_eq!(purge_path_bound_units(&d, Path::new("w/.gate.h.a")), 2);
        assert!(!d.join(".fingerprint/bound-aaaa").exists() && !d.join("deps/libbound-aaaa.rlib").exists());
        assert!(d.join(".fingerprint/plain-bbbb").is_dir() && d.join("deps/libplain-bbbb.rlib").is_file());
    }

    fn fill(dir: &Path, mib: usize) {
        fs::create_dir_all(dir.join("deps")).unwrap();
        fs::write(dir.join("deps/libbig.rlib"), vec![7u8; mib << 20]).unwrap();
    }

    #[test_id(tf06slv)]
    #[test]
    fn a_pool_over_the_cap_loses_its_older_entries_first_and_is_emptied_when_the_newest_alone_is_over() {
        let t = testkit::TempDir::new("gate-warm-bound");
        let root = t.path();
        for i in 0..2 {
            let d = root.join(format!("t{i}/debug"));
            fill(&d, 2);
            assert!(donate(root, "r", &d, &format!("c{i}"), Path::new("t")));
            let e = entries(root, "r").pop().unwrap();
            fs::File::open(&e).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(1000 + i as u64)).unwrap();
        }
        let newest = entries(root, "r").pop().unwrap();
        let line = bound(root, "r", 3 << 20);
        assert_eq!(entries(root, "r"), vec![newest], "{line}");
        assert!(size(root) <= 3 << 20 && size(root) > 0, "{line}");
        let line = bound(root, "r", 1 << 20);
        assert_eq!(size(root), 0, "{line}");
        assert!(line.contains("emptied"), "{line}");
    }

    #[test_id(tfdtv21)]
    #[test]
    fn trimming_a_pooled_entry_drops_the_dependency_files_nothing_touched_for_a_day() {
        let t = testkit::TempDir::new("gate-warm-trim");
        let root = t.path();
        let d = root.join("t/debug");
        fill(&d, 1);
        let stale = d.join("deps/libstale.rlib");
        fs::write(&stale, "s").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3 * 86_400);
        let f = fs::File::options().write(true).open(&stale).unwrap();
        f.set_modified(old).unwrap();
        f.set_times(fs::FileTimes::new().set_accessed(old).set_modified(old)).unwrap();
        assert!(donate(root, "r", &d, "c", Path::new("t")));
        bound(root, "r", 1 << 30);
        let kept = entries(root, "r").pop().unwrap().join("debug/deps");
        assert!(kept.join("libbig.rlib").is_file());
        assert!(!kept.join("libstale.rlib").exists());
    }

    #[test_id(tiffd0i)]
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
        let dest = root.join("name/debug");
        built(&dest);
        std::os::unix::fs::symlink(&dest, tree.join("target/debug")).unwrap();
        assert!(donate_tree(&tree, "r", 1 << 20).is_some());
        assert_eq!(entries(&root, "r").len(), 1);
        assert!(!dest.exists());
    }

    /// A workspace crate built with different features is a different unit, so a donor built from
    /// a different `-p` selection leaves it, and everything above it, to rebuild. A dependent that
    /// asks a workspace crate for features makes the crate's own feature set depend on which
    /// dependents a gate selected.
    #[test_id(t52wqvc)]
    #[test]
    fn a_workspace_crates_features_do_not_depend_on_which_dependents_a_gate_selects() {
        let manifest = spira_config::resolve::checkout_root_for_process().unwrap().join("Cargo.toml");
        let o = Command::new("cargo") // batch-job: reads the workspace manifest, no build
            .args(["metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path"])
            .arg(&manifest)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let meta: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        let pkgs = meta["packages"].as_array().unwrap();
        let names: Vec<&str> = pkgs.iter().map(|p| p["name"].as_str().unwrap()).collect();
        let mut asked: Vec<String> = pkgs
            .iter()
            .flat_map(|p| p["dependencies"].as_array().unwrap().iter().map(move |d| (p["name"].as_str().unwrap(), d)))
            .filter(|(_, d)| names.contains(&d["name"].as_str().unwrap()) && !d["features"].as_array().unwrap().is_empty())
            .map(|(from, d)| format!("{from} -> {}", d["name"].as_str().unwrap()))
            .collect();
        asked.sort();
        assert!(asked.is_empty(), "{asked:?}");
    }
}
