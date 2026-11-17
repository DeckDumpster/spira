//! `core-full-list` — nothing asks the control store for the unfiltered `list`.
//! Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct CoreFullList;

const NAME: &str = "core-full-list";

fn unfiltered() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"lc_state::list\s*\(|(?:^|[^!])\[\s*"list"(\.into\(\)|\.to_string\(\))?\s*\]|spira-lc list\s*(["')|;>2]|$)"#,
        )
        .expect("static regex")
    })
}

fn is_test_file(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.ends_with("tests.rs") || base.starts_with("test-") || path.contains("/tests/")
}

fn exempt(line: &str) -> bool {
    line.contains("batch-job:") || line.contains("not-a-lifecycle-list:")
}

fn scan(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("#[cfg(test)]") {
            break;
        }
        if t.starts_with("//") || t.starts_with('#') || !unfiltered().is_match(l) {
            continue;
        }
        if exempt(l) || (i > 0 && exempt(lines[i - 1])) {
            continue;
        }
        out.push((i + 1, t.to_string()));
    }
    out
}

impl Rule for CoreFullList {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        (e.path.ends_with(".rs") || e.path.ends_with(".sh")) && !is_test_file(&e.path)
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in &files {
            let Some(c) = tree.content(e) else { continue };
            for (line, text) in scan(&String::from_utf8_lossy(c)) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "the unfiltered `spira-lc list` is refused by the store: it would read every row. Ask for what \
the code needs: `list --live` (the rows a tick acts on), `--state S[,S]`, `--ids a,b` or a page (`--after ID --limit N`). A true \
batch job says so with a `batch-job: <why>` comment on the call or the line above; a verb that is not the lifecycle list, with `not-a-lifecycle-list:`."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(CoreFullList)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        CoreFullList.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_unfiltered_list_is_caught_in_every_form_a_core_process_can_ask_for_one() {
        let t = TempDir::new("cfl-planted");
        t.write("sentinel/src/a.rs", "let o = h.lc(bin, &[\"list\".into()]);\n");
        t.write("strand/src/b.rs", "let rows = spira_store::lc_state::list();\n");
        t.write("watchtower/src/d.rs", "let r = c.call(&[\"list\"]);\n");
        t.write("dwarf/x.sh", "spira-lc list | jq .\n");
        t.write("spira-lc/src/e.rs", "let b = c.call(&[\"list\".into()]);\n");
        let got = run(&t, &["sentinel/src/a.rs", "strand/src/b.rs", "watchtower/src/d.rs", "dwarf/x.sh", "spira-lc/src/e.rs"]).unwrap();
        assert_eq!(got.len(), 5, "{got:?}");
    }

    #[test]
    fn a_filtered_list_a_batch_job_a_comment_and_a_test_are_silent() {
        let t = TempDir::new("cfl-quiet");
        t.write(
            "sentinel/src/a.rs",
            "let a = h.lc(bin, &[\"list\".into(), \"--live\".into()]);\nlet b = lc_state::list_live_on(&c);\nlet c = lc_state::list_ids_on(&c, &ids);\n// lc_state::list() is the old way\n// batch-job: the nightly export\nlet d = lc_state::list();\nlet e = lc_state::list(); // batch-job: audit\n",
        );
        t.write("dwarf/x.sh", "spira-lc list --state WORKING\n# spira-lc list\n");
        t.write("heimdall/src/c.rs", "let r = c.call(&[\"list\"]); // not-a-lifecycle-list: conditions\n");
        t.write("strand/src/tests.rs", "let rows = lc_state::list();\n");
        t.write("strand/src/model_tests.rs", "let v = &[\"list\"];\n");
        t.write("strand/src/b.rs", "fn f() {}\n#[cfg(test)]\nmod t { fn x() { lc_state::list(); } }\n");
        let got = run(&t, &["sentinel/src/a.rs", "dwarf/x.sh", "heimdall/src/c.rs", "strand/src/model_tests.rs", "strand/src/b.rs"]).unwrap();
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn a_crate_outside_the_old_core_scope_is_caught_too() {
        let t = TempDir::new("cfl-wide");
        t.write("spira-lc/src/y.rs", "let o = c.call(&[\"list\".into()]);\n");
        t.write("census/src/z.rs", "let rows = spira_store::lc_state::list();\n");
        assert_eq!(run(&t, &["spira-lc/src/y.rs", "census/src/z.rs"]).unwrap().len(), 2);
    }

    #[test]
    fn a_tree_with_no_source_refuses() {
        let t = TempDir::new("cfl-none");
        t.write("notes/y.txt", "list\n");
        assert!(run(&t, &["notes/y.txt"]).is_err());
    }
}
