//! `round-record-readers` — nothing reads a round file (`round` or `round.<batch>` in a queue
//! directory) but the queue's own pre-store reader and install's remover. The lifecycle batch is
//! the one record of a round's state; `queue round status` is how operator-facing tooling asks
//! for it.
//!
//! `round-records-live-only` — inside `queue/`, only `live_records` calls `open_records`, so no
//! reader can take the record of an ended round for the open one.

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

/// The round files' only reader until the batch machine is on the control store, and the install
/// step that removes them after; the reader's tests plant the files it reads.
const OWNERS: [&str; 2] = ["queue/src/ops/round_files.rs", "install/src/round_records.rs"];
const OWNER_TESTS: [&str; 2] = ["queue/src/tests.rs", "queue/src/tests/"];

fn owns(path: &str) -> bool {
    OWNERS.contains(&path) || OWNER_TESTS.iter().any(|t| path.starts_with(t))
}

fn re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"queue/[^/\s"']+/round(\.[^/\s"']*)?(["'\s)]|$)|queue_file\(\s*(&format!\(\s*)?"round(\.[^"]*)?"|"round\.\{"#)
            .expect("static regex")
    })
}

/// Inside the queue crate a constant holding the file's name is how a reader names it.
fn queue_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"=\s*"round\.?"\s*;"#).expect("static regex"))
}

fn scan(text: &str, in_queue: bool) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            !(t.starts_with('#') || t.starts_with("//"))
        })
        .filter(|(_, l)| re().is_match(l) || (in_queue && queue_re().is_match(l)))
        .map(|(i, l)| (i + 1, l.trim_start().to_string()))
        .collect()
}

impl Rule for RoundRecordReaders {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") || e.path.ends_with(".sh"))
            && !owns(&e.path)
            && !e.path.starts_with("spira-lint/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(c) = tree.content(e) else { continue };
            for (line, text) in scan(&String::from_utf8_lossy(c), e.path.starts_with("queue/")) {
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

#[derive(Default)]
pub struct LiveOnly {
    checked: Cell<Option<usize>>,
}

const LIVE_ONLY: &str = "round-records-live-only";
const READER: &str = "open_records";
const FILTER: &str = "live_records";

fn fn_name(line: &str) -> Option<&str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:const|async|unsafe)\s+)*fn\s+(\w+)").expect("static regex"));
    re.captures(line).and_then(|c| c.get(1)).map(|m| m.as_str())
}

fn names_reader(code: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(&format!(r"\b{READER}\b")).expect("static regex")).is_match(code)
}

/// Each use of `open_records` (a call or a function value) outside the body of `live_records`.
fn stray_calls(text: &str) -> Vec<(usize, String)> {
    let mut within = "";
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if let Some(name) = fn_name(l) {
            within = name;
            if name == READER {
                continue;
            }
        }
        let code = l.split("//").next().unwrap_or("");
        if within == FILTER || !names_reader(code) {
            continue;
        }
        out.push((i + 1, l.trim_start().to_string()));
    }
    out
}

impl Rule for LiveOnly {
    fn name(&self) -> &'static str {
        LIVE_ONLY
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.starts_with("queue/") && e.path.ends_with(".rs")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        self.checked.set(None);
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(c) = tree.content(e) else { continue };
            for (line, text) in stray_calls(&String::from_utf8_lossy(c)) {
                out.push(Finding { rule: LIVE_ONLY, path: e.path.clone(), line: Some(line), message: text });
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
        "open_records returns the records of ended rounds too; a reader that takes one for the open \
round acts on a round the lifecycle has closed. Read through live_records."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(RoundRecordReaders::default()),
        Box::new(LiveOnly::default()),
    ]
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
    fn a_planted_read_inside_the_queue_crate_is_caught_in_each_spelling() {
        let t = TempDir::new("rrr-queue");
        t.write(
            "queue/src/ops/round.rs",
            "const RECORD: &str = \"round\";\nconst EXTRA: &str = \"round.\";\nlet p = c.queue_file(&format!(\"round.{batch}\"));\nlet q = c.queue_file(\"round.b1\");\nlet r = dir.join(\"queue/spira/round.b1\");\n",
        );
        let found = run(&t, &["queue/src/ops/round.rs"]).unwrap();
        assert_eq!(found.len(), 5, "{found:?}");
    }

    #[test]
    fn the_verb_comments_the_owners_and_other_names_are_silent() {
        let t = TempDir::new("rrr-ok");
        t.write("spira/a.sh", "out=\"$(queue round status)\"\n# queue/spira/round is gone\nls queue/spira/round-seq\n");
        t.write("queue/src/ops/round_files.rs", "const RECORD: &str = \"round\";\nlet f = c.queue_file(RECORD);\n");
        t.write("install/src/round_records.rs", "name == \"round\" || name.strip_prefix(\"round.\")\n");
        t.write("queue/src/r.rs", "let s = c.queue_file(\"round-staged\");\nlet n = c.queue_file(\"round-seq\");\nlet v = \"round\".to_string();\n");
        t.write("queue/src/tests/round.rs", "fs::write(t.qfile(\"round\"), rec);\n");
        t.write("testenv/src/timing.rs", "pub const ROUND: &str = \"round\";\n");
        let files = ["spira/a.sh", "queue/src/ops/round_files.rs", "install/src/round_records.rs", "queue/src/r.rs", "queue/src/tests/round.rs", "testenv/src/timing.rs"];
        assert_eq!(run(&t, &files).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("rrr-none");
        t.write("queue/src/r.rs", "x\n");
        assert!(run(&t, &["queue/src/r.rs"]).is_err());
    }

    fn run_live_only(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        LiveOnly::default().check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_call_of_open_records_outside_live_records_is_caught() {
        let t = TempDir::new("rlo");
        t.write("queue/src/ops/stage.rs", "fn stage() {\n    let r = open_records(&c).into_iter().next();\n}\n");
        t.write(
            "queue/src/ops/round.rs",
            "fn open_records(c: &Ctx) -> Vec<R> {\n    v\n}\nfn live_records(w: &World) -> Vec<R> {\n    open_records(c)\n}\npub(super) fn record_of(c: &Ctx) {\n    open_records(c).into_iter().find(f)\n}\n",
        );
        t.write("queue/src/ops/publish.rs", "async fn p() {}\nfn live_records() {}\nconst fn q() {\n    let f = rs.map(open_records);\n}\n");
        let found = run_live_only(&t, &["queue/src/ops/stage.rs", "queue/src/ops/round.rs", "queue/src/ops/publish.rs"]).unwrap();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().any(|f| f.contains("queue/src/ops/publish.rs:4")), "{found:?}");
        assert!(found.iter().any(|f| f.contains("queue/src/ops/round.rs:8")), "{found:?}");
        assert!(found.iter().any(|f| f.contains("queue/src/ops/stage.rs:2")), "{found:?}");
    }

    #[test]
    fn the_definition_the_filter_and_comments_are_silent() {
        let t = TempDir::new("rlo-ok");
        t.write(
            "queue/src/ops/round.rs",
            "/// never [`open_records`] directly\nfn open_records(c: &Ctx) -> Vec<R> {\n    v\n}\npub(crate) fn live_records(w: &World) -> Vec<R> {\n    // open_records(c) is filtered here\n    open_records(c)\n        .into_iter()\n}\n",
        );
        t.write("queue/src/ops/simple.rs", "fn x() {\n    reopen_records(c); // open_records(c) is not called\n}\n");
        t.write("cockpit/a.rs", "fn x() { open_records(c); }\n");
        assert_eq!(run_live_only(&t, &["queue/src/ops/round.rs", "queue/src/ops/simple.rs", "cockpit/a.rs"]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn no_queue_source_in_scope_refuses() {
        let t = TempDir::new("rlo-none");
        t.write("cockpit/a.rs", "x\n");
        assert!(run_live_only(&t, &["cockpit/a.rs"]).is_err());
    }
}
