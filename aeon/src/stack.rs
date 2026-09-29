//! The claim's stack proposal (design stacked-dependents-2026-09-28 §1/§4.1): parsing
//! `spira-claim stack <id>`'s answer, and building the merged base commit it implies. Every
//! git call goes through `merge-tree --write-tree` + `commit-tree` — no working tree is ever
//! touched, so a conflict here leaves nothing to clean up and nothing shared is disturbed.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ports::Git;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Proposal {
    pub claimable: bool,
    pub stack: BTreeMap<String, String>,
    pub stack_depth: u32,
    pub stack_max_depth: u32,
}

/// Parses `spira-claim stack <id>`'s stdout — its own JSON object regardless of exit code
/// (0 claimable, 3 refused; see `spira-claim`'s own doc). `None` for anything else (a
/// truncated write, an incompatible version, the binary missing entirely): the caller's own
/// fallback is an empty, unstacked proposal, never a crash.
pub fn parse_proposal(text: &str) -> Option<Proposal> {
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let claimable = v.get("claimable")?.as_bool()?;
    let stack = v
        .get("stack")
        .and_then(|s| s.as_object())
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();
    let stack_depth = v.get("stack_depth").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    let stack_max_depth = v.get("stack_max_depth").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    Some(Proposal { claimable, stack, stack_depth, stack_max_depth })
}

/// The two prerequisites (or "the landing ref" for the first merge in the chain) whose
/// tips conflicted while chaining the stack onto the base — "stack conflict: <a> x <b>" is
/// the design's own note format, given to both prerequisites named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub a: String,
    pub b: String,
}

impl Conflict {
    pub fn note(&self) -> String {
        format!("stack conflict: {} x {}", self.a, self.b)
    }
}

/// Chains every `(prereq, tip)` in `stack` onto `base_fq`, in the map's own deterministic
/// (prerequisite-id) order, via `git merge-tree --write-tree` + `commit-tree`. `Ok(base_fq)`
/// unchanged when `stack` is empty. Each certified tip already carries its own
/// prerequisites merged in (it was built the same way), so only the direct entries named
/// here are ever merged — never a transitive walk.
pub fn build_stacked_base(git: &dyn Git, repo: &Path, base_fq: &str, stack: &BTreeMap<String, String>) -> Result<String, Conflict> {
    let mut cur = base_fq.to_string();
    let mut last_label = "the landing ref".to_string();
    for (prereq, tip) in stack {
        let mt = git.git(repo, &["merge-tree", "--write-tree", &cur, tip]);
        let tree = mt.stdout.lines().next().unwrap_or("").trim().to_string();
        if !mt.success() || tree.is_empty() {
            return Err(Conflict { a: last_label, b: prereq.clone() });
        }
        let msg = format!("stacked base: merge {prereq} ({tip}) onto {cur}");
        let ct = git.git(repo, &["commit-tree", &tree, "-p", &cur, "-p", tip, "-m", &msg]);
        if !ct.success() {
            return Err(Conflict { a: last_label, b: prereq.clone() });
        }
        cur = ct.text().trim().to_string();
        last_label = prereq.clone();
    }
    Ok(cur)
}

/// `{"a":"sha","b":"sha"}` — the stack's evidence for the `Claim` event's `--kind` (no
/// serde_json dependency on the caller's part beyond what it already carries as a string).
pub fn stack_json(stack: &BTreeMap<String, String>) -> String {
    serde_json::to_string(stack).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Out;
    use std::path::PathBuf;

    #[test]
    fn parse_proposal_reads_claimable_and_stack() {
        let p = parse_proposal(r#"{"claimable":true,"stack":{"sp-a":"tipA"},"stack_depth":1,"stack_max_depth":4}"#).unwrap();
        assert!(p.claimable);
        assert_eq!(p.stack.get("sp-a").map(String::as_str), Some("tipA"));
        assert_eq!((p.stack_depth, p.stack_max_depth), (1, 4));
    }

    #[test]
    fn parse_proposal_too_deep_still_carries_the_depth() {
        let p = parse_proposal(r#"{"claimable":false,"reason":"TooDeep","stack":{},"stack_depth":5,"stack_max_depth":4}"#).unwrap();
        assert!(!p.claimable);
        assert!(p.stack.is_empty());
        assert_eq!((p.stack_depth, p.stack_max_depth), (5, 4));
    }

    #[test]
    fn parse_proposal_garbage_is_none() {
        assert!(parse_proposal("not json").is_none());
        assert!(parse_proposal("").is_none());
    }

    struct FakeGit {
        calls: std::sync::Mutex<Vec<Vec<String>>>,
        answers: std::sync::Mutex<Vec<Out>>,
    }
    impl Git for FakeGit {
        fn git(&self, _d: &Path, args: &[&str]) -> Out {
            self.calls.lock().unwrap().push(args.iter().map(|s| s.to_string()).collect());
            self.answers.lock().unwrap().remove(0)
        }
    }
    fn fg(answers: Vec<Out>) -> FakeGit {
        FakeGit { calls: std::sync::Mutex::new(vec![]), answers: std::sync::Mutex::new(answers) }
    }

    #[test]
    fn empty_stack_is_the_base_unchanged() {
        let git = fg(vec![]);
        assert_eq!(build_stacked_base(&git, &PathBuf::from("/r"), "origin/main", &BTreeMap::new()), Ok("origin/main".into()));
        assert!(git.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn one_stacked_tip_merges_cleanly() {
        let git = fg(vec![Out::ok("treeoid\n"), Out::ok("mergesha\n")]);
        let mut stack = BTreeMap::new();
        stack.insert("sp-a".to_string(), "tipA".to_string());
        assert_eq!(build_stacked_base(&git, &PathBuf::from("/r"), "origin/main", &stack), Ok("mergesha".into()));
        let calls = git.calls.lock().unwrap();
        assert_eq!(calls[0], vec!["merge-tree", "--write-tree", "origin/main", "tipA"]);
        assert_eq!(calls[1], vec!["commit-tree", "treeoid", "-p", "origin/main", "-p", "tipA", "-m", "stacked base: merge sp-a (tipA) onto origin/main"]);
    }

    #[test]
    fn a_conflicting_second_tip_names_the_first_stacked_prereq_and_itself() {
        let git = fg(vec![Out::ok("tree1\n"), Out::ok("merge1\n"), Out::fail(1, "CONFLICT")]);
        let mut stack = BTreeMap::new();
        stack.insert("sp-a".to_string(), "tipA".to_string());
        stack.insert("sp-b".to_string(), "tipB".to_string());
        let err = build_stacked_base(&git, &PathBuf::from("/r"), "origin/main", &stack).unwrap_err();
        assert_eq!(err, Conflict { a: "sp-a".into(), b: "sp-b".into() });
        assert_eq!(err.note(), "stack conflict: sp-a x sp-b");
    }

    #[test]
    fn the_first_tip_conflicting_with_base_names_the_landing_ref() {
        let git = fg(vec![Out::fail(1, "CONFLICT")]);
        let mut stack = BTreeMap::new();
        stack.insert("sp-a".to_string(), "tipA".to_string());
        let err = build_stacked_base(&git, &PathBuf::from("/r"), "origin/main", &stack).unwrap_err();
        assert_eq!(err.note(), "stack conflict: the landing ref x sp-a");
    }
}
