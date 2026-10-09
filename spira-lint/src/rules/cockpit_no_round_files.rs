//! `cockpit-no-round-files` — nothing under `cockpit/` or `cockpit-collect/` reads a round pass
//! from a file. The pass is a field of the batch row, read through spira-lc.

use crate::SyncCell as Cell;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct CockpitNoRoundFiles {
    checked: Cell<Option<usize>>,
}

impl Default for CockpitNoRoundFiles {
    fn default() -> Self {
        CockpitNoRoundFiles { checked: Cell::new(None) }
    }
}

const NAME: &str = "cockpit-no-round-files";

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"round-progress|round_progress|batch-results|rounds/[^\s"']*\.(running|result)"#).expect("static regex"))
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

impl Rule for CockpitNoRoundFiles {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (pathspec_match("cockpit/*", &e.path) || pathspec_match("cockpit-collect/*", &e.path))
            && (e.path.ends_with(".rs") || e.path.ends_with(".sh"))
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
        "a progress file is a second record of the pass and disagrees with the batch row. Read the \
pass (number, phase, reds) from the batch row through spira-lc; add the field there if it is missing."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        CockpitNoRoundFiles::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_read_of_a_round_file_is_caught() {
        let t = TempDir::new("cnrf");
        t.write("cockpit/panel/src/a.rs", "let p = run.join(\"round-progress.json\");\n");
        t.write("cockpit-collect/src/b.rs", "let r = dir.join(\"rounds/b1.running\");\n");
        t.write("cockpit/x.sh", "cat \"$SPIRA_RUN/round-progress.json\"\n");
        let got = run(&t, &["cockpit/panel/src/a.rs", "cockpit-collect/src/b.rs", "cockpit/x.sh"]).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn comments_and_other_trees_are_silent() {
        let t = TempDir::new("cnrf-ok");
        t.write("cockpit/a.rs", "// round-progress.json is retired\nlet c = Command::new(\"spira-lc\");\n");
        t.write("round-vm/y.rs", "let p = \"round-progress.json\";\n");
        assert_eq!(run(&t, &["cockpit/a.rs", "round-vm/y.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("cnrf-none");
        t.write("spira/y.sh", "round-progress\n");
        assert!(run(&t, &["spira/y.sh"]).is_err());
    }
}
