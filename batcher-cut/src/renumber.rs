//! A member that adds `lifecycle/migrations/NNNN-x.sql` while another landed `NNNN-y.sql` has
//! no textual conflict, only two migrations with one number. After each successful member
//! merge the new files are checked against the tree the merge was made into; a colliding one
//! takes the next free number and the merge commit is amended with the references rewritten.

use std::path::Path;
use std::process::Command;

pub const DIR: &str = "lifecycle/migrations";

fn git(wt: &Path, args: &[&str]) -> Result<String, String> {
    // batch-job: git history operation, as long as the repository is large
    let o = Command::new("git").arg("-C").arg(wt).args(args).output().map_err(|e| format!("git {args:?}: {e}"))?;
    if !o.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}

fn number(name: &str) -> Option<u32> {
    let (n, rest) = name.split_once('-')?;
    if n.len() < 4 || !n.bytes().all(|b| b.is_ascii_digit()) || !rest.ends_with(".sql") {
        return None;
    }
    n.parse().ok()
}

/// `(old, new)` file names for each of `added` whose number is taken by a name in `existing`
/// or by an earlier added one; new numbers start above everything seen.
pub fn plan(existing: &[String], added: &[String]) -> Vec<(String, String)> {
    let mut taken: Vec<u32> = existing.iter().filter_map(|n| number(n)).collect();
    let mut next = taken.iter().copied().chain(added.iter().filter_map(|n| number(n))).max().unwrap_or(0) + 1;
    let mut out = Vec::new();
    let mut added = added.to_vec();
    added.sort();
    for name in added {
        let Some(n) = number(&name) else { continue };
        if !taken.contains(&n) {
            taken.push(n);
            continue;
        }
        let slug = name.split_once('-').map(|(_, s)| s).unwrap_or("");
        while taken.contains(&next) {
            next += 1;
        }
        out.push((name.clone(), format!("{next:04}-{slug}")));
        taken.push(next);
    }
    out
}

fn replace_unless(text: &str, needle: &str, new: &str, continues: fn(u8) -> bool) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(needle) {
        let after = &rest[i + needle.len()..];
        out.push_str(&rest[..i]);
        out.push_str(if after.bytes().next().is_some_and(continues) { needle } else { new });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Replaces a migration's stem, and its bare `migrations/NNNN` form, where it is not the
/// prefix of a longer name or number.
pub fn rewrite(text: &str, old: &str, new: &str) -> String {
    let stem = |s: &str| s.trim_end_matches(".sql").to_string();
    let text = replace_unless(text, &stem(old), &stem(new), |b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    let num = |s: &str| format!("migrations/{}", &s[..s.find('-').unwrap_or(0)]);
    replace_unless(&text, &num(old), &num(new), |b| b.is_ascii_digit())
}

fn names(wt: &Path, rev: &str) -> Vec<String> {
    git(wt, &["ls-tree", "--name-only", rev, &format!("{DIR}/")])
        .map(|o| o.lines().filter_map(|l| l.rsplit('/').next()).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Renumbers colliding migrations introduced by the merge commit at HEAD. Returns the
/// renames made; the merge commit is amended in place.
pub fn after_merge(wt: &Path) -> Result<Vec<(String, String)>, String> {
    let before = names(wt, "HEAD^1");
    let added: Vec<String> = names(wt, "HEAD").into_iter().filter(|n| !before.contains(n)).collect();
    let renames = plan(&before, &added);
    if renames.is_empty() {
        return Ok(renames);
    }
    let changed: Vec<String> = git(wt, &["diff", "--name-only", "HEAD^1", "HEAD"])?.lines().map(str::to_string).collect();
    for (old, new) in &renames {
        git(wt, &["mv", &format!("{DIR}/{old}"), &format!("{DIR}/{new}")])?;
    }
    for f in changed {
        let path = wt.join(&f);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let mut out = text.clone();
        for (old, new) in &renames {
            out = rewrite(&out, old, new);
        }
        if out != text {
            std::fs::write(&path, out).map_err(|e| format!("{f}: {e}"))?;
        }
    }
    git(wt, &["add", "-A"])?;
    git(wt, &["commit", "-q", "--amend", "--no-edit"])?;
    Ok(renames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn collision_takes_next_free_number() {
        let p = plan(&s(&["0015-a.sql", "0016-b.sql"]), &s(&["0016-c.sql", "0017-d.sql"]));
        assert_eq!(p, vec![("0016-c.sql".to_string(), "0018-c.sql".to_string())]);
    }

    #[test]
    fn no_collision_no_rename() {
        assert!(plan(&s(&["0015-a.sql"]), &s(&["0016-b.sql"])).is_empty());
    }

    #[test]
    fn two_added_with_one_number_split() {
        let p = plan(&s(&["0015-a.sql"]), &s(&["0016-b.sql", "0016-c.sql"]));
        assert_eq!(p, vec![("0016-c.sql".to_string(), "0017-c.sql".to_string())]);
    }

    #[test]
    fn rewrite_respects_boundaries() {
        let t = "see migrations/0016-c.sql and migrations/0016 but not migrations/00160 or 0016-cc.sql";
        let r = rewrite(t, "0016-c.sql", "0018-c.sql");
        assert_eq!(r, "see migrations/0018-c.sql and migrations/0018 but not migrations/00160 or 0016-cc.sql");
    }

    fn git_ok(d: &Path, a: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(d).args(a).status().unwrap().success(), "{a:?}");
    }

    #[test]
    fn merge_with_colliding_migration_is_renumbered_and_references_follow() {
        let d = testkit::TempDir::new("renumber");
        git_ok(&d, &["init", "-q", "-b", "main"]);
        git_ok(&d, &["config", "user.email", "t@t"]);
        git_ok(&d, &["config", "user.name", "t"]);
        fs::create_dir_all(d.join(DIR)).unwrap();
        fs::write(d.join(DIR).join("0001-a.sql"), "x").unwrap();
        git_ok(&d, &["add", "-A"]);
        git_ok(&d, &["commit", "-q", "-m", "base"]);
        git_ok(&d, &["checkout", "-q", "-b", "side"]);
        fs::write(d.join(DIR).join("0002-side.sql"), "y").unwrap();
        fs::write(d.join("ref.txt"), "uses migrations/0002-side.sql").unwrap();
        git_ok(&d, &["add", "-A"]);
        git_ok(&d, &["commit", "-q", "-m", "side"]);
        git_ok(&d, &["checkout", "-q", "main"]);
        fs::write(d.join(DIR).join("0002-main.sql"), "z").unwrap();
        git_ok(&d, &["add", "-A"]);
        git_ok(&d, &["commit", "-q", "-m", "main"]);
        git_ok(&d, &["merge", "-q", "--no-ff", "-m", "merge", "side"]);

        let r = after_merge(&d).unwrap();
        assert_eq!(r, vec![("0002-side.sql".to_string(), "0003-side.sql".to_string())]);
        assert!(d.join(DIR).join("0003-side.sql").exists());
        assert!(!d.join(DIR).join("0002-side.sql").exists());
        assert!(d.join(DIR).join("0002-main.sql").exists());
        assert_eq!(fs::read_to_string(d.join("ref.txt")).unwrap(), "uses migrations/0003-side.sql");
        assert!(after_merge(&d).unwrap().is_empty(), "a renumbered tree has no collision left");
    }
}
