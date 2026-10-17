//! `journal-reads` — no shipped code reads journald (law-nothing-reads-the-journal). Contract:
//! DESIGN.md.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::rust_test::{shape, whole_test_files};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct JournalReads;

const NAME: &str = "journal-reads";
const ALLOW_FILE: &str = "spira-lint/journal-readers-allow";

fn reader() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)journalctl|sd_journal|sd-journal|libsystemd|systemd::journal|systemd-journal|journald|python-systemd").expect("static regex"))
}

fn exempt_path(path: &str) -> bool {
    path.starts_with("spira-lint/")
        || path.ends_with(".md")
        || path.ends_with(".txt")
        || path.split('/').any(|c| matches!(c, "tests" | "testlib" | "docs" | "fixtures" | "target") || c.starts_with("test-"))
}

fn parse_allow(text: &str) -> Result<BTreeMap<String, usize>, LintError> {
    let mut m = BTreeMap::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = t.split_whitespace().collect();
        if f.len() != 2 || !f[1].starts_with("sp-") {
            return Err(LintError::BadAllow { file: ALLOW_FILE.to_string(), line: i + 1, reason: format!("want `<path> <converting-bead>`, got `{t}`") });
        }
        m.insert(f[0].to_string(), i + 1);
    }
    Ok(m)
}

fn first_hit(src: &[u8], regions: &[(usize, usize)], whole: bool) -> Option<usize> {
    if whole {
        return None;
    }
    reader().find_iter(src).map(|m| m.start()).find(|at| !regions.iter().any(|(a, b)| a <= at && at < b))
}

impl Rule for JournalReads {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        !exempt_path(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let allow = parse_allow(&tree.read_text(ALLOW_FILE))?;
        let shapes: Vec<(&Entry, crate::rust_test::Shape)> =
            files.iter().filter(|e| e.path.ends_with(".rs")).filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);
        let mut by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for e in &files {
            let Some(src) = tree.content(e) else { continue };
            let regions = shapes.iter().find(|(x, _)| x.path == e.path).map(|(_, s)| s.regions.clone()).unwrap_or_default();
            if let Some(at) = first_hit(src, &regions, whole.contains(&e.path)) {
                by.insert(e.path.clone(), (src[..at].iter().filter(|b| **b == b'\n').count() + 1, at));
            }
        }
        let mut out = Vec::new();
        for (path, (line, _)) in &by {
            if !allow.contains_key(path) {
                out.push(Finding {
                    rule: NAME,
                    path: path.clone(),
                    line: Some(*line),
                    message: "reads the journal (law-nothing-reads-the-journal): take it from the component that produced it".to_string(),
                });
            }
        }
        for (path, line) in &allow {
            if !by.contains_key(path) {
                out.push(Finding {
                    rule: NAME,
                    path: path.clone(),
                    line: None,
                    message: format!("{ALLOW_FILE}:{line} allows {path}, which no longer reads the journal — remove the line (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "law-nothing-reads-the-journal: the journal is a log for humans. Get the fact from the component \
that produced it: a systemd D-Bus event, the producer's own record, or a pushed metric. Tests may read \
a journal only to assert on a fixture they wrote. spira-lint/journal-readers-allow only shrinks."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        JournalReads.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_journalctl_call_in_a_non_allowlisted_file_is_flagged() {
        let t = TempDir::new("jr-fail");
        t.write(ALLOW_FILE, "");
        t.write("spira/probe.sh", "#!/bin/bash\njournalctl --user -u x -n 5\n");
        t.write("a/src/lib.rs", "pub fn f() {\n    let _ = std::process::Command::new(\"journalctl\");\n}\n");
        let got = run(&t, &["spira/probe.sh", "a/src/lib.rs"]).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("journal-reads: a/src/lib.rs:2: reads the journal (law-nothing-reads-the-journal)"), "{got:?}");
        assert!(got[1].starts_with("journal-reads: spira/probe.sh:2:"), "{got:?}");
    }

    #[test]
    fn tests_prose_and_allowlisted_readers_pass() {
        let t = TempDir::new("jr-pass");
        t.write(ALLOW_FILE, "# h\nspira/old.sh sp-abc\n");
        t.write("spira/old.sh", "journalctl -n 1\n");
        t.write("spira/test-x.sh", "journalctl -n 1\n");
        t.write("a/tests/t.rs", "fn t() { let _ = \"journalctl\"; }\n");
        t.write("docs/j.md", "journalctl\n");
        t.write("a/src/lib.rs", "pub fn f() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { let _ = \"journalctl\"; }\n}\n");
        let files = ["spira/old.sh", "spira/test-x.sh", "a/tests/t.rs", "docs/j.md", "a/src/lib.rs"];
        assert_eq!(run(&t, &files).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_stale_or_malformed_allowance_is_refused() {
        let t = TempDir::new("jr-allow");
        t.write("spira/fixed.sh", "true\n");
        t.write(ALLOW_FILE, "spira/fixed.sh sp-abc\n");
        assert_eq!(
            run(&t, &["spira/fixed.sh"]).unwrap(),
            vec!["journal-reads: spira/fixed.sh: spira-lint/journal-readers-allow:1 allows spira/fixed.sh, which no longer reads the journal — remove the line (the list only shrinks)"]
        );
        t.write(ALLOW_FILE, "spira/fixed.sh\n");
        assert!(run(&t, &["spira/fixed.sh"]).is_err());
    }

    #[test]
    fn no_candidate_files_is_a_refusal() {
        let t = TempDir::new("jr-empty");
        t.write("a.md", "x\n");
        assert_eq!(run(&t, &["a.md"]), Err(LintError::EmptyScope));
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(JournalReads)]
}
