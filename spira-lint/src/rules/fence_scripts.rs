//! `fence-scripts` — no new bash fence or lint script: every new fence is a spira-lint rule.
//! Contract: DESIGN.md.

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct FenceScripts;

const NAME: &str = "fence-scripts";
const ALLOW_FILE: &str = "spira-lint/fence-scripts-allow";

/// `spira-lint/fence-scripts-allow`: exact paths, each with the line it sits on.
pub struct FenceScriptAllow(Vec<(usize, String)>);

impl FenceScriptAllow {
    pub fn parse(text: &str) -> FenceScriptAllow {
        FenceScriptAllow(
            text.lines()
                .enumerate()
                .filter(|(_, l)| {
                    let t = l.trim_start();
                    !(t.is_empty() || t.starts_with('#'))
                })
                .map(|(i, l)| (i + 1, l.trim().to_string()))
                .collect(),
        )
    }
    pub fn covers(&self, path: &str) -> bool {
        self.0.iter().any(|(_, p)| p == path)
    }
}

/// `spira/<name>-fence.sh` or `spira/<name>-lint.sh`, directly in spira/.
pub fn is_fence_script(path: &str) -> bool {
    path.strip_prefix("spira/")
        .is_some_and(|rest| !rest.contains('/') && (rest.ends_with("-fence.sh") || rest.ends_with("-lint.sh")))
}

impl Rule for FenceScripts {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_fence_script(&e.path)
    }

    /// An empty scope is the goal here, not a refusal: the day the last bash fence is
    /// ported, nothing matches. The positive control is the stale-entry check instead.
    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let allow = FenceScriptAllow::parse(&tree.read_text(ALLOW_FILE));
        let mut out = Vec::new();
        for e in tree.entries.iter().filter(|e| self.applies_to(e)) {
            if !allow.covers(&e.path) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: None,
                    message: "a new bash fence/lint script — write it as a spira-lint rule instead".to_string(),
                });
            }
        }
        for (line, p) in &allow.0 {
            let present = tree.entries.iter().any(|e| e.path == *p && tree.root.join(p).is_file());
            if !present {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("lists {p}, which no longer exists — remove the line (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A new fence is a spira-lint Rule (spira-lint/src/rules/), not a bash script. \
spira-lint/fence-scripts-allow only shrinks: remove a line when its fence is ported and deleted."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, tracked: &[&str], untracked: &[&str]) -> Vec<String> {
        let tree = Tree::from_paths(t.path(), tracked.iter().copied(), untracked.iter().copied());
        FenceScripts.check(&tree).unwrap().iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn scope_is_direct_children_of_spira() {
        assert!(is_fence_script("spira/foo-fence.sh"));
        assert!(is_fence_script("spira/test-foo-lint.sh"));
        assert!(!is_fence_script("spira/hooks/aeon-fence.sh"));
        assert!(!is_fence_script("spira/gate-fences.sh"));
        assert!(!is_fence_script("other/foo-lint.sh"));
    }

    #[test]
    fn a_new_script_is_refused_a_listed_one_is_not() {
        let t = TempDir::new("fs");
        t.write("spira/old-fence.sh", "");
        t.write("spira/new-lint.sh", "");
        t.write(ALLOW_FILE, "# header\n\nspira/old-fence.sh\n");
        assert_eq!(
            run(&t, &["spira/old-fence.sh"], &["spira/new-lint.sh"]),
            vec!["fence-scripts: spira/new-lint.sh: a new bash fence/lint script — write it as a spira-lint rule instead"]
        );
        t.remove("spira/new-lint.sh");
        assert!(run(&t, &["spira/old-fence.sh"], &[]).is_empty());
    }

    #[test]
    fn a_stale_entry_is_refused_so_the_list_shrinks() {
        let t = TempDir::new("fs-stale");
        t.write(ALLOW_FILE, "spira/gone-fence.sh\n");
        assert_eq!(
            run(&t, &[], &[]),
            vec!["fence-scripts: spira-lint/fence-scripts-allow:1: lists spira/gone-fence.sh, which no longer exists — remove the line (the list only shrinks)"]
        );
    }
}
