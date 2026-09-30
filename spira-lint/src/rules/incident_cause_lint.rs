//! `incident-cause-lint` — every `SPIRA_INCIDENT_REF` filing site also declares
//! `SPIRA_INCIDENT_CAUSE` (law-producers-declare-what-they-know).
//! Ported from `spira/incident-cause-lint.sh` (sp-pppt0). Contract: DESIGN.md.
//!
//! A producer that sets `SPIRA_INCIDENT_REF` without `SPIRA_INCIDENT_CAUSE` files
//! recurrences into the undifferentiated "unrecorded" bucket, collapsing the census
//! taxonomy. Every `SPIRA_INCIDENT_REF=` assignment must carry `SPIRA_INCIDENT_CAUSE`
//! somewhere in its surrounding 14-line window (10 before, 3 after).
//!
//! **Intended difference from the bash fence.** The bash fence computed its window with
//! `sed -n "$((l-10)),$((l+3))p"`; for a site on line 10 or earlier that is
//! `sed -n "-4,9p"` or similar, which GNU sed rejects as an unrecognised option (not a
//! line-address error), so the search silently sees nothing and the site is refused even
//! when its cause sits earlier in the file. This rule clamps the window to the file's
//! start instead, so a site near the top of a short file is not refused only for being
//! near the top.

use std::cell::Cell;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct IncidentCauseLint {
    checked: Cell<Option<usize>>,
}

impl Default for IncidentCauseLint {
    fn default() -> Self {
        IncidentCauseLint { checked: Cell::new(None) }
    }
}

const NAME: &str = "incident-cause-lint";
const REF: &str = "SPIRA_INCIDENT_REF=";
const CAUSE: &str = "SPIRA_INCIDENT_CAUSE";

/// `spira/*.sh`, any depth, excluding suites (basename starting `test-`) — the bash fence's
/// `grep -rn --include='*.sh' | grep -v '/test-'` over its own directory.
fn is_scoped(path: &str) -> bool {
    if !crate::pathspec_match("spira/*.sh", path) {
        return false;
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    !base.starts_with("test-")
}

/// 1-based line numbers of every `SPIRA_INCIDENT_REF=` site with no `SPIRA_INCIDENT_CAUSE`
/// in its surrounding window. A site is code, not prose: a `#`-comment line, or an
/// assignment quoted in a string before the `=`, is not a live site
/// (law-a-matcher-reads-code-not-prose).
fn undeclared(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with('#') {
            continue;
        }
        let Some(idx) = l.find(REF) else { continue };
        let before = &l[..idx];
        if before.contains('"') || before.contains('\'') {
            continue;
        }
        let ln = i + 1;
        let start = ln.saturating_sub(10).max(1);
        let end = (ln + 3).min(lines.len());
        if !lines[start - 1..end].iter().any(|w| w.contains(CAUSE)) {
            out.push(ln);
        }
    }
    out
}

impl Rule for IncidentCauseLint {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        is_scoped(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(c) = tree.content(e) else { continue };
            for line in undeclared(&String::from_utf8_lossy(c)) {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(line),
                    message: "SPIRA_INCIDENT_REF= with no SPIRA_INCIDENT_CAUSE in its surrounding 14 lines".into(),
                });
            }
        }
        if out.is_empty() {
            self.checked.set(Some(files.len()));
        }
        Ok(out)
    }

    fn checked(&self) -> Option<(usize, String)> {
        self.checked.get().map(|n| (n, "files".to_string()))
    }

    fn hint(&self) -> &'static str {
        "Every SPIRA_INCIDENT_REF filing site declares SPIRA_INCIDENT_CAUSE beside it, so the \
census taxonomy can tell causes apart instead of collapsing into \"unrecorded\"."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        IncidentCauseLint::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_offender_is_refused_by_file_and_line() {
        let t = TempDir::new("icl");
        // Padded so the site sits past line 10 — the ordinary, non-edge-case shape.
        let mut body = "#!/usr/bin/env bash\n".repeat(11);
        body.push_str("SPIRA_INCIDENT_REF=incident:planted-offender\n");
        t.write("spira/offender.sh", &body);
        let got = run(&t, &["spira/offender.sh"]).unwrap();
        assert_eq!(got, vec!["incident-cause-lint: spira/offender.sh:12: SPIRA_INCIDENT_REF= with no SPIRA_INCIDENT_CAUSE in its surrounding 14 lines"]);
    }

    #[test]
    fn a_declared_site_is_clean() {
        let t = TempDir::new("icl-ok");
        t.write(
            "spira/declared.sh",
            "#!/usr/bin/env bash\nSPIRA_INCIDENT_CAUSE=y \\\nSPIRA_INCIDENT_REF=x \\\nbash incident.sh\n",
        );
        assert!(run(&t, &["spira/declared.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_site_near_the_top_of_a_short_file_still_finds_a_nearby_cause() {
        // This is the clamped-window fix: line 2 has no 10 lines before it, but the cause
        // two lines earlier is still in the (clamped) window.
        let t = TempDir::new("icl-top");
        t.write("spira/x.sh", "SPIRA_INCIDENT_CAUSE=y\nSPIRA_INCIDENT_REF=x\n");
        assert!(run(&t, &["spira/x.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_comment_quoting_the_assignment_is_not_a_site() {
        let t = TempDir::new("icl-comment");
        t.write(
            "spira/commented.sh",
            "# fixed by rewording the comment that used to quote\n# SPIRA_INCIDENT_REF=\"closed-not-landed:$id\" here\ndo_thing() { :; }\n",
        );
        assert!(run(&t, &["spira/commented.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_quoted_reference_before_the_assignment_is_not_a_site() {
        let t = TempDir::new("icl-quoted");
        t.write("spira/x.sh", "echo \"see SPIRA_INCIDENT_REF=foo in the docs\"\n");
        assert!(run(&t, &["spira/x.sh"]).unwrap().is_empty());
    }

    #[test]
    fn suites_and_non_sh_files_are_out_of_scope() {
        let t = TempDir::new("icl-scope");
        t.write("spira/test-x.sh", "SPIRA_INCIDENT_REF=x\n");
        t.write("spira/notes.md", "SPIRA_INCIDENT_REF=x\n");
        assert_eq!(run(&t, &["spira/test-x.sh", "spira/notes.md"]), Err(LintError::EmptyScope));
    }

    #[test]
    fn a_clean_tree_reports_checked() {
        let t = TempDir::new("icl-clean");
        t.write("spira/a.sh", "echo\n");
        let tree = Tree::from_paths(t.path(), ["spira/a.sh"], std::iter::empty::<&str>());
        let r = IncidentCauseLint::default();
        assert_eq!(r.check(&tree), Ok(vec![]));
        assert_eq!(r.checked(), Some((1, "files".to_string())));
    }
}
