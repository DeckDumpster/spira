//! `script-exec` — every operator-runnable `spira/*.sh` carries the execute bit.
//! Ported from `spira/test-script-exec.sh`. Contract: DESIGN.md.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use crate::{direct_child, lines, Entry, Finding, LintError, Rule, Tree};

pub struct ScriptExec;

const NAME: &str = "script-exec";
/// The header declaration that makes a script legitimately non-executable.
const SOURCED_ONLY: &str = "Sourced, never executed";

/// True when the first 10 lines declare the file sourced-only.
pub fn declares_sourced_only(content: &[u8]) -> bool {
    lines(content)
        .iter()
        .take(10)
        .any(|l| String::from_utf8_lossy(l).contains(SOURCED_ONLY))
}

impl Rule for ScriptExec {
    fn name(&self) -> &'static str {
        NAME
    }

    /// `spira/*.sh` directly in spira/, minus the `test-*` suites.
    fn applies_to(&self, e: &Entry) -> bool {
        direct_child(&e.path, "spira").is_some_and(|b| b.ends_with(".sh") && !b.starts_with("test-"))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let mut out = Vec::new();
        for e in crate::scope(tree, self)? {
            let Some(c) = tree.content(e) else { continue };
            if declares_sourced_only(c) {
                continue;
            }
            let exec = fs::metadata(tree.root.join(&e.path))
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            if !exec {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: None,
                    message: "not executable — chmod +x it, or declare \"Sourced, never executed\" in its header".into(),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "An operator command without +x ships silent. A library states \"Sourced, never executed\" \
in its first 10 lines instead; the header is the declaration, so no list can drift from it."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn chmod(t: &TempDir, rel: &str, mode: u32) {
        fs::set_permissions(t.path().join(rel), fs::Permissions::from_mode(mode)).unwrap();
    }

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ScriptExec.check(&tree).map(|v| v.iter().map(|f| f.path.clone()).collect())
    }

    #[test]
    fn a_planted_non_executable_script_is_caught_then_clears_with_x() {
        let t = TempDir::new("sx");
        t.write("spira/canary-operator.sh", "#!/usr/bin/env bash\ntrue\n");
        chmod(&t, "spira/canary-operator.sh", 0o644);
        assert_eq!(run(&t, &["spira/canary-operator.sh"]).unwrap(), vec!["spira/canary-operator.sh"]);
        chmod(&t, "spira/canary-operator.sh", 0o755);
        assert!(run(&t, &["spira/canary-operator.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_sourced_only_header_and_suites_are_exempt() {
        let t = TempDir::new("sx-src");
        t.write("spira/fake-lib.sh", "# fake-lib.sh — a helper.\n# Sourced, never executed.\ntrue\n");
        t.write("spira/test-foo.sh", "true\n");
        t.write("spira/late.sh", &format!("{}# Sourced, never executed.\n", "#\n".repeat(10)));
        for f in ["spira/fake-lib.sh", "spira/test-foo.sh", "spira/late.sh"] {
            chmod(&t, f, 0o644);
        }
        // the declaration counts only within the first 10 lines
        assert_eq!(
            run(&t, &["spira/fake-lib.sh", "spira/test-foo.sh", "spira/late.sh"]).unwrap(),
            vec!["spira/late.sh"]
        );
    }

    #[test]
    fn an_empty_scope_is_refused() {
        let t = TempDir::new("sx-empty");
        assert_eq!(run(&t, &["spira/test-a.sh", "spira/sub/b.sh"]), Err(LintError::EmptyScope));
    }
}
