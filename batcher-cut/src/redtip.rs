//! The tips a red was attributed to. A member whose tip is unchanged since such a red is left out
//! of the cut, whether or not the eject that followed was accepted (law-a-retry-must-change-an-input).

use std::fs;
use std::path::{Path, PathBuf};

use batcher::core::Member;

fn file(run: &Path, repo: &str, id: &str) -> PathBuf {
    run.join("batcher-red-tips").join(repo).join(id)
}

fn read(run: &Path, repo: &str, id: &str) -> Option<(String, bool)> {
    let text = fs::read_to_string(file(run, repo, id)).ok()?;
    let mut f = text.split_whitespace();
    Some((f.next()?.to_string(), f.next() == Some("filed")))
}

/// Records that a red was attributed to `id` at `tip`. A new tip replaces the old record.
pub fn record(run: &Path, repo: &str, id: &str, tip: &str) {
    if read(run, repo, id).is_some_and(|(t, _)| t == tip) {
        return;
    }
    let path = file(run, repo, id);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(e) = fs::write(&path, format!("{tip}\n")) {
        eprintln!("batcher {repo}: red at {tip} of {id} not recorded ({e}); it may be cut again unchanged");
    }
}

/// Marks `id`'s record filed, once its note has been left.
pub fn mark_filed(run: &Path, repo: &str, id: &str, tip: &str) {
    if let Err(e) = fs::write(file(run, repo, id), format!("{tip} filed\n")) {
        eprintln!("batcher {repo}: {id} not marked filed ({e}); it will be noted again");
    }
}

/// Splits `pool` into the members to cut and the ids withheld because their tip is the one a red
/// was attributed to. A withheld id comes back with `true` the first time only, which is when it is to be filed.
pub fn withhold(
    run: &Path,
    repo: &str,
    pool: Vec<Member>,
) -> (Vec<Member>, Vec<(String, String, bool)>) {
    let mut kept = Vec::new();
    let mut withheld = Vec::new();
    for m in pool {
        match read(run, repo, &m.id) {
            Some((tip, filed)) if tip == m.tip => {
                withheld.push((m.id, tip, !filed));
            }
            Some(_) => {
                let _ = fs::remove_file(file(run, repo, &m.id));
                kept.push(m);
            }
            None => kept.push(m),
        }
    }
    (kept, withheld)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, tip: &str) -> Member {
        Member {
            id: id.into(),
            tip: tip.into(),
            title: String::new(),
            priority: None,
            express: false,
            base_fix: false,
            certified_at: 0,
            stack: Default::default(),
            blocked_by: vec![],
        }
    }

    #[test]
    fn a_member_at_the_tip_a_red_was_attributed_to_is_left_out_and_filed_once() {
        let run = testkit::TempDir::new("batcher-cut-redtip");
        record(run.path(), "r", "sp-a", "t1");
        let pool = || vec![member("sp-a", "t1"), member("sp-b", "t2")];
        let (kept, out) = withhold(run.path(), "r", pool());
        assert_eq!(
            kept.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["sp-b"]
        );
        assert_eq!(out, [("sp-a".to_string(), "t1".to_string(), true)]);
        mark_filed(run.path(), "r", "sp-a", "t1");
        let (_, again) = withhold(run.path(), "r", pool());
        assert_eq!(
            again,
            [("sp-a".to_string(), "t1".to_string(), false)],
            "filed once, not every cut"
        );
    }

    #[test]
    fn a_new_tip_or_another_repo_is_cut() {
        let run = testkit::TempDir::new("batcher-cut-redtip-moved");
        record(run.path(), "r", "sp-a", "t1");
        assert_eq!(
            withhold(run.path(), "r", vec![member("sp-a", "t9")])
                .0
                .len(),
            1,
            "a changed tip is a changed input"
        );
        assert_eq!(
            withhold(run.path(), "other", vec![member("sp-a", "t1")])
                .0
                .len(),
            1
        );
        assert!(
            !file(run.path(), "r", "sp-a").exists(),
            "a record the tip has moved past is dropped"
        );
        record(run.path(), "r", "sp-a", "t9");
        record(run.path(), "r", "sp-a", "t10");
        assert_eq!(
            withhold(run.path(), "r", vec![member("sp-a", "t9")])
                .0
                .len(),
            1,
            "a later record replaces the earlier"
        );
    }
}
