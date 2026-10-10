//! `testenv-result-readers` — operator-facing tooling reads a testenv run through `testenv
//! status --json`: the per-suite `.result` files and the run's `batch.meta` are the runner's
//! output, not a second source of what the run did.
//!
//! Files that still read a run that way are listed in `spira-lint/testenv-result-readers-allow`,
//! which only shrinks.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct TestenvResultReaders {
    checked: Cell<Option<usize>>,
}

impl Default for TestenvResultReaders {
    fn default() -> Self {
        TestenvResultReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "testenv-result-readers";
const ALLOW_FILE: &str = "spira-lint/testenv-result-readers-allow";

const SCOPE: [&str; 4] = ["cockpit/*", "cockpit-collect/*", "spira-world/*", "watchtower/*"];

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"[*]\.result|\.result["'}]|batch\.meta"#).expect("static regex"))
}

fn scan(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !(t.starts_with('#') || t.starts_with("//"))
        })
        .filter(|(_, l)| pattern().is_match(l))
        .map(|(i, l)| (i + 1, l.trim_start().to_string()))
        .collect()
}

impl Rule for TestenvResultReaders {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        SCOPE.iter().any(|s| pathspec_match(s, &e.path)) && (e.path.ends_with(".rs") || e.path.ends_with(".sh"))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let allow: Vec<(usize, String)> = tree
            .read_text(ALLOW_FILE)
            .lines()
            .enumerate()
            .filter(|(_, l)| {
                let t = l.trim_start();
                !(t.is_empty() || t.starts_with('#'))
            })
            .map(|(i, l)| (i + 1, l.trim().to_string()))
            .collect();
        let mut out = Vec::new();
        let mut offenders = BTreeSet::new();
        for e in &files {
            let Some(c) = tree.content(e) else { continue };
            for (line, text) in scan(&String::from_utf8_lossy(c)) {
                offenders.insert(e.path.clone());
                if allow.iter().any(|(_, p)| p == &e.path) {
                    continue;
                }
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
            }
        }
        for (line, p) in &allow {
            if !offenders.contains(p) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("lists {p}, which no longer reads a testenv run from a file — remove the line (the list only shrinks)"),
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
        "what a testenv run did is its recorded state: ask `testenv status --json <results-dir>`. \
Never read the per-suite result files or the batch.meta under a results directory."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(TestenvResultReaders::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        TestenvResultReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_result_read_is_caught() {
        let t = TempDir::new("trr");
        t.write("cockpit/a.rs", "let l = fs::read_to_string(dir.join(format!(\"{s}.result\")));\n");
        t.write("cockpit-collect/x.sh", "for f in \"$R\"/*.result; do :; done\n");
        t.write("spira-world/src/b.rs", "let m = fs::read_to_string(r.join(\"batch.meta\"));\n");
        t.write("watchtower/d.sh", "cat \"$R/test-a.sh.result\"\n");
        let got = run(&t, &["cockpit/a.rs", "cockpit-collect/x.sh", "spira-world/src/b.rs", "watchtower/d.sh"]).unwrap();
        assert_eq!(got.len(), 4, "{got:?}");
    }

    #[test]
    fn comments_field_reads_other_trees_and_listed_files_are_silent() {
        let t = TempDir::new("trr-ok");
        t.write("watchtower/p.rs", "// the .result\" file is retired\nif run.result == \"signal\" {}\nlet s = Command::new(\"testenv\").args([\"status\", \"--json\"]);\n");
        t.write("batcher-cut/y.rs", "let p = \"a.result\";\n");
        t.write("spira-world/old.rs", "*.result\n");
        t.write(ALLOW_FILE, "spira-world/old.rs\n");
        assert_eq!(run(&t, &["watchtower/p.rs", "batcher-cut/y.rs", "spira-world/old.rs", ALLOW_FILE]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_file_that_no_longer_reads_is_a_finding() {
        let t = TempDir::new("trr-stale");
        t.write("cockpit/a.rs", "let s = 1;\n");
        t.write(ALLOW_FILE, "cockpit/a.rs\n");
        assert_eq!(run(&t, &["cockpit/a.rs", ALLOW_FILE]).unwrap().len(), 1);
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("trr-none");
        t.write("spira/y.sh", "*.result\n");
        assert!(run(&t, &["spira/y.sh"]).is_err());
    }
}
