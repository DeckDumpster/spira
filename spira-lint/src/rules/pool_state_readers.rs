//! `pool-state-readers` — operator-facing tooling reads the VM pool through `round-vm status --json`,
//! never from the pool's state files.

use crate::SyncCell as Cell;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct PoolStateReaders {
    checked: Cell<Option<usize>>,
}

impl Default for PoolStateReaders {
    fn default() -> Self {
        PoolStateReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "pool-state-readers";

const SCOPE: [&str; 3] = ["cockpit/*", "cockpit-collect/*", "spira-world/*"];

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"pool\.json|template\.json|round-vm/state|round-vm\.lock"#).expect("static regex"))
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

impl Rule for PoolStateReaders {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        SCOPE.iter().any(|s| pathspec_match(s, &e.path)) && (e.path.ends_with(".rs") || e.path.ends_with(".sh"))
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
        "a pool state file is a second record of the pool and is read with no lock. Read leases, \
doomed VMs, the template and ages from `round-vm status --json`; add the field there if it is missing."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        PoolStateReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_read_of_a_pool_file_is_caught() {
        let t = TempDir::new("psr");
        t.write("cockpit/panel/src/a.rs", "let p = state.join(\"pool.json\");\n");
        t.write("spira-world/src/b.rs", "let r = dir.join(\"template.json\");\n");
        t.write("cockpit-collect/x.sh", "cat \"$SPIRA_RUN/round-vm/state/pool.json\"\n");
        let got = run(&t, &["cockpit/panel/src/a.rs", "spira-world/src/b.rs", "cockpit-collect/x.sh"]).unwrap();
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn comments_and_other_trees_are_silent() {
        let t = TempDir::new("psr-ok");
        t.write("cockpit/a.rs", "// pool.json is retired\nlet c = Command::new(\"round-vm\");\n");
        t.write("round-vm/y.rs", "let p = \"pool.json\";\n");
        assert_eq!(run(&t, &["cockpit/a.rs", "round-vm/y.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("psr-none");
        t.write("spira/y.sh", "pool.json\n");
        assert!(run(&t, &["spira/y.sh"]).is_err());
    }
}
