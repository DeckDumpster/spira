//! `testdb-mode-lint` — server-mode testdb requires a stated reason.
//! Ported from `spira/testdb-mode-lint.sh` (sp-pppt0). Contract: DESIGN.md.
//!
//! Embedded Dolt resets in ~5s; server mode pays a real dolt-beads-test round trip, median
//! 110s. A suite that pins `SPIRA_TESTDB_MODE=server` without saying why is
//! indistinguishable, at a glance, from one that needs the real engine. This fence requires
//! a header, `# testdb-mode: server — <reason>`, anywhere in the file, with non-empty text
//! after the em dash.

use crate::SyncCell as Cell;

use crate::{direct_child, Entry, Finding, LintError, Rule, Tree};

pub struct TestdbModeLint {
    checked: Cell<Option<usize>>,
}

impl Default for TestdbModeLint {
    fn default() -> Self {
        TestdbModeLint { checked: Cell::new(None) }
    }
}

const NAME: &str = "testdb-mode-lint";
const REQUEST: &str = "SPIRA_TESTDB_MODE=server";

/// `spira/test-*.sh`, directly in spira/.
fn is_suite(path: &str) -> bool {
    direct_child(path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
}

/// 1-based line numbers of every live (non-comment) request line.
fn request_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim_start().starts_with('#') && l.contains(REQUEST))
        .map(|(i, _)| i + 1)
        .collect()
}

/// A `# testdb-mode: server — <reason>` header, non-empty reason, anywhere in the file.
/// Mirrors the bash fence's `^#[[:space:]]*testdb-mode:[[:space:]]*server[[:space:]]*—[[:space:]]*[^[:space:]]`.
fn has_reason_header(text: &str) -> bool {
    text.lines().any(|l| {
        let Some(rest) = l.strip_prefix('#') else { return false };
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix("testdb-mode:") else { return false };
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix("server") else { return false };
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix('—') else { return false };
        let rest = rest.trim_start_matches([' ', '\t']);
        !rest.is_empty()
    })
}

impl Rule for TestdbModeLint {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_suite(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let suites = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &suites {
            let Some(c) = tree.content(e) else { continue };
            let text = String::from_utf8_lossy(c);
            let hits = request_lines(&text);
            if hits.is_empty() || has_reason_header(&text) {
                continue;
            }
            for line in hits {
                let raw = text.lines().nth(line - 1).unwrap_or_default();
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(line),
                    message: crate::trim_lead(raw.as_bytes()),
                });
            }
        }
        if out.is_empty() {
            self.checked.set(Some(suites.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "suites".to_string()))
    }

    fn hint(&self) -> &'static str {
        "Either flip to embedded (drop the SPIRA_TESTDB_MODE=server request) or say why the \
real engine is required: # testdb-mode: server — <reason> anywhere in the file. The two \
genuine reasons: concurrency across processes, or server-only SQL."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TestdbModeLint::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn an_unexplained_request_is_caught_and_named_by_line() {
        let t = TempDir::new("tdm");
        t.write("spira/test-planted.sh", "#!/usr/bin/env bash\nexport SPIRA_TESTDB_MODE=server\n");
        let got = run(&t, &["spira/test-planted.sh"]).unwrap();
        assert_eq!(
            got,
            vec!["testdb-mode-lint: spira/test-planted.sh:2: export SPIRA_TESTDB_MODE=server"]
        );
    }

    #[test]
    fn a_stated_reason_silences_it_while_the_request_stays() {
        let t = TempDir::new("tdm-ok");
        t.write(
            "spira/test-explained.sh",
            "#!/usr/bin/env bash\n# testdb-mode: server — needs real bd sql\nexport SPIRA_TESTDB_MODE=server\n",
        );
        assert!(run(&t, &["spira/test-explained.sh"]).unwrap().is_empty());
    }

    #[test]
    fn an_empty_reason_does_not_count() {
        let t = TempDir::new("tdm-empty");
        t.write(
            "spira/test-x.sh",
            "#!/usr/bin/env bash\n# testdb-mode: server —\nexport SPIRA_TESTDB_MODE=server\n",
        );
        assert_eq!(run(&t, &["spira/test-x.sh"]).unwrap().len(), 1);
    }

    #[test]
    fn a_commented_out_request_is_not_a_live_request() {
        let t = TempDir::new("tdm-comment");
        t.write("spira/test-x.sh", "#!/usr/bin/env bash\n# export SPIRA_TESTDB_MODE=server\n");
        assert!(run(&t, &["spira/test-x.sh"]).unwrap().is_empty());
    }

    #[test]
    fn an_inline_request_without_export_is_still_caught() {
        let t = TempDir::new("tdm-inline");
        t.write("spira/test-x.sh", "#!/usr/bin/env bash\nSPIRA_TESTDB_MODE=server testdb_up x\n");
        assert_eq!(run(&t, &["spira/test-x.sh"]).unwrap().len(), 1);
    }

    #[test]
    fn an_embedded_only_suite_is_unaffected_and_reports_checked() {
        let t = TempDir::new("tdm-clean");
        t.write("spira/test-a.sh", "#!/usr/bin/env bash\necho embedded-only\n");
        let tree = Tree::from_paths(t.path(), ["spira/test-a.sh"], std::iter::empty::<&str>());
        let r = TestdbModeLint::default();
        assert_eq!(r.check(&tree), Ok(vec![]));
        assert_eq!(r.checked(), Some((1, "suites".to_string())));
    }

    #[test]
    fn no_suites_in_scope_refuses() {
        let t = TempDir::new("tdm-none");
        t.write("spira/lib.sh", "echo\n");
        assert_eq!(run(&t, &["spira/lib.sh"]), Err(LintError::EmptyScope));
    }
}
