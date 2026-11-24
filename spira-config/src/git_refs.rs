//! A repository's refs read from its ref files, starting no process. What this cannot tell it
//! says so ([`Ref::Unknown`], [`Ref::Object`], `None`) and the caller asks git.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What reading a ref without git came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ref {
    Sha(String),
    /// No loose or packed ref by any spelling git would try.
    Absent,
    /// A loose ref naming an object that may be an annotated tag: peel it before trusting it.
    Object(String),
    /// Not tellable this way (a revision expression, an unreadable layout): ask git.
    Unknown,
}

/// A checkout's git directory, and the common one its refs live in (a linked worktree's is its parent's).
pub fn git_dirs(repo: &Path) -> Option<(PathBuf, PathBuf)> {
    let dotgit = repo.join(".git");
    let gitdir = if dotgit.is_file() {
        let text = fs::read_to_string(&dotgit).ok()?;
        let target = PathBuf::from(text.strip_prefix("gitdir:")?.trim());
        if target.is_absolute() { target } else { repo.join(target) }
    } else {
        dotgit
    };
    let common = match fs::read_to_string(gitdir.join("commondir")) {
        Ok(rel) => {
            let rel = PathBuf::from(rel.trim());
            if rel.is_absolute() { rel } else { gitdir.join(rel) }
        }
        Err(_) => gitdir.clone(),
    };
    Some((gitdir, common))
}

pub fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What `name` names in the checkout at `repo`; `Unknown` when its layout is not one this reads.
pub fn read_ref_in(repo: &Path, name: &str) -> Ref {
    git_dirs(repo).filter(|(_, common)| common.join("refs").is_dir()).map_or(Ref::Unknown, |(_, common)| read_ref(&common, name))
}

/// The commit a ref names, from its loose file or `packed-refs` (the peeled line when a tag has one).
/// A symbolic ref is followed; a loose tag is an `Object`, since the file may name a tag object.
pub fn read_ref(common: &Path, name: &str) -> Ref {
    read_ref_at(common, name, 0)
}

const SYMREF_DEPTH: usize = 5;

fn read_ref_at(common: &Path, name: &str, depth: usize) -> Ref {
    let packed = fs::read_to_string(common.join("packed-refs")).unwrap_or_default();
    let spellings = [format!("refs/{name}"), format!("refs/heads/{name}"), format!("refs/remotes/{name}"), format!("refs/tags/{name}")];
    for candidate in spellings.iter().chain(std::iter::once(&name.to_string())) {
        if candidate.starts_with("refs/") {
            if let Ok(text) = fs::read_to_string(common.join(candidate)) {
                let sha = text.trim();
                if let Some(target) = sha.strip_prefix("ref:") {
                    return if depth < SYMREF_DEPTH { read_ref_at(common, target.trim(), depth + 1) } else { Ref::Unknown };
                }
                return match (is_sha(sha), candidate.starts_with("refs/tags/")) {
                    (true, false) => Ref::Sha(sha.to_string()),
                    (true, true) => Ref::Object(sha.to_string()),
                    _ => Ref::Unknown,
                };
            }
        }
        let mut lines = packed.lines().peekable();
        while let Some(l) = lines.next() {
            if let Some((sha, _)) = l.split_once(' ').filter(|(sha, r)| r == candidate && is_sha(sha)) {
                let peeled = lines.peek().and_then(|n| n.strip_prefix('^')).filter(|p| is_sha(p));
                return Ref::Sha(peeled.unwrap_or(sha).to_string());
            }
        }
    }
    if name.starts_with("refs/") || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/')) {
        return Ref::Unknown;
    }
    Ref::Absent
}

/// The branches under `prefix` (a `refs/heads/` path such as `refs/heads/spira/`) as `(name
/// under refs/heads/, sha)` in name order, a loose ref over a packed one. `None` when the
/// prefix or the layout is not one this reads, or any ref under it is not a plain sha: git
/// answers then, so a symbolic ref or a half-written file is never guessed at.
pub fn branch_tips(repo: &Path, prefix: &str) -> Option<Vec<(String, String)>> {
    let under = prefix.strip_prefix("refs/heads/")?;
    let (_, common) = git_dirs(repo)?;
    if !common.join("refs").is_dir() {
        return None;
    }
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(packed) = fs::read_to_string(common.join("packed-refs")) {
        for l in packed.lines().filter(|l| !l.starts_with(['#', '^'])) {
            if let Some((sha, name)) = l.split_once(' ').and_then(|(sha, r)| Some((sha, r.strip_prefix("refs/heads/")?))).filter(|(_, n)| n.starts_with(under)) {
                is_sha(sha).then(|| out.insert(name.to_string(), sha.to_string()))?;
            }
        }
    }
    let (dir, rest) = under.rsplit_once('/').map_or(("", under), |(d, r)| (d, r));
    let dir_path = if dir.is_empty() { common.join("refs/heads") } else { common.join("refs/heads").join(dir) };
    let stem = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    walk(&dir_path, &stem, &format!("{stem}{rest}"), &mut out)?;
    Some(out.into_iter().collect())
}

/// The `refs/heads/spira/*` tips by bead id (the name without `spira/`); `None` as [`branch_tips`].
pub fn spira_tips(repo: &Path) -> Option<BTreeMap<String, String>> {
    Some(branch_tips(repo, "refs/heads/spira/")?.into_iter().filter_map(|(name, sha)| Some((name.strip_prefix("spira/")?.to_string(), sha))).collect())
}

/// The commit a ref names when the ref files say so outright; `None` when git must be asked.
pub fn sha_of(repo: &Path, name: &str) -> Option<String> {
    match read_ref_in(repo, name) {
        Ref::Sha(sha) => Some(sha),
        _ => None,
    }
}

/// The `[remote "…"]` names in the common dir's `config`; `None` when the file cannot be read or
/// pulls in another by `include`, which this does not follow: git answers then.
pub fn remotes(repo: &Path) -> Option<Vec<String>> {
    let (_, common) = git_dirs(repo)?;
    let text = fs::read_to_string(common.join("config")).ok()?;
    let mut names = vec![];
    for line in text.lines().map(str::trim).filter(|l| l.starts_with('[')) {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("[include") || lower.starts_with("[remote.") {
            return None;
        }
        if lower.starts_with("[remote ") {
            if line.contains('\\') {
                return None;
            }
            let start = line.find('"')? + 1;
            let end = line.rfind('"').filter(|e| *e >= start)?;
            names.push(line[start..end].to_string());
        }
    }
    names.sort();
    names.dedup();
    Some(names)
}

type Held = (String, Box<dyn std::any::Any + Send>);

fn holds() -> &'static std::sync::Mutex<std::collections::HashMap<(PathBuf, String), Held>> {
    static HOLDS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(PathBuf, String), Held>>> = std::sync::OnceLock::new();
    HOLDS.get_or_init(Default::default)
}

/// The answer to `question` about `repo`, held process-wide under `key`, the shas it depends on:
/// `read` runs only when `key` differs from the one held, and a failed read is not held. One entry
/// per (repo, question), so a moved key replaces the entry.
pub fn held<T: Clone + Send + 'static>(repo: &Path, question: &str, key: &str, read: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let id = (repo.to_path_buf(), question.to_string());
    let hit = holds().lock().unwrap_or_else(|e| e.into_inner()).get(&id).filter(|(k, _)| k == key).and_then(|(_, v)| v.downcast_ref::<T>().cloned());
    if let Some(v) = hit {
        return Ok(v);
    }
    let v = read()?;
    holds().lock().unwrap_or_else(|e| e.into_inner()).insert(id, (key.to_string(), Box::new(v.clone())));
    Ok(v)
}

fn walk(dir: &Path, stem: &str, want: &str, out: &mut BTreeMap<String, String>) -> Option<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(()),
        Err(_) => return None,
    };
    for e in entries {
        let e = e.ok()?;
        let name = format!("{stem}{}", e.file_name().to_str()?);
        if name.ends_with(".lock") || !(name.starts_with(want) || want.starts_with(&format!("{name}/"))) {
            continue;
        }
        if e.path().is_dir() {
            walk(&e.path(), &format!("{name}/"), want, out)?;
        } else if name.starts_with(want) {
            let text = fs::read_to_string(e.path()).ok()?;
            is_sha(text.trim()).then(|| out.insert(name, text.trim().to_string()))?;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(c: char) -> String {
        c.to_string().repeat(40)
    }

    #[test_id(tmxzvci)]
    #[test]
    fn branch_tips_read_loose_and_packed_refs_and_through_a_worktrees_gitdir() {
        let d = testkit::TempDir::new("refs-tips");
        let common = d.join("main/.git");
        fs::create_dir_all(common.join("refs/heads/spira")).unwrap();
        fs::create_dir_all(common.join("refs/heads/other")).unwrap();
        fs::write(common.join("refs/heads/spira/sp-loose"), format!("{}\n", sha('1'))).unwrap();
        fs::write(common.join("refs/heads/spira/sp-both"), format!("{}\n", sha('2'))).unwrap();
        fs::write(common.join("refs/heads/spira/sp-stale.lock"), "").unwrap();
        fs::write(common.join("refs/heads/other/sp-not-ours"), format!("{}\n", sha('3'))).unwrap();
        fs::write(common.join("packed-refs"), format!("# pack-refs\n{} refs/heads/spira/sp-packed\n^{}\n{} refs/heads/spira/sp-both\n{} refs/heads/main\n", sha('4'), sha('5'), sha('6'), sha('7'))).unwrap();
        let want = vec![("spira/sp-both".to_string(), sha('2')), ("spira/sp-loose".to_string(), sha('1')), ("spira/sp-packed".to_string(), sha('4'))];
        assert_eq!(branch_tips(&d.join("main"), "refs/heads/spira/"), Some(want.clone()));

        let wt_git = common.join("worktrees/w");
        fs::create_dir_all(&wt_git).unwrap();
        fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        fs::create_dir_all(d.join("wt")).unwrap();
        fs::write(d.join("wt/.git"), format!("gitdir: {}\n", wt_git.display())).unwrap();
        assert_eq!(branch_tips(&d.join("wt"), "refs/heads/spira/"), Some(want), "a linked worktree reads the common refs");
        assert_eq!(branch_tips(&d.join("absent"), "refs/heads/spira/"), None, "no checkout is not an empty one");
        assert_eq!(branch_tips(&d.join("main"), "spira/"), None, "only a refs/heads/ prefix is read");
    }

    #[test_id(tb1clfm)]
    #[test]
    fn a_ref_that_is_not_a_plain_sha_under_the_prefix_leaves_the_answer_to_git() {
        let d = testkit::TempDir::new("refs-tips-odd");
        let common = d.join(".git");
        fs::create_dir_all(common.join("refs/heads/spira")).unwrap();
        fs::write(common.join("refs/heads/spira/sp-a"), format!("{}\n", sha('1'))).unwrap();
        assert!(branch_tips(d.path(), "refs/heads/spira/").is_some());
        fs::write(common.join("refs/heads/spira/sp-sym"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(branch_tips(d.path(), "refs/heads/spira/"), None);
        assert_eq!(branch_tips(d.path(), "refs/heads/other/"), Some(vec![]), "a symbolic ref outside the prefix is not read");
    }

    #[test_id(tnw9ydo)]
    #[test]
    fn a_tip_rewritten_between_two_reads_is_seen_on_the_second() {
        let d = testkit::TempDir::new("refs-rewrite");
        let common = d.join(".git");
        fs::create_dir_all(common.join("refs/heads/spira")).unwrap();
        let f = common.join("refs/heads/spira/sp-a");
        fs::write(&f, format!("{}\n", sha('1'))).unwrap();
        assert_eq!(branch_tips(d.path(), "refs/heads/spira/").unwrap()[0].1, sha('1'));
        assert_eq!(read_ref_in(d.path(), "spira/sp-a"), Ref::Sha(sha('1')));
        fs::write(&f, format!("{}\n", sha('2'))).unwrap();
        assert_eq!(branch_tips(d.path(), "refs/heads/spira/").unwrap()[0].1, sha('2'));
        assert_eq!(read_ref_in(d.path(), "spira/sp-a"), Ref::Sha(sha('2')));
    }

    #[test_id(twl7wmn)]
    #[test]
    fn a_prefix_without_a_closing_slash_or_with_nested_directories_matches_by_name() {
        let d = testkit::TempDir::new("refs-tips-prefix");
        let common = d.join(".git");
        fs::create_dir_all(common.join("refs/heads/a/b")).unwrap();
        fs::create_dir_all(common.join("refs/heads/a/c")).unwrap();
        fs::write(common.join("refs/heads/a/b/x"), format!("{}\n", sha('1'))).unwrap();
        fs::write(common.join("refs/heads/a/c/y"), format!("{}\n", sha('2'))).unwrap();
        fs::write(common.join("refs/heads/a/bz"), format!("{}\n", sha('3'))).unwrap();
        fs::write(common.join("packed-refs"), format!("{} refs/heads/a/b/p\n", sha('4'))).unwrap();
        let names = |p: &str| branch_tips(d.path(), p).unwrap().into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(names("refs/heads/a/b/"), ["a/b/p", "a/b/x"]);
        assert_eq!(names("refs/heads/a/b"), ["a/b/p", "a/b/x", "a/bz"], "a prefix is by name, not by directory");
        assert_eq!(names("refs/heads/"), ["a/b/p", "a/b/x", "a/bz", "a/c/y"]);
    }

    #[test_id(tc6clhu)]
    #[test]
    fn a_ref_is_its_loose_file_else_its_packed_line_and_a_loose_tag_is_not_trusted() {
        let d = testkit::TempDir::new("host-refs");
        let git = d.join(".git");
        fs::create_dir_all(git.join("refs/heads/local")).unwrap();
        fs::create_dir_all(git.join("refs/tags")).unwrap();
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        fs::write(git.join("refs/heads/local/main"), format!("{a}\n")).unwrap();
        fs::write(git.join("refs/tags/loose"), format!("{b}\n")).unwrap();
        fs::write(git.join("packed-refs"), format!("# pack-refs\n{b} refs/heads/packed\n{a} refs/tags/r-1\n^{c}\n")).unwrap();
        let sha = |n: &str| read_ref_in(d.path(), n);
        assert_eq!(sha("local/main"), Ref::Sha(a.clone()));
        assert_eq!(sha("refs/heads/local/main"), Ref::Sha(a));
        assert_eq!(sha("packed"), Ref::Sha(b.clone()));
        assert_eq!(sha("r-1"), Ref::Sha(c), "a packed tag answers with the commit it peels to");
        assert_eq!(sha("loose"), Ref::Object(b), "a loose tag may be the tag object, so it is peeled before it is trusted");
        assert_eq!(sha("nonesuch"), Ref::Absent, "no spelling git would try names it");
        assert_eq!(sha("HEAD~1"), Ref::Unknown, "a revision expression is git's to read");
        assert_eq!(read_ref_in(&d.join("elsewhere"), "x"), Ref::Unknown);
    }

    #[test_id(tvvo0lj)]
    #[test]
    fn a_loose_tag_is_an_object_to_peel_and_a_symbolic_ref_is_followed() {
        let d = testkit::TempDir::new("host-symref");
        let git = d.join(".git");
        fs::create_dir_all(git.join("refs/heads")).unwrap();
        fs::create_dir_all(git.join("refs/remotes/origin")).unwrap();
        fs::create_dir_all(git.join("refs/tags")).unwrap();
        let a = "a".repeat(40);
        fs::write(git.join("refs/heads/main"), format!("{a}\n")).unwrap();
        fs::write(git.join("refs/remotes/origin/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(git.join("refs/heads/loop"), "ref: refs/heads/loop\n").unwrap();
        fs::write(git.join("refs/tags/loose"), format!("{a}\n")).unwrap();
        assert_eq!(read_ref(&git, "origin/HEAD"), Ref::Sha(a.clone()));
        assert_eq!(read_ref(&git, "loose"), Ref::Object(a));
        assert_eq!(read_ref(&git, "loop"), Ref::Unknown, "a symbolic cycle is git's to name");
    }

    #[test_id(tom6h2s)]
    #[test]
    fn remotes_are_the_configs_remote_sections_and_an_include_leaves_it_to_git() {
        let d = testkit::TempDir::new("refs-remotes");
        let git = d.join(".git");
        fs::create_dir_all(&git).unwrap();
        assert_eq!(remotes(d.path()), None, "no config file is git's to answer");
        fs::write(git.join("config"), "[core]\n\tbare = false\n[branch \"main\"]\n\tremote = origin\n").unwrap();
        assert_eq!(remotes(d.path()), Some(vec![]));
        fs::write(git.join("config"), "[remote \"origin\"]\n\turl = x\n[core]\n[remote \"up stream\"]\n\turl = y\n").unwrap();
        assert_eq!(remotes(d.path()), Some(vec!["origin".to_string(), "up stream".to_string()]));
        fs::write(git.join("config"), "[remote \"origin\"]\n[include]\n\tpath = more\n").unwrap();
        assert_eq!(remotes(d.path()), None);
    }

    #[test_id(t6qv35m)]
    #[test]
    fn a_held_answer_re_reads_on_a_moved_key_and_holds_no_failure() {
        let d = testkit::TempDir::new("refs-held");
        let reads = std::cell::Cell::new(0);
        let read = |v: u32| {
            reads.set(reads.get() + 1);
            Ok::<u32, String>(v)
        };
        assert_eq!(held(d.path(), "q", "k1", || read(1)), Ok(1));
        assert_eq!(held(d.path(), "q", "k1", || read(2)), Ok(1), "the same key is not read again");
        assert_eq!(reads.get(), 1);
        assert_eq!(held(d.path(), "q", "k2", || read(3)), Ok(3), "a moved key reads again");
        assert_eq!(held(d.path(), "q", "k1", || read(4)), Ok(4), "a moved key replaces the entry");
        assert_eq!(held(d.path(), "q", "k9", || Err::<u32, _>("boom".to_string())), Err("boom".to_string()));
        assert_eq!(held(d.path(), "q", "k9", || read(5)), Ok(5), "a failure is not held");
        assert_eq!(held(d.path(), "other", "k9", || read(6)), Ok(6), "each question has its own entry");
    }
}
