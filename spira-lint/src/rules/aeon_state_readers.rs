//! `aeon-state-readers` — nothing that reports on an aeon reads its run from a file or a
//! process. The phase and the disposition are fields of the bead's lifecycle row, read through
//! spira-lc; the marker files that carried the outcome are retired, and the operator-facing
//! health and panel readers read no pidfile, lease file, ledger or `/proc` entry.

use crate::SyncCell as Cell;
use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct AeonStateReaders {
    checked: Cell<Option<usize>>,
}

impl Default for AeonStateReaders {
    fn default() -> Self {
        AeonStateReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "aeon-state-readers";

const MARKER_SCOPE: &[&str] = &["cockpit/*", "cockpit-collect/*", "watchtower/*", "aeon/*", "spira-world/*", "spira/*.sh"];
const READER_SCOPE: &[&str] = &["cockpit/ops/src/health*", "cockpit/panel/*"];

fn marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\.(lapsed|thrash|slain)["']"#).expect("static regex"))
}

fn process_or_file() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"/proc\b|procfs::|\.pid["']|aeon-ledger|\.lease["']"#).expect("static regex"))
}

fn is_test_file(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    base == "tests.rs" || base.starts_with("test-") || path.contains("/tests/")
}

fn scan(path: &str, text: &str, readers: bool) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let t = l.trim_start();
        if t.starts_with("#[cfg(test)]") {
            break;
        }
        if t.starts_with('#') || t.starts_with("//") {
            continue;
        }
        if marker().is_match(l) || (readers && process_or_file().is_match(l)) {
            out.push((i + 1, format!("{path}: {t}")));
        }
    }
    out
}

fn in_scope(globs: &[&str], path: &str) -> bool {
    globs.iter().any(|g| pathspec_match(g, path))
}

impl Rule for AeonStateReaders {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (in_scope(MARKER_SCOPE, &e.path) || in_scope(READER_SCOPE, &e.path))
            && (e.path.ends_with(".rs") || e.path.ends_with(".sh"))
            && !is_test_file(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(c) = tree.content(e) else { continue };
            let readers = in_scope(READER_SCOPE, &e.path);
            for (line, text) in scan(&e.path, &String::from_utf8_lossy(c), readers) {
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
        "an aeon's phase and how its session was cut short are fields of its WORKING row. Record them \
with `spira-lc phase` / `spira-lc disposition` and read them from `spira-lc show` / `list`; a marker, \
pid, lease or log file is a second record that disagrees with the row. Add the field to the machine if it is missing."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        AeonStateReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_read_of_a_marker_file_is_caught_in_every_tree_that_could_read_one() {
        let t = TempDir::new("asr-marker");
        t.write("aeon/src/a.rs", "let p = run.join(format!(\"{id}.slain\"));\n");
        t.write("cockpit-collect/src/b.rs", "let r = dir.join(\"sp-a.thrash\");\n");
        t.write("spira/x.sh", "cat \"$SPIRA_RUN/$1.lapsed\"\n");
        t.write("spira-world/src/c.rs", "std::fs::write(run.join(\"x.slain\"), \"\")\n");
        let got = run(&t, &["aeon/src/a.rs", "cockpit-collect/src/b.rs", "spira/x.sh", "spira-world/src/c.rs"]).unwrap();
        assert_eq!(got.len(), 4, "{got:?}");
    }

    #[test]
    fn a_planted_pid_lease_ledger_or_proc_read_is_caught_in_the_health_and_panel_readers() {
        let t = TempDir::new("asr-reader");
        t.write("cockpit/ops/src/health_main.rs", "let n = std::fs::read(format!(\"/proc/{pid}/cmdline\"));\n");
        t.write("cockpit/ops/src/health/sections.rs", "if name.ends_with(\".pid\") {}\n");
        t.write("cockpit/panel/src/store.rs", "let l = run.join(\"aeon-ledger.log\");\n");
        t.write("cockpit/panel/src/model.rs", "use cockpit_ops::procfs::cmdline;\n");
        let got = run(&t, &["cockpit/ops/src/health_main.rs", "cockpit/ops/src/health/sections.rs", "cockpit/panel/src/store.rs", "cockpit/panel/src/model.rs"]).unwrap();
        assert_eq!(got.len(), 4, "{got:?}");
    }

    #[test]
    fn what_is_not_a_read_is_silent() {
        let t = TempDir::new("asr-ok");
        t.write("aeon/src/decide.rs", "i.slain = true;\nif i.lapsed { x }\nlet thrash = i.thrash;\n");
        t.write("aeon/src/b.rs", "// the .slain marker is retired\nfn f() {}\n#[cfg(test)]\nmod t { fn x() { run.join(\"sp.slain\"); } }\n");
        t.write("aeon/src/tests.rs", "std::fs::write(run.join(\"sp-nm.slain\"), \"\");\n");
        t.write("spira/test-x.sh", "printf x > \"$SPIRA_RUN/$1.slain\"\n");
        t.write("cockpit/ops/src/layout.rs", "let c = fs::read(format!(\"/proc/{pid}/cmdline\"));\n");
        t.write("cockpit/ops/src/health/a.rs", "let c = Command::new(\"spira-lc\");\n");
        let got = run(&t, &["aeon/src/decide.rs", "aeon/src/b.rs", "aeon/src/tests.rs", "spira/test-x.sh", "cockpit/ops/src/layout.rs", "cockpit/ops/src/health/a.rs"]).unwrap();
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("asr-none");
        t.write("round-vm/y.rs", "\"x.slain\"\n");
        assert!(run(&t, &["round-vm/y.rs"]).is_err());
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(AeonStateReaders::default()),
    ]
}
