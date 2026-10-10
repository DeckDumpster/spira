//! `gate-state-readers` — operator-facing tooling reads a gate through `gate status <bead>` and
//! stops one with `gate cancel <bead>`: a `gate.log` or `$SPIRA_RUN` admission, holder or
//! record file is a second record of the run, and an argv match on `gate.sh` is a process scan.
//!
//! Files that still read a gate that way are listed in `spira-lint/gate-state-readers-allow`,
//! which only shrinks.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct GateStateReaders {
    checked: Cell<Option<usize>>,
}

impl Default for GateStateReaders {
    fn default() -> Self {
        GateStateReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "gate-state-readers";
const ALLOW_FILE: &str = "spira-lint/gate-state-readers-allow";

const SCOPE: [&str; 3] = ["cockpit/*", "cockpit-collect/*", "spira-world/*"];

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"gate\.log|gate-admission|gate-machine|\.lock\.holder|SPIRA_GATE_LOG|\bgate\.sh\b"#).expect("static regex"))
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

impl Rule for GateStateReaders {
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
                    message: format!("lists {p}, which no longer reads a gate's state from a file or a process — remove the line (the list only shrinks)"),
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
        "a gate's state is its machine: ask `gate status <bead>`, stop it with `gate cancel <bead>`. \
Never read gate.log or the admission and holder files, or match gate.sh in a process listing."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(GateStateReaders::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        GateStateReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_gate_file_read_or_process_match_is_caught() {
        let t = TempDir::new("gsr");
        t.write("cockpit/a.rs", "let l = fs::read_to_string(run.join(\"gate.log\"));\n");
        t.write("cockpit-collect/x.sh", "ls \"$SPIRA_RUN/gate-admission\"\n");
        t.write("spira-world/src/b.rs", "let p = home.join(\"gate.sh\");\n");
        t.write("cockpit/c.rs", "let h = tree.join(\"x.lock.holder\");\n");
        t.write("cockpit/d.rs", "let h = run.join(\"gate-machine\");\n");
        let got = run(&t, &["cockpit/a.rs", "cockpit-collect/x.sh", "spira-world/src/b.rs", "cockpit/c.rs", "cockpit/d.rs"]).unwrap();
        assert_eq!(got.len(), 5, "{got:?}");
    }

    #[test]
    fn comments_other_trees_and_listed_files_are_silent() {
        let t = TempDir::new("gsr-ok");
        t.write("cockpit/a.rs", "// gate.log is retired\nlet s = Command::new(\"gate\");\n");
        t.write("watchtower/y.rs", "let p = \"gate.log\";\n");
        t.write("spira-world/old.rs", "gate.sh\n");
        t.write(ALLOW_FILE, "spira-world/old.rs\n");
        assert_eq!(run(&t, &["cockpit/a.rs", "watchtower/y.rs", "spira-world/old.rs", ALLOW_FILE]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_file_that_no_longer_reads_is_a_finding() {
        let t = TempDir::new("gsr-stale");
        t.write("cockpit/a.rs", "let s = 1;\n");
        t.write(ALLOW_FILE, "cockpit/a.rs\n");
        assert_eq!(run(&t, &["cockpit/a.rs", ALLOW_FILE]).unwrap().len(), 1);
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("gsr-none");
        t.write("spira/y.sh", "gate.log\n");
        assert!(run(&t, &["spira/y.sh"]).is_err());
    }
}
