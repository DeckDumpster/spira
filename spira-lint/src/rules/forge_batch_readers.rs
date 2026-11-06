//! `forge-batch-readers` — nothing outside the queue reads a forge batch's `open`, `prepared` or
//! `forge-state` file in a queue directory. Once the batch machine is on the control store the
//! batch's own record holds them, and `queue batch show` is how anything else asks for it; a
//! reader of the file goes blind on the store. Install's remover deletes the files after the move,
//! and the migration classifier imports the legacy file as install hands it over.
//!
//! Files that still read one are listed in `spira-lint/forge-batch-readers-allow`, which only
//! shrinks.

use crate::SyncCell as Cell;
use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct ForgeBatchReaders {
    checked: Cell<Option<usize>>,
}

impl Default for ForgeBatchReaders {
    fn default() -> Self {
        ForgeBatchReaders { checked: Cell::new(None) }
    }
}

const NAME: &str = "forge-batch-readers";
const ALLOW_FILE: &str = "spira-lint/forge-batch-readers-allow";

/// The queue reads and writes the files until the store holds the batch; install removes them,
/// and the migration classifier imports the open one from whichever queue directory install
/// names, which no queue verb can be pointed at. `queue.step` is the queue's own verdict step
/// run by the scheduler; it records the forge state as the queue did.
const OWNERS: [&str; 4] = ["queue/", "install/src/round_records.rs", "spira-lc/src/legacy_files.rs", "spira-runtime/src/tasks/a/queue_step.rs"];

fn owns(path: &str) -> bool {
    OWNERS.iter().any(|o| path.starts_with(o))
}

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(join|queue_file)\(\s*"(open|prepared|forge-state)"|"[^"\s]*/(open|prepared|forge-state)"|queue/[^/\s"']+/(open|prepared|forge-state)(["'\s)]|$)|"(prepared|forge-state)""#,
        )
        .expect("static regex")
    })
}

/// A state machine's `"prepared"` event, not the file: a transition's `on:`, a `go(..)` call, or
/// the event column of a `(Phase::X, "prepared", ..)` table row.
fn event_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(on:\s*|\bgo\((?:[a-z_]+,\s*)?|Phase::\w+,\s*)"prepared""#).expect("static regex"))
}

fn scan(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !(t.starts_with('#') || t.starts_with("//"))
        })
        .filter(|(_, l)| pattern().is_match(&event_label().replace_all(l, "")))
        .map(|(i, l)| (i + 1, l.trim_start().to_string()))
        .collect()
}

impl Rule for ForgeBatchReaders {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") || e.path.ends_with(".sh")) && !owns(&e.path) && !e.path.starts_with("spira-lint/")
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
                    message: format!("lists {p}, which no longer reads a forge batch's file — remove the line (the list only shrinks)"),
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
        "a forge batch's state is its record: ask `queue batch show --repo <r>`, whose `forge.*` lines \
carry the verdict's last look at the PR. Never read the open, prepared or forge-state file."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(ForgeBatchReaders::default())]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        ForgeBatchReaders::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_read_of_the_open_or_forge_state_file_is_caught_in_each_spelling() {
        let t = TempDir::new("fbr");
        t.write("loom/src/a.rs", "let kv = kv(&dir.join(\"open\"));\nlet fs = kv(&dir.join(\"forge-state\"));\n");
        t.write("cockpit-collect/src/b.rs", "let p = queue_dir.join(\"r/open\");\nlet q = c.queue_file(\"prepared\");\n");
        t.write("spira/x.sh", "pr=$(grep pr= \"$QUEUEDIR/$REPO/open\")\ncat queue/deck/forge-state\n");
        t.write("round-controller/src/c.rs", "const STATE: &str = \"forge-state\";\n");
        let got = run(&t, &["loom/src/a.rs", "cockpit-collect/src/b.rs", "spira/x.sh", "round-controller/src/c.rs"]).unwrap();
        assert_eq!(got.len(), 7, "{got:?}");
    }

    #[test]
    fn the_verb_comments_owners_other_names_and_listed_files_are_silent() {
        let t = TempDir::new("fbr-ok");
        t.write("loom/src/a.rs", "// dir.join(\"open\") is retired\nlet s = queue(&[\"batch\", \"show\", \"--repo\", r]);\nlet st = \"open\";\nlet m = \"must not read as 0/open\";\n");
        t.write("spira/y.sh", "# \"$Q/open\" is gone\nqueue batch show --repo deck\nmv \"$Q/open.t\" x\n");
        t.write("queue/src/ops/forge_batch.rs", "pub const FORGE_STATE: &str = \"forge-state\";\nlet f = c.queue_file(\"open\");\n");
        t.write("install/src/round_records.rs", "const FORGE_FILES: [&str; 3] = [\"open\", \"prepared\", \"forge-state\"];\n");
        t.write("czar-pass/src/old.rs", "let open = dir.join(\"open\");\n");
        t.write(ALLOW_FILE, "czar-pass/src/old.rs\n");
        let files = ["loom/src/a.rs", "spira/y.sh", "queue/src/ops/forge_batch.rs", "install/src/round_records.rs", "czar-pass/src/old.rs", ALLOW_FILE];
        assert_eq!(run(&t, &files).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn a_prepared_event_in_a_state_machine_is_not_the_file() {
        let t = TempDir::new("fbr-event");
        t.write("spira-runtime/src/tasks/d/x.rs", "(Phase::Running, \"prepared\", Phase::Running),\nself.go(c, \"prepared\")?;\nTransition { from: S::A, on: \"prepared\", to: S::B },\nwith(carry, go(\"prepared\")?)\nlet f = c.queue_file(\"prepared\");\n");
        let got = run(&t, &["spira-runtime/src/tasks/d/x.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains(":5:"), "{got:?}");
    }

    #[test]
    fn a_listed_file_that_no_longer_reads_is_a_finding() {
        let t = TempDir::new("fbr-stale");
        t.write("czar-pass/src/a.rs", "let s = 1;\n");
        t.write(ALLOW_FILE, "czar-pass/src/a.rs\n");
        assert_eq!(run(&t, &["czar-pass/src/a.rs", ALLOW_FILE]).unwrap().len(), 1);
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("fbr-none");
        t.write("queue/src/ops/forge_batch.rs", "x\n");
        assert!(run(&t, &["queue/src/ops/forge_batch.rs"]).is_err(), "an owner is out of scope");
    }
}
