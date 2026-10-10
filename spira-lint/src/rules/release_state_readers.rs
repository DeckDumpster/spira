//! `release-state-readers` — operator-facing tooling reads releases through `release status
//! --json`: the history, hotfix and machine record files under `$SPIRA_RUN/release` and the
//! `current` link in the releases directory are second records of what is in force.
//!
//! Files that still read a release that way are listed in `spira-lint/release-state-readers-allow`,
//! which only shrinks.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct ReleaseStateReaders {
    checked: Cell<Option<usize>>,
}

impl Default for ReleaseStateReaders {
    fn default() -> Self {
        ReleaseStateReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "release-state-readers";
const ALLOW_FILE: &str = "spira-lint/release-state-readers-allow";

const SCOPE: [&str; 4] = ["cockpit/*", "cockpit-collect/*", "spira-world/*", "watchtower/*"];

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"release/(history|hotfix|machine)|\.join\("(history|hotfix)"\)|releases?/current\b|\.join\("current"\)|SPIRA_RELEASES\b"#).expect("static regex"))
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

impl Rule for ReleaseStateReaders {
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
                    message: format!("lists {p}, which no longer reads release state from a file — remove the line (the list only shrinks)"),
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
        "what is in force is the release machine: ask `release status --json`. \
Never read the history, hotfix or machine files under the run directory, or the releases `current` link."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(ReleaseStateReaders::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ReleaseStateReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_release_file_read_is_caught() {
        let t = TempDir::new("rsr");
        t.write("cockpit/a.rs", "let l = fs::read_to_string(run.join(\"release/history\"));\n");
        t.write("cockpit-collect/x.sh", "cat \"$SPIRA_RUN/release/hotfix\"\n");
        t.write("spira-world/src/b.rs", "let p = fs::read_link(releases.join(\"current\"));\n");
        t.write("cockpit/c.rs", "let r = env::var(\"SPIRA_RELEASES\");\n");
        t.write("watchtower/d.rs", "let h = state.join(\"release/machine\");\n");
        let got = run(&t, &["cockpit/a.rs", "cockpit-collect/x.sh", "spira-world/src/b.rs", "cockpit/c.rs", "watchtower/d.rs"]).unwrap();
        assert_eq!(got.len(), 5, "{got:?}");
    }

    #[test]
    fn comments_other_trees_the_verb_and_listed_files_are_silent() {
        let t = TempDir::new("rsr-ok");
        t.write("cockpit/a.rs", "// release/history is retired\nlet s = Command::new(\"release\").args([\"status\", \"--json\"]);\n");
        t.write("queue/y.rs", "let p = \"release/history\";\n");
        t.write("spira-world/old.rs", "releases/current\n");
        t.write(ALLOW_FILE, "spira-world/old.rs\n");
        assert_eq!(run(&t, &["cockpit/a.rs", "queue/y.rs", "spira-world/old.rs", ALLOW_FILE]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_file_that_no_longer_reads_is_a_finding() {
        let t = TempDir::new("rsr-stale");
        t.write("cockpit/a.rs", "let s = 1;\n");
        t.write(ALLOW_FILE, "cockpit/a.rs\n");
        assert_eq!(run(&t, &["cockpit/a.rs", ALLOW_FILE]).unwrap().len(), 1);
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("rsr-none");
        t.write("spira/y.sh", "release/history\n");
        assert!(run(&t, &["spira/y.sh"]).is_err());
    }
}
