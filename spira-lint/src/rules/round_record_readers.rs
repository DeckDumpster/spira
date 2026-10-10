//! `round-record-readers` — nothing outside `queue/` reads the round record file. The lifecycle
//! batch is the one record of a round's state; `queue round status` is how operator-facing
//! tooling asks for it.

use crate::SyncCell as Cell;
use std::sync::OnceLock;

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct RoundRecordReaders {
    checked: Cell<Option<usize>>,
}

impl Default for RoundRecordReaders {
    fn default() -> Self {
        RoundRecordReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "round-record-readers";

fn re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"queue/[^/\s"']+/round(["'\s)]|$)|queue_file\(\s*"round"\s*\)"#).expect("static regex"))
}

fn scan(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !(t.starts_with('#') || t.starts_with("//"))
        })
        .filter(|(_, l)| re().is_match(l))
        .map(|(i, l)| (i + 1, l.trim_start().to_string()))
        .collect()
}

impl Rule for RoundRecordReaders {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") || e.path.ends_with(".sh")) && !e.path.starts_with("queue/") && !e.path.starts_with("spira-lint/")
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
        "A round's state is the lifecycle batch's; a second copy read from the queue directory can \
disagree with it. Ask `queue round status`, or add the field to that verb."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        RoundRecordReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_read_of_the_round_record_is_caught() {
        let t = TempDir::new("rrr");
        t.write("cockpit/a.rs", "let p = run.join(\"queue/spira/round\");\n");
        t.write("spira/x.sh", "phase=$(grep phase \"$SPIRA_RUN/queue/spira/round\")\n");
        t.write("spira/y.rs", "let kv = read_kv(&c.queue_file(\"round\"));\n");
        assert_eq!(run(&t, &["cockpit/a.rs", "spira/x.sh", "spira/y.rs"]).unwrap().len(), 3);
    }

    #[test]
    fn the_verb_comments_the_queue_crate_and_longer_names_are_silent() {
        let t = TempDir::new("rrr-ok");
        t.write("spira/a.sh", "out=\"$(queue round status)\"\n# queue/spira/round is gone\nls queue/spira/round.lock\n");
        t.write("queue/src/r.rs", "let f = c.queue_file(\"round\");\n");
        assert_eq!(run(&t, &["spira/a.sh", "queue/src/r.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("rrr-none");
        t.write("queue/src/r.rs", "x\n");
        assert!(run(&t, &["queue/src/r.rs"]).is_err());
    }
}
