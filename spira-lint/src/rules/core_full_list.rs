//! `core-full-list` — a core process never asks the control store for the unfiltered `list`.
//! Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::Regex;

use crate::{pathspec_match, Entry, Finding, LintError, Rule, Tree};

pub struct CoreFullList;

const NAME: &str = "core-full-list";

const SCOPE: &[&str] = &["sentinel/*", "strand/*", "landing-pass/*", "incident/*", "hel/*", "dwarf/*", "watchtower/*", "spira-runtime/src/tasks/*"];

fn unfiltered() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"lc_state::(list|list_on|list_with|list_within|list_raw|list_raw_on|list_raw_with)\s*\(|beads_on\([^)]*,\s*None\s*\)|(?:^|[^!])\[\s*"list"(\.into\(\)|\.to_string\(\))?\s*\]|spira-lc list\s*(["')|;>2]|$)"#,
        )
        .expect("static regex")
    })
}

fn is_test_file(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    base == "tests.rs" || base.starts_with("test-") || path.contains("/tests/")
}

fn exempt(line: &str) -> bool {
    line.contains("batch-job:")
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
        SCOPE.iter().any(|g| pathspec_match(g, &e.path)) && (e.path.ends_with(".rs") || e.path.ends_with(".sh")) && !is_test_file(&e.path)
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
        "the unfiltered `spira-lc list` is every bead (~6 MB) and sets the store's p95. Ask for what \
the code needs: `list --live` (the rows a tick acts on), `--state S[,S]`, `--ids a,b` or a page. A true \
batch job says so with a `batch-job: <why>` comment on the call or the line above."
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
        t.write("hel/src/c.rs", "let rows = spira_store::lc_state::list_on(&c);\n");
        t.write("watchtower/src/d.rs", "let r = c.call(&[\"list\"]);\n");
        t.write("dwarf/x.sh", "spira-lc list | jq .\n");
        t.write("spira-runtime/src/tasks/e.rs", "let b = beads_on(&c, None);\n");
        let got = run(&t, &["sentinel/src/a.rs", "strand/src/b.rs", "hel/src/c.rs", "watchtower/src/d.rs", "dwarf/x.sh", "spira-runtime/src/tasks/e.rs"]).unwrap();
        assert_eq!(got.len(), 6, "{got:?}");
    }

    #[test]
    fn a_filtered_list_a_batch_job_a_comment_a_test_and_another_crate_are_silent() {
        let t = TempDir::new("cfl-quiet");
        t.write(
            "sentinel/src/a.rs",
            "let a = h.lc(bin, &[\"list\".into(), \"--live\".into()]);\nlet b = lc_state::list_live_on(&c);\nlet c = lc_state::list_ids_on(&c, &ids);\n// lc_state::list() is the old way\n// batch-job: the nightly export\nlet d = lc_state::list();\nlet e = lc_state::list(); // batch-job: audit\n",
        );
        t.write("dwarf/x.sh", "spira-lc list --state WORKING\n# spira-lc list\n");
        t.write("strand/src/tests.rs", "let rows = lc_state::list();\n");
        t.write("strand/src/b.rs", "fn f() {}\n#[cfg(test)]\nmod t { fn x() { lc_state::list(); } }\n");
        t.write("spira-lc/src/y.rs", "let rows = lc_state::list();\n");
        let got = run(&t, &["sentinel/src/a.rs", "dwarf/x.sh", "strand/src/tests.rs", "strand/src/b.rs", "spira-lc/src/y.rs"]).unwrap();
        assert_eq!(got, Vec::<String>::new());
    }

    #[test]
    fn nothing_in_scope_refuses() {
        let t = TempDir::new("cfl-none");
        t.write("spira-lc/src/y.rs", "lc_state::list();\n");
        assert!(run(&t, &["spira-lc/src/y.rs"]).is_err());
    }
}
