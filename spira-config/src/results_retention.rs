//! Age-bounded pruning of a results folder, run by the tools that write into it.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// What one prune call did: entries removed, and over-age entries still waiting for a later call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub removed: usize,
    pub left: usize,
}

/// `SPIRA_RESULTS_KEEP_DAYS` and `SPIRA_RESULTS_PRUNE_MAX`, as declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    pub keep_days: u32,
    pub max: usize,
}

impl Retention {
    pub fn from_config() -> Result<Retention, String> {
        Ok(Retention {
            keep_days: crate::process::cfg_parse::<u32>("SPIRA_RESULTS_KEEP_DAYS")?,
            max: crate::process::cfg_parse::<u32>("SPIRA_RESULTS_PRUNE_MAX")? as usize,
        })
    }

    /// [`prune`] under this retention, with the one stderr line when something went.
    pub fn prune(&self, tool: &str, dir: &Path, now: u64, keep: &dyn Fn(&str) -> bool) -> Pruned {
        let p = prune(dir, self.keep_days, self.max, now, keep);
        if p.removed > 0 {
            eprintln!(
                "{tool}: pruned {} results older than {}d from {} ({} left)",
                p.removed,
                self.keep_days,
                dir.display(),
                p.left
            );
        }
        p
    }
}

fn mtime(p: &Path) -> Option<u64> {
    fs::symlink_metadata(p).ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// What a write into `<run>/rounds` keeps: anything that is not a `*.results` directory of
/// `repo`'s own batches, and the results of every batch in `open`.
pub fn rounds_keep<'a>(repo: &'a str, open: &'a [String]) -> impl Fn(&str) -> bool + 'a {
    move |name| {
        let Some(batch) = name.strip_suffix(".results") else {
            return true;
        };
        let batch = batch.replace("%2F", "/");
        let own = batch.starts_with(&format!("{repo}-")) || batch.starts_with(&format!("{repo}/"));
        !own || open.contains(&batch)
    }
}

/// The newest mtime of the entry and of its direct children.
fn age_stamp(p: &Path) -> Option<u64> {
    let own = mtime(p)?;
    let kids = fs::read_dir(p).into_iter().flatten().flatten().filter_map(|e| mtime(&e.path()));
    Some(kids.fold(own, u64::max))
}

/// Removes up to `max` entries of `dir`, oldest first, whose newest mtime is more than
/// `keep_days` days before `now` and whose name `keep` does not claim. A failed removal is
/// skipped; the entry stays among `left`.
pub fn prune(dir: &Path, keep_days: u32, max: usize, now: u64, keep: &dyn Fn(&str) -> bool) -> Pruned {
    let Ok(rd) = fs::read_dir(dir) else {
        return Pruned::default();
    };
    let cutoff = now.saturating_sub(u64::from(keep_days) * 86400);
    let mut old: Vec<(u64, std::path::PathBuf)> = rd
        .flatten()
        .filter(|e| !keep(&e.file_name().to_string_lossy()))
        .filter_map(|e| age_stamp(&e.path()).map(|t| (t, e.path())))
        .filter(|(t, _)| *t < cutoff)
        .collect();
    old.sort();
    let mut out = Pruned::default();
    for (_, p) in old {
        let gone = out.removed < max && if p.is_dir() && !p.is_symlink() { fs::remove_dir_all(&p).is_ok() } else { fs::remove_file(&p).is_ok() };
        if gone {
            out.removed += 1;
        } else {
            out.left += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const NOW: u64 = 2_000_000_000;

    fn stamp(p: &Path, days: u64) {
        let t = UNIX_EPOCH + Duration::from_secs(NOW - days * 86400);
        let f = fs::File::open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    fn file(dir: &Path, name: &str, days: u64) {
        let p = dir.join(name);
        fs::write(&p, "x").unwrap();
        stamp(&p, days);
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn an_entry_older_than_the_window_is_removed() {
        let t = testkit::TempDir::new("retention-old");
        file(t.path(), "a", 8);
        assert_eq!(prune(t.path(), 7, 50, NOW, &none), Pruned { removed: 1, left: 0 });
        assert!(!t.path().join("a").exists());
    }

    #[test]
    fn an_entry_inside_the_window_is_kept() {
        let t = testkit::TempDir::new("retention-young");
        file(t.path(), "a", 6);
        assert_eq!(prune(t.path(), 7, 50, NOW, &none), Pruned::default());
        assert!(t.path().join("a").exists());
    }

    #[test]
    fn an_old_dir_with_a_fresh_child_is_kept() {
        let t = testkit::TempDir::new("retention-live");
        let d = t.path().join("run");
        fs::create_dir(&d).unwrap();
        file(&d, "out", 1);
        stamp(&d, 8);
        assert_eq!(prune(t.path(), 7, 50, NOW, &none), Pruned::default());
        assert!(d.exists());
    }

    #[test]
    fn an_old_entry_the_caller_keeps_survives() {
        let t = testkit::TempDir::new("retention-keep");
        file(t.path(), "open", 8);
        file(t.path(), "shut", 8);
        let p = prune(t.path(), 7, 50, NOW, &|n| n == "open");
        assert_eq!(p, Pruned { removed: 1, left: 0 });
        assert!(t.path().join("open").exists() && !t.path().join("shut").exists());
    }

    #[test]
    fn one_call_removes_at_most_max_oldest_first() {
        let t = testkit::TempDir::new("retention-max");
        for (i, d) in [20u64, 10, 12, 9, 15].iter().enumerate() {
            file(t.path(), &format!("e{i}"), *d);
        }
        assert_eq!(prune(t.path(), 7, 2, NOW, &none), Pruned { removed: 2, left: 3 });
        assert!(!t.path().join("e0").exists() && !t.path().join("e4").exists());
        assert!(t.path().join("e1").exists());
    }

    #[test]
    fn rounds_keep_claims_only_own_repo_results_outside_the_open_set() {
        let open = vec!["spira-open".to_string(), "spira/b2".to_string()];
        let keep = rounds_keep("spira", &open);
        assert!(!keep("spira-old.results") && !keep("spira%2Fold.results"));
        assert!(keep("spira-open.results") && keep("spira%2Fb2.results"));
        assert!(keep("cubby-old.results") && keep("spira-old.result") && keep("spira-old.mender-x"));
    }

    #[test]
    fn a_missing_dir_returns_zeros() {
        let t = testkit::TempDir::new("retention-missing");
        assert_eq!(prune(&t.path().join("nope"), 7, 50, NOW, &none), Pruned::default());
    }
}
