//! `bare-unit-name` — non-test code never spells an installed spira unit as a bare
//! `spira-<name>.service` / `.timer` literal; installed units carry the instance suffix, so it
//! names a unit that is not there. Resolve it through `spira_config::unit`. Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::lex::rust;
use crate::rust_test::{shape, whole_test_files, Shape};
use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct BareUnitName;

const NAME: &str = "bare-unit-name";
const ALLOW_FILE: &str = "spira-lint/bare-unit-name-allow";
const EXEMPT: &[&str] = &["target/", "install/", "spira-lint/", "testkit/", "spira-config/src/unit.rs"];

impl Rule for BareUnitName {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path.ends_with(".rs") && !EXEMPT.iter().any(|p| e.path.starts_with(p))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        static LIT: OnceLock<Regex> = OnceLock::new();
        let lit = LIT.get_or_init(|| Regex::new(r"spira-[a-z0-9]+(?:-[a-z0-9]+)*\.(?:service|timer)\b").expect("static regex"));
        let files = crate::scope(tree, self)?;
        let shapes: Vec<(&Entry, Shape)> =
            files.iter().filter_map(|e| tree.content(e).map(|c| (*e, shape(c)))).collect();
        let whole = whole_test_files(tree, &shapes);

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
        for (e, s) in &shapes {
            if whole.contains(&e.path) {
                continue;
            }
            let Some(src) = tree.content(e) else { continue };
            let cl = rust::classify(src);
            let text = cl.without_comments();
            let hits: Vec<_> = lit.find_iter(&text).filter(|m| !s.covers(m.start())).collect();
            if hits.is_empty() {
                continue;
            }
            offenders.insert(e.path.clone());
            if allow.iter().any(|(_, p)| p == &e.path) {
                continue;
            }
            for m in hits {
                out.push(Finding {
                    rule: NAME,
                    path: e.path.clone(),
                    line: Some(cl.line_of(m.start())),
                    message: format!(
                        "bare unit name {} — installed units carry the instance suffix; resolve it through spira_config::unit",
                        String::from_utf8_lossy(&text[m.start()..m.end()])
                    ),
                });
            }
        }
        for (line, p) in &allow {
            if !offenders.contains(p) {
                out.push(Finding {
                    rule: NAME,
                    path: ALLOW_FILE.to_string(),
                    line: Some(*line),
                    message: format!("lists {p}, which no longer needs an exception — remove the line (the list only shrinks)"),
                });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "An installed spira unit is named spira-<name>-<instance>.service, so a bare \
spira-<name>.service passed to systemctl or shown in a hint names a unit that is not there \
(a restart or poll of it fails 'not found'). Build the name with spira_config::unit::unit_name \
(or resolve_unit when the unit may predate the suffix). spira-lint/bare-unit-name-allow only shrinks."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(BareUnitName)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        BareUnitName.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_bare_unit_literal_in_code_is_caught_and_the_instance_form_is_not() {
        let t = TempDir::new("bareunit");
        t.write(ALLOW_FILE, "# shrink-only\n");
        t.write("s/src/p.rs", "pub const UNIT: &str = \"spira-submission.service\";\n");
        t.write("ok/src/p.rs", "// spira-submission.service in a comment\nfn f(i: &str) -> String { format!(\"spira-submission-{i}.service\") }\n#[cfg(test)]\nmod tests { const U: &str = \"spira-x.service\"; }\n");
        let got = run(&t, &["s/src/p.rs", "ok/src/p.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("bare-unit-name: s/src/p.rs:1:"), "{got:?}");
    }

    #[test]
    fn an_allow_line_with_no_offender_is_itself_a_finding() {
        let t = TempDir::new("bareunit-stale");
        t.write(ALLOW_FILE, "s/src/p.rs\n");
        t.write("s/src/p.rs", "fn f() {}\n");
        let got = run(&t, &["s/src/p.rs"]).unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].contains("no longer needs an exception"), "{got:?}");
    }
}
