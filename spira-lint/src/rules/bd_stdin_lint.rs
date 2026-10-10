//! `bd-stdin-lint` — refuse a bare "-" body passed to `bd note` or `bd create -d`.
//! Ported from `spira/bd-stdin-lint.sh` (sp-pppt0; defect sp-j5z3). Contract: DESIGN.md.
//!
//! `bd note <id> - <<'EOF'` records the literal "-" and discards the heredoc body: `bd note`
//! takes prose as positional arguments, so "-" is stored verbatim and exits 0. Likewise
//! `bd create ... -d - <<'EOF'`. The correct forms are `bd note <id> --stdin <<EOF` and
//! `bd create <title> --body-file - <<EOF`.

use crate::SyncCell as Cell;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct BdStdinLint {
    checked: Cell<Option<usize>>,
}

impl Default for BdStdinLint {
    fn default() -> Self {
        BdStdinLint { checked: Cell::new(None) }
    }
}

const NAME: &str = "bd-stdin-lint";

fn note_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"bd\s.*note\s.*\s-\s<<").expect("static regex"))
}

fn create_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"bd\s.*create\s.*(-d\s+-|-d-|--description\s+-)\s*(<|$|[^-])").expect("static regex")
    })
}

/// One pass per file: line:text per live (non-comment) hit of either shape.
fn scan(text: &str) -> Vec<(usize, String)> {
    let (note, create) = (note_re(), create_re());
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim_start().starts_with('#') {
            continue;
        }
        if note.is_match(l) || create.is_match(l) {
            out.push((i + 1, l.trim_start().to_string()));
        }
    }
    out
}

impl Rule for BdStdinLint {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        pathspec_match("spira/*", &e.path) || pathspec_match("chamber/*", &e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(c) = tree.content(e) else { continue };
            for (line, text) in scan(&String::from_utf8_lossy(c)) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
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
        "A bare \"-\" body stores the literal dash and discards the heredoc. Use the stdin \
forms instead: bd note <id> --stdin <<'EOF'  /  bd create <title> --body-file - <<'EOF'"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        BdStdinLint::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn both_bad_shapes_are_caught_in_spira_and_chamber() {
        let t = TempDir::new("bds");
        t.write("spira/planted.sh", "#!/usr/bin/env bash\nbd -C /db note sp-abc - <<'NOTE'\nsome text\nNOTE\n");
        t.write("chamber/planted.md", "bd -C {{DB}} create \"title\" -d - -l plan <<'BODY'\nprose here\nBODY\n");
        let got = run(&t, &["spira/planted.sh", "chamber/planted.md"]).unwrap();
        assert_eq!(
            got,
            vec![
                "bd-stdin-lint: chamber/planted.md:1: bd -C {{DB}} create \"title\" -d - -l plan <<'BODY'".to_string(),
                "bd-stdin-lint: spira/planted.sh:2: bd -C /db note sp-abc - <<'NOTE'".to_string(),
            ]
        );
    }

    #[test]
    fn the_stdin_forms_are_silent_beside_the_plants() {
        let t = TempDir::new("bds-ok");
        t.write(
            "spira/x.sh",
            "bd -C /db note sp-abc --stdin <<'NOTE'\nclean\nNOTE\nbd -C /db create \"title\" --body-file - -l plan <<'BODY'\nclean\nBODY\n",
        );
        assert!(run(&t, &["spira/x.sh"]).unwrap().is_empty());
    }

    #[test]
    fn a_commented_out_example_is_not_a_live_invocation() {
        let t = TempDir::new("bds-comment");
        t.write("spira/doc.sh", "# bd -C /db note sp-abc - <<EOF\n");
        assert!(run(&t, &["spira/doc.sh"]).unwrap().is_empty());
    }

    #[test]
    fn nested_paths_under_spira_and_chamber_are_in_scope() {
        let t = TempDir::new("bds-nested");
        t.write("spira/sub/x.sh", "bd -C /db note sp-abc - <<EOF\n");
        assert_eq!(run(&t, &["spira/sub/x.sh"]).unwrap().len(), 1);
    }

    #[test]
    fn a_clean_tree_reports_checked_and_an_empty_tree_refuses() {
        let t = TempDir::new("bds-clean");
        t.write("spira/x.sh", "echo clean\n");
        let tree = Tree::from_paths(t.path(), ["spira/x.sh"], std::iter::empty::<&str>());
        let r = BdStdinLint::default();
        assert_eq!(r.check(&tree), Ok(vec![]));
        assert_eq!(r.checked(), Some((1, "files".to_string())));

        let empty = TempDir::new("bds-empty");
        empty.write("README.md", "x\n");
        assert_eq!(run(&empty, &["README.md"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(BdStdinLint::default()),
    ]
}
