//! `wiki-add-fence` — refuse blanket git-add forms targeting SPIRA_WIKI.
//! Contract: DESIGN.md. Ported from `spira/wiki-add-fence.sh` (deleted).
//!
//! Blanket staging on the wiki checkout (`git add -A`, `git add .`, `git commit -a`) sweeps
//! another actor's uncommitted work into the commit (incident: sp-4fl2e). `wiki-commit.sh` is
//! the canonical path; it stages each file explicitly.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct WikiAddFence;

const NAME: &str = "wiki-add-fence";
/// This rule's own source names the offending forms; it must never flag itself.
const OWN_SOURCE: &str = "spira-lint/src/rules/wiki_add_fence.rs";

fn blanket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"add -A|add \.[\s;|&]|add \.$|commit -a").expect("static regex"))
}

fn is_comment_line(line: &[u8]) -> bool {
    let start = line.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(line.len());
    line[start..].starts_with(b"#")
}

/// Non-comment lines mentioning `SPIRA_WIKI` alongside a blanket-add form: (1-based line, raw
/// line text).
pub fn scan(content: &[u8]) -> Vec<(usize, String)> {
    if !content.windows(b"SPIRA_WIKI".len()).any(|w| w == b"SPIRA_WIKI") {
        return Vec::new();
    }
    crate::lines(content)
        .iter()
        .enumerate()
        .filter(|(_, l)| !is_comment_line(l) && l.windows(b"SPIRA_WIKI".len()).any(|w| w == b"SPIRA_WIKI"))
        .filter(|(_, l)| blanket_re().is_match(l))
        .map(|(i, l)| (i + 1, String::from_utf8_lossy(l).into_owned()))
        .collect()
}

impl Rule for WikiAddFence {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.tracked && e.path.ends_with(".sh")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in files {
            if e.path == OWN_SOURCE {
                continue;
            }
            let Some(content) = tree.content(e) else { continue };
            for (line, text) in scan(content) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A blanket git-add on the wiki checkout sweeps another actor's uncommitted work into \
the commit. Use wiki-commit.sh — it stages each file explicitly."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_git(t.path()).unwrap();
        WikiAddFence.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn empty_scope_refuses_then_add_dash_a_is_seen_red_and_withdrawn() {
        let t = TempDir::new("waf");
        t.git_init();
        assert_eq!(run(&t), Err(LintError::EmptyScope));

        t.write("spira/helper.sh", "echo ok\n");
        t.git(&["add", "."]);
        assert!(run(&t).unwrap().is_empty());

        t.write(
            "spira/broken-wiki.sh",
            "#!/usr/bin/env bash\ngit -C \"$SPIRA_WIKI\" add -A\ngit -C \"$SPIRA_WIKI\" commit -m \"wiki writes\"\n",
        );
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("broken-wiki.sh"));
        assert!(got[0].contains("add -A"));

        t.remove("spira/broken-wiki.sh");
        t.git(&["add", "-A"]);
        assert!(run(&t).unwrap().is_empty());
    }

    #[test]
    fn add_dot_is_caught_and_comment_lines_are_not() {
        let hits = scan(b"#!/usr/bin/env bash\ngit -C \"$SPIRA_WIKI\" add .\n");
        assert_eq!(hits.len(), 1);

        let clean = scan(b"#!/usr/bin/env bash\n# do not use git -C \"$SPIRA_WIKI\" add -A (it sweeps unrelated work)\nprintf '%s\\n' \"$SPIRA_WIKI\"\n");
        assert!(clean.is_empty());
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![
        Box::new(WikiAddFence),
    ]
}
