//! A red verdict, remembered by gate key. The key is the merged tree, the changed files, the
//! command and the harness, so a second gate with the same key is a repeat and answers what
//! the first did. Only a verdict that names whose fault the red is (`branch-red`, `base-red`)
//! is stored; a NO_VERDICT judged nothing and is always re-run. `<verdicts>/red/<key>`;
//! anything unparseable or out of date is a miss.

use std::path::{Path, PathBuf};

const SEP: &str = "---\n";

pub fn path(verdicts: &Path, key: &str) -> Option<PathBuf> {
    (!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| verdicts.join("red").join(key))
}

pub fn recordable(status: i32, reason: &str) -> bool {
    matches!((status, reason), (crate::engine::FAIL, "branch-red") | (crate::engine::BASEFAIL, "base-red"))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub status: i32,
    pub reason: String,
    pub suite: String,
    pub when: String,
    pub by: String,
    pub msg: String,
}

#[allow(clippy::too_many_arguments)]
pub fn render(status: i32, reason: &str, suite: &str, when: &str, at: u64, by: &str, msg: &str) -> String {
    format!("status={status}\nreason={reason}\nsuite={suite}\nwhen={when}\nat={at}\nby={by}\n{SEP}{msg}")
}

pub fn fresh(entry: &str, ttl: &str, now: u64) -> Option<Entry> {
    let (head, msg) = entry.split_once(SEP)?;
    let (when, by) = crate::key::cache_fresh(head, ttl, now)?;
    let field = |k: &str| head.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(str::to_string);
    let status: i32 = field("status")?.parse().ok()?;
    let reason = field("reason")?;
    recordable(status, &reason).then(|| Entry { status, reason, suite: field("suite").unwrap_or_default(), when, by, msg: msg.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BASEFAIL, FAIL, NOVERDICT};

    #[test]
    fn a_fresh_red_comes_back_whole_and_a_stale_or_unattributed_one_does_not() {
        let e = render(FAIL, "branch-red", "test-x.sh", "t", 100, "me", "gate: boom\n");
        let got = fresh(&e, "50", 120).unwrap();
        assert_eq!((got.status, got.reason.as_str(), got.suite.as_str(), got.msg.as_str()), (FAIL, "branch-red", "test-x.sh", "gate: boom\n"));
        assert!(fresh(&e, "50", 150).is_none());
        assert!(fresh(&e, "", 120).is_none());
        assert!(fresh("garbage", "50", 120).is_none());
        let nv = render(NOVERDICT, "died", "-", "t", 100, "me", "m");
        assert!(fresh(&nv, "50", 120).is_none());
        let bf = render(BASEFAIL, "base-red", "-", "t", 100, "me", "m");
        assert!(fresh(&bf, "50", 120).is_some());
    }

    #[test]
    fn only_a_plain_key_makes_a_path() {
        assert!(path(Path::new("/v"), "abc123").is_some());
        assert!(path(Path::new("/v"), "../x").is_none());
        assert!(path(Path::new("/v"), "").is_none());
    }
}
