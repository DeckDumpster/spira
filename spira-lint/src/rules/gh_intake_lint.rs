//! `gh-intake-lint` — `gh-intake.sh` never constructs a write to GitHub and never reads a
//! credential (law-beads-is-never-public). Contract: DESIGN.md.
//! Ported from `spira/gh-intake-lint.sh` (deleted).

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{lines, trim_lead, Entry, Finding, LintError, Rule, Tree};

pub struct GhIntakeLint;

const NAME: &str = "gh-intake-lint";
const TARGET: &str = "spira/gh-intake.sh";

fn match_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let write = r"curl[^|]*-X[ \t]*(POST|PATCH|PUT|DELETE)|--data|-d[ \t]";
        let cred = r"GITHUB_TOKEN|github\.token|Authorization:";
        Regex::new(&format!("({write})|({cred})")).expect("static regex")
    })
}

fn is_comment(line: &[u8]) -> bool {
    let start = line.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(line.len());
    line[start..].starts_with(b"#")
}

/// Hits in `content`: (1-based line, raw line text).
pub fn scan(content: &[u8]) -> Vec<(usize, String)> {
    if !match_re().is_match(content) {
        return Vec::new();
    }
    lines(content)
        .iter()
        .enumerate()
        .filter(|(_, l)| !is_comment(l) && match_re().is_match(l))
        .map(|(i, l)| (i + 1, String::from_utf8_lossy(l).into_owned()))
        .collect()
}

impl Rule for GhIntakeLint {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.path == TARGET
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let Some(content) = tree.text_of(TARGET) else {
            return Err(LintError::Refused(format!("{TARGET} is missing — refusing to report clean")));
        };
        let bytes = content.as_bytes();
        Ok(scan(bytes)
            .into_iter()
            .map(|(line, text)| Finding {
                rule: NAME,
                path: TARGET.to_string(),
                line: Some(line),
                message: trim_lead(text.as_bytes()),
            })
            .collect())
    }

    fn hint(&self) -> &'static str {
        "gh-intake.sh must only read the tracker (law-beads-is-never-public): no mutating curl \
flag, no credential reference."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_git(t.path()).unwrap();
        GhIntakeLint.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_missing_target_refuses() {
        let t = TempDir::new("gil-missing");
        t.git_init();
        t.write("spira/other.sh", "echo ok\n");
        t.git(&["add", "."]);
        assert!(matches!(run(&t), Err(LintError::Refused(_))));
    }

    #[test]
    fn every_offending_shape_is_caught_then_clears() {
        let hits = scan(
            b"#!/usr/bin/env bash\ncurl -X POST https://api.github.com/repos/x/y/issues\ncurl --data '{}' https://api.github.com\nif [ -n \"$GITHUB_TOKEN\" ]; then :; fi\necho \"$github.token\"\ncurl -H \"Authorization: token $t\"\n",
        );
        assert_eq!(hits.len(), 5, "{hits:?}");
        assert!(hits[0].1.contains("curl -X POST"));
        assert!(hits[1].1.contains("curl --data"));
        assert!(hits[2].1.contains("GITHUB_TOKEN"));
        assert!(hits[3].1.contains("github.token"));
        assert!(hits[4].1.contains("Authorization:"));

        let clean = scan(
            b"#!/usr/bin/env bash\n# gh-intake.sh must never construct a write or read GITHUB_TOKEN / Authorization:\ncurl -sS --max-time 30 \"https://api.github.com/repos/x/y/issues?state=open\"\n",
        );
        assert!(clean.is_empty());
    }

    #[test]
    fn the_shipped_target_reports_clean_over_a_correct_file() {
        let t = TempDir::new("gil-clean");
        t.git_init();
        t.write(TARGET, "#!/usr/bin/env bash\ncurl -sS \"https://api.github.com/repos/x/y/issues\"\n");
        t.git(&["add", "."]);
        assert!(run(&t).unwrap().is_empty());
    }
}
