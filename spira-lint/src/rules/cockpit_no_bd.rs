//! `cockpit-no-bd` — nothing under `cockpit/` or `cockpit-collect/` invokes `bd`. Bead state
//! comes from spira-lc and bead content and label/comment writes go through `spira-lc content`.

use crate::SyncCell as Cell;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct CockpitNoBd {
    checked: Cell<Option<usize>>,
}

impl Default for CockpitNoBd {
    fn default() -> Self {
        CockpitNoBd { checked: Cell::new(None) }
    }
}

const NAME: &str = "cockpit-no-bd";

fn rust_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""bd"|\bbd_bin\b|\bbdq\s*\(|\bbdjson\s*\("#).expect("static regex"))
}

fn shell_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(^|[\s;&|(`])bd\s+-?[A-Za-z]|\$\{?BD(_BIN)?\}?\b"#).expect("static regex"))
}

fn scan(path: &str, text: &str) -> Vec<(usize, String)> {
    let re = if path.ends_with(".rs") { rust_re() } else { shell_re() };
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !(t.starts_with('#') || t.starts_with("//"))
        })
        .filter(|(_, l)| re.is_match(l))
        .map(|(i, l)| (i + 1, l.trim_start().to_string()))
        .collect()
}

impl Rule for CockpitNoBd {
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
            for (line, text) in scan(&e.path, &String::from_utf8_lossy(c)) {
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
        "bd's status lags the lifecycle machine, so a pane that reads or writes through it shows \
landed work as ready. Read state with spira-lc (state, list, holds) and everything else through \
`spira-lc content …`, `work`, `reply`, `resolve` or `mail`; add the verb there if it is missing."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        CockpitNoBd::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn every_way_of_invoking_bd_is_caught() {
        let t = TempDir::new("cnb");
        t.write("cockpit/panel/src/a.rs", "let c = Command::new(\"bd\");\nlet b = db::bd_bin();\n");
        t.write("cockpit-collect/src/b.rs", "let r = bdjson(&[\"list\"]);\nlet s = bdq(&[\"show\"]);\n");
        t.write("cockpit/x.sh", "out=$(bd -C \"$db\" list --json)\n\"$BD\" close x\n");
        let got = run(&t, &["cockpit/panel/src/a.rs", "cockpit-collect/src/b.rs", "cockpit/x.sh"]).unwrap();
        assert_eq!(got.len(), 6, "{got:?}");
    }

    #[test]
    fn spira_lc_content_comments_and_other_trees_are_silent() {
        let t = TempDir::new("cnb-ok");
        t.write("cockpit/a.rs", "// bd is gone\nlet c = Command::new(\"spira-lc\"); // content\nlet s = \"bd: refused\";\n");
        t.write("cockpit/x.sh", "# bd list\n\"$LC\" content list --json\n");
        t.write("spira/y.sh", "bd list\n");
        assert_eq!(run(&t, &["cockpit/a.rs", "cockpit/x.sh", "spira/y.sh"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("cnb-none");
        t.write("spira/y.sh", "bd list\n");
        assert!(run(&t, &["spira/y.sh"]).is_err());
    }
}
