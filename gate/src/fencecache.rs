//! The fences verdict, cached by tree id. A fences-only trial is a pure function of the tree,
//! the harness and the command it ran, so a PASS for tree T is reused by any later gate on T
//! (the branch's trial or the base's). `<verdicts>/fences/<repo>/<tree>`; the harness and the
//! command's hash are checked on every read, and anything unparseable is a miss.

use std::path::{Path, PathBuf};

const SEP: &str = "---\n";

#[derive(Clone, Debug)]
pub struct Key {
    pub path: PathBuf,
    pub harness: String,
}

pub fn path(verdicts: &Path, repo: &str, tree: &str) -> Option<PathBuf> {
    (crate::cert::is_repo_name(repo) && crate::cert::is_object_id(tree))
        .then(|| verdicts.join("fences").join(repo).join(tree))
}

fn cmd_id(cmd: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in cmd.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub fn render(harness: &str, cmd: &str, out: &str) -> String {
    format!("verdict=PASS\nharness={harness}\ncmd={}\n{SEP}{out}", cmd_id(cmd))
}

/// The proved output of a PASS recorded under this harness for this command.
pub fn fresh(entry: &str, harness: &str, cmd: &str) -> Option<String> {
    if harness.is_empty() {
        return None;
    }
    let (head, out) = entry.split_once(SEP)?;
    let want = [
        "verdict=PASS".to_string(),
        format!("harness={harness}"),
        format!("cmd={}", cmd_id(cmd)),
    ];
    head.lines().eq(want.iter().map(String::as_str)).then(|| out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_reused_only_under_the_same_harness_and_command() {
        let e = render("h", "c1", "fence: x checked 3 files");
        assert_eq!(fresh(&e, "h", "c1").as_deref(), Some("fence: x checked 3 files"));
        assert_eq!(fresh(&e, "h2", "c1"), None);
        assert_eq!(fresh(&e, "h", "c2"), None);
        assert_eq!(fresh(&e, "", "c1"), None);
        assert_eq!(fresh("garbage", "h", "c1"), None);
    }

    #[test]
    fn only_a_resolved_tree_and_a_safe_repo_make_a_path() {
        let v = Path::new("/v");
        assert!(path(v, "spira", &"a".repeat(40)).is_some());
        assert!(path(v, "spira", "-").is_none());
        assert!(path(v, "../x", &"a".repeat(40)).is_none());
    }
}
