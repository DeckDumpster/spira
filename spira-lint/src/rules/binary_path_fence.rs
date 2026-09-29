//! `binary-path-fence` — no hardcoded build-output path outside conf.sh's spira_bin.
//! Contract: DESIGN.md.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{lines, scope, trim_lead, Entry, Finding, LintError, Rule, Tree};

pub struct BinaryPathFence;

const NAME: &str = "binary-path-fence";
const ALLOW_FILE: &str = "spira/binary-path-fence-allow";
/// This rule's own source spells out every pattern it hunts.
const OWN_SOURCE: &str = "spira-lint/src/rules/binary_path_fence.rs";
pub const PATTERNS: &[&str] = &["target/release/", "target/debug/", "target/aeon/", "bin/spira-"];

/// `spira/binary-path-fence-allow`: exact paths; `#` starts a comment anywhere on a line.
pub struct BinaryPathAllow(BTreeSet<String>);

impl BinaryPathAllow {
    pub fn parse(text: &str) -> BinaryPathAllow {
        BinaryPathAllow(
            text.lines()
                .map(|l| l.split('#').next().unwrap_or("").trim())
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }
    pub fn covers(&self, path: &str) -> bool {
        self.0.contains(path)
    }
}

fn pattern_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alts: Vec<String> = PATTERNS.iter().map(|p| regex::escape(p)).collect();
        Regex::new(&alts.join("|")).expect("static regex")
    })
}

/// `path-ok:` followed by a non-empty reason.
fn marked(line: &[u8]) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"path-ok:[ \t]*\S").expect("static regex")).is_match(line)
}

/// Text: no NUL byte and at least one byte that is not a newline (`grep -qI .`).
fn is_text(content: &[u8]) -> bool {
    !content.contains(&0) && content.iter().any(|&b| b != b'\n')
}

/// Hits in one file: (1-based line, line text with leading whitespace stripped).
pub fn scan(content: &[u8]) -> Vec<(usize, String)> {
    if !pattern_re().is_match(content) {
        return Vec::new();
    }
    let ls = lines(content);
    let mut out = Vec::new();
    for (i, l) in ls.iter().enumerate() {
        if marked(l) || (i > 0 && marked(ls[i - 1])) {
            continue;
        }
        if pattern_re().is_match(l) {
            out.push((i + 1, trim_lead(l)));
        }
    }
    out
}

impl Rule for BinaryPathFence {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(ALLOW_FILE)
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.tracked
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = scope(tree, self)?;
        let allow = BinaryPathAllow::parse(&tree.read_text(ALLOW_FILE));
        let mut out = Vec::new();
        for e in files {
            let p = e.path.as_str();
            if p == ALLOW_FILE || p == OWN_SOURCE || allow.covers(p) {
                continue;
            }
            if p.ends_with(".md") || p.ends_with(".json") || p.ends_with(".tsv") {
                continue;
            }
            let Some(content) = tree.content(e) else { continue };
            if !is_text(content) {
                continue;
            }
            for (line, text) in scan(content) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: text });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "The lines above name a binary's build-output path instead of resolving it through \
conf.sh's spira_bin — a second resolver that silently assumes one tree. If the file IS the \
build/release machinery, add it to spira/binary-path-fence-allow; if one line has to, mark it \
`# path-ok: <reason>` on the line or the one above it."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_git(t.path()).unwrap();
        BinaryPathFence.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    // ── the bash suite's cases, in its order ──────────────────────────────────────────

    #[test]
    fn walk_plant_escapes_and_withdrawal() {
        let t = TempDir::new("bpf");
        t.git_init();
        assert_eq!(run(&t), Err(LintError::EmptyScope), "an empty index refuses to report clean");

        t.write("spira/planted.sh", "#!/usr/bin/env bash\nBIN=\"$SPIRA_REPO/target/release/reconciler\"\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got, vec!["binary-path-fence: spira/planted.sh:2: BIN=\"$SPIRA_REPO/target/release/reconciler\""]);
        assert!(BinaryPathFence.hint().contains("path-ok"), "the hint names the escape hatch");

        t.write("spira/planted2.sh", "#!/usr/bin/env bash\nBIN=\"$SPIRA_REPO/bin/spira-supervise\"\n");
        t.git(&["add", "."]);
        assert!(run(&t).unwrap().iter().any(|l| l.contains("spira/planted2.sh:2")), "bin/spira-<name> too");
        t.git(&["rm", "-qf", "spira/planted2.sh"]);

        t.write("spira/marked.sh", "#!/usr/bin/env bash\nBIN=\"$R/target/release/marked\" # path-ok: test fixture\n");
        t.write("spira/binary-path-fence-allow", "spira/allowed.sh  # the build machinery\n");
        t.write("spira/allowed.sh", "#!/usr/bin/env bash\nBIN=\"$R/target/release/allowed\"\n");
        t.write("docs.md", "# see target/release/doc-only\n");
        t.write("data.json", "{\"note\":\"target/release/fixture\"}\n");
        t.git(&["add", "."]);
        let got = run(&t).unwrap();
        assert_eq!(got.len(), 1, "only the plant: {got:?}");
        assert!(got[0].contains("spira/planted.sh"));

        t.git(&["rm", "-qf", "spira/planted.sh"]);
        assert!(run(&t).unwrap().is_empty());
    }

    // ── the marker and the text test ──────────────────────────────────────────────────

    #[test]
    fn marker_needs_a_reason_and_covers_the_next_line() {
        let src = b"# path-ok: builds it\nX=target/debug/a\nY=target/aeon/b # path-ok\nZ=target/debug/c\n";
        let hits: Vec<usize> = scan(src).into_iter().map(|h| h.0).collect();
        assert_eq!(hits, vec![3, 4], "bare `path-ok` with no reason exempts nothing");
    }

    #[test]
    fn untracked_binary_and_empty_files_are_not_scanned() {
        let t = TempDir::new("bpf-skip");
        t.git_init();
        t.write("keep.txt", "fine\n");
        t.write("blob.bin", "target/release/x\0\n");
        t.write("blank.txt", "\n\n");
        t.git(&["add", "."]);
        t.write("new.sh", "target/release/x\n");
        assert!(run(&t).unwrap().is_empty());
    }
}
