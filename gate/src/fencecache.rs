//! The fences verdict, cached by tree id. A fences-only trial is a function of the tree, the
//! harness, the command it ran, the deny list and which of its `test:` rows the gated item
//! owns (lint lets an item delete its own test). A PASS for tree T is reused by any later gate
//! on T that matches (the branch's trial or the base's). `<verdicts>/fences/<repo>/<tree>`;
//! all but the tree and repo are checked on every read, and anything unparseable is a miss.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use spira_config::unit_tier::DenyList;

const SEP: &str = "---\n";

#[derive(Clone, Debug)]
pub struct Key {
    pub path: PathBuf,
    pub harness: String,
    /// [`deny_id`] of the deny list the fences are handed.
    pub deny: String,
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

/// Names the owned ids rather than the bead, so items that own no row share entries.
pub fn deny_id(list: &DenyList, bead: &str) -> String {
    let own: BTreeSet<&str> = list.test_rows().into_iter().filter(|(_, _, owner)| *owner == bead).map(|(_, id, _)| id).collect();
    let own = if own.is_empty() { "-".to_string() } else { Vec::from_iter(own).join(",") };
    format!("{} own={own}", crate::key::sha256_hex(list.render().as_bytes()))
}

pub fn render(harness: &str, cmd: &str, deny: &str, out: &str) -> String {
    format!("verdict=PASS\nharness={harness}\ncmd={}\ndeny={deny}\n{SEP}{out}", cmd_id(cmd))
}

/// The proved output of a PASS recorded under this harness and deny list for this command.
pub fn fresh(entry: &str, harness: &str, cmd: &str, deny: &str) -> Option<String> {
    if harness.is_empty() {
        return None;
    }
    let (head, out) = entry.split_once(SEP)?;
    let want = [
        "verdict=PASS".to_string(),
        format!("harness={harness}"),
        format!("cmd={}", cmd_id(cmd)),
        format!("deny={deny}"),
    ];
    head.lines().eq(want.iter().map(String::as_str)).then(|| out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t3nh7tp)]
    #[test]
    fn an_entry_is_reused_only_under_the_same_harness_and_command() {
        let e = render("h", "c1", "d1", "fence: x checked 3 files");
        assert_eq!(fresh(&e, "h", "c1", "d1").as_deref(), Some("fence: x checked 3 files"));
        assert_eq!(fresh(&e, "h2", "c1", "d1"), None);
        assert_eq!(fresh(&e, "h", "c2", "d1"), None);
        assert_eq!(fresh(&e, "h", "c1", "d2"), None);
        assert_eq!(fresh(&e, "", "c1", "d1"), None);
        assert_eq!(fresh("garbage", "h", "c1", "d1"), None);
        assert_eq!(fresh(&e.replace("deny=d1\n", ""), "h", "c1", "d1"), None);
    }

    #[test_id(tndk6uh)]
    #[test]
    fn only_a_resolved_tree_and_a_safe_repo_make_a_path() {
        let v = Path::new("v");
        assert!(path(v, "spira", &"a".repeat(40)).is_some());
        assert!(path(v, "spira", "-").is_none());
        assert!(path(v, "../x", &"a".repeat(40)).is_none());
    }
}
