//! `liveness-readers` — operator-facing tooling asks whether an aeon or a watcher is alive
//! through the verbs (`aeon stop`, `watchd status`, the lifecycle row's lease), never by
//! scanning processes: a `pgrep` pattern, a `/proc/<pid>/cmdline` read or an `aeon-*.pid`
//! pidfile is a second record of liveness that disagrees with the lease.
//!
//! Files that still read liveness that way are listed in `spira-lint/liveness-readers-allow`,
//! which only shrinks.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct LivenessReaders {
    checked: Cell<Option<usize>>,
}

impl Default for LivenessReaders {
    fn default() -> Self {
        LivenessReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "liveness-readers";
const ALLOW_FILE: &str = "spira-lint/liveness-readers-allow";

const SCOPE: [&str; 3] = ["cockpit/*", "cockpit-collect/*", "spira-world/*"];

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"pgrep|\bcmdline\b|aeon-[^"\s]*\.pid"#).expect("static regex"))
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

impl Rule for LivenessReaders {
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
                    message: format!("lists {p}, which no longer reads liveness from a process — remove the line (the list only shrinks)"),
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
        "liveness is the lease: ask `watchd status` or the lifecycle row, and stop an aeon with `aeon stop <bead>`. \
Never scan processes or read an aeon pidfile."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        LivenessReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_process_scan_or_pidfile_read_is_caught() {
        let t = TempDir::new("lr");
        t.write("cockpit/a.rs", "let c = fs::read(format!(\"/proc/{pid}/cmdline\"));\n");
        t.write("cockpit-collect/x.sh", "pgrep -f 'aeon.sh builder'\n");
        t.write("spira-world/src/b.rs", "let p = run.join(\"aeon-builder-sp-1.pid\");\n");
        let got = run(&t, &["cockpit/a.rs", "cockpit-collect/x.sh", "spira-world/src/b.rs"]).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn comments_other_trees_and_listed_files_are_silent() {
        let t = TempDir::new("lr-ok");
        t.write("cockpit/a.rs", "// pgrep is retired\nlet s = Command::new(\"watchd\");\n");
        t.write("aeon/y.rs", "let p = \"aeon-x.pid\";\n");
        t.write("spira-world/old.rs", "pgrep x\n");
        t.write(ALLOW_FILE, "spira-world/old.rs\n");
        assert_eq!(run(&t, &["cockpit/a.rs", "aeon/y.rs", "spira-world/old.rs", ALLOW_FILE]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_listed_file_that_no_longer_reads_is_a_finding() {
        let t = TempDir::new("lr-stale");
        t.write("cockpit/a.rs", "let s = 1;\n");
        t.write(ALLOW_FILE, "cockpit/a.rs\n");
        assert_eq!(run(&t, &["cockpit/a.rs", ALLOW_FILE]).unwrap().len(), 1);
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("lr-none");
        t.write("spira/y.sh", "pgrep x\n");
        assert!(run(&t, &["spira/y.sh"]).is_err());
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(LivenessReaders::default()),
    ]
}
