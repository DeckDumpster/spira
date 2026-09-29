//! `covers-entries` — every path glob in a suite's `# covers:` declaration resolves to
//! something in the tree. Ported from `spira/test-covers-entries.sh`. Contract: DESIGN.md.

use std::collections::BTreeSet;

use crate::{direct_child, Entry, Finding, LintError, Rule, Tree};

pub struct CoversEntries;

const NAME: &str = "covers-entries";

/// `suite_covers_of` (spira/suite-covers.sh): the first `# covers:` line's globs, folded with
/// its continuation lines — a comment line indented by two or more blanks that is not itself
/// a `# word:` directive. `None` when the suite declares nothing.
pub fn covers_of(text: &str) -> Option<Vec<String>> {
    let mut it = text.lines();
    let first = it.by_ref().find_map(|l| {
        let rest = l.strip_prefix('#')?.trim_start_matches(' ');
        rest.strip_prefix("covers:").map(|r| r.trim_start_matches(' ').to_string())
    })?;
    let mut out = first;
    for l in it {
        let Some(body) = l.strip_prefix('#') else { break };
        let blanks = body.len() - body.trim_start_matches([' ', '\t']).len();
        let rest = &body[blanks..];
        let is_directive = body.starts_with(' ')
            && body[1..].split_once(':').is_some_and(|(w, _)| {
                !w.is_empty()
                    && w.as_bytes()[0].is_ascii_alphabetic()
                    && w.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'-')
            });
        if blanks >= 2 && !rest.is_empty() && !rest.starts_with('#') && !is_directive {
            out.push(' ');
            out.push_str(rest);
        } else {
            break;
        }
    }
    Some(out.split_whitespace().map(str::to_string).collect())
}

/// A use-case (`UC-<area>-NN`) or gap (`G-NN`) id: a catalogue entry, never a path.
pub fn is_catalogue_token(t: &str) -> bool {
    let two_digits = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(r) = t.strip_prefix("G-") {
        return two_digits(r);
    }
    t.strip_prefix("UC-").is_some_and(|r| r.len() >= 3 && r.ends_with(|c: char| c.is_ascii_digit()) && {
        let (head, nn) = r.split_at(r.len() - 2);
        two_digits(nn) && head.ends_with('-')
    })
}

/// A shell pathname glob over one path: `*` and `?` never cross `/` nor match a leading `.`,
/// and `[…]` is a bracket expression (`!`/`^` negates).
pub fn glob_match(pat: &str, path: &str) -> bool {
    let (ps, ss): (Vec<&str>, Vec<&str>) = (pat.split('/').collect(), path.split('/').collect());
    ps.len() == ss.len() && ps.iter().zip(&ss).all(|(p, s)| seg_match(p.as_bytes(), s.as_bytes(), true))
}

fn seg_match(p: &[u8], s: &[u8], at_start: bool) -> bool {
    let dot_guard = |c: u8| !(at_start && c == b'.');
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => {
            if at_start && s.first() == Some(&b'.') {
                return false;
            }
            (0..=s.len()).any(|i| seg_match(rest, &s[i..], false))
        }
        Some((b'?', rest)) => s.first().is_some_and(|&c| dot_guard(c)) && seg_match(rest, &s[1..], false),
        Some((b'[', rest)) => {
            let Some(&c) = s.first() else { return false };
            if !dot_guard(c) {
                return false;
            }
            match bracket(rest, c) {
                Some((true, after)) => seg_match(after, &s[1..], false),
                Some((false, _)) => false,
                None => c == b'[' && seg_match(rest, &s[1..], false), // unterminated: a literal
            }
        }
        Some((b'\\', rest)) if !rest.is_empty() => s.first() == Some(&rest[0]) && seg_match(&rest[1..], &s[1..], false),
        Some((&c, rest)) => s.first() == Some(&c) && seg_match(rest, &s[1..], false),
    }
}

/// `[…]` after its `[`: whether `c` is in it, and the pattern after the `]`.
fn bracket(p: &[u8], c: u8) -> Option<(bool, &[u8])> {
    let (neg, mut i) = match p.first() {
        Some(b'!') | Some(b'^') => (true, 1),
        _ => (false, 0),
    };
    let start = i;
    let mut hit = false;
    while i < p.len() {
        if p[i] == b']' && i > start {
            return Some((hit != neg, &p[i + 1..]));
        }
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            hit |= p[i] <= c && c <= p[i + 2];
            i += 3;
        } else {
            hit |= p[i] == c;
            i += 1;
        }
    }
    None
}

/// Every path the tree holds: the walk's files and each of their parent directories.
pub fn tree_paths(tree: &Tree) -> BTreeSet<String> {
    let mut all = BTreeSet::new();
    for e in &tree.entries {
        let mut p = e.path.as_str();
        all.insert(p.to_string());
        while let Some((parent, _)) = p.rsplit_once('/') {
            if !all.insert(parent.to_string()) {
                break;
            }
            p = parent;
        }
    }
    all
}

impl Rule for CoversEntries {
    fn name(&self) -> &'static str {
        NAME
    }

    /// `spira/test-*.sh`, directly in spira/.
    fn applies_to(&self, e: &Entry) -> bool {
        direct_child(&e.path, "spira").is_some_and(|b| b.starts_with("test-") && b.ends_with(".sh"))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let paths = tree_paths(tree);
        let resolves = |tok: &str| {
            if tok.contains(['*', '?', '[']) {
                paths.iter().any(|p| glob_match(tok, p))
            } else {
                paths.contains(tok.trim_end_matches('/'))
            }
        };
        let mut out = Vec::new();
        let mut declared = 0usize;
        for e in crate::scope(tree, self)? {
            let Some(c) = tree.content(e) else { continue };
            let Some(toks) = covers_of(&String::from_utf8_lossy(c)) else { continue };
            if toks.is_empty() {
                continue;
            }
            declared += 1;
            for t in toks.iter().filter(|t| !is_catalogue_token(t)) {
                if !resolves(t) {
                    out.push(Finding {
                        rule: NAME,
                        path: e.path.clone(),
                        line: None,
                        message: format!("# covers: token '{t}' matches no file in the tree"),
                    });
                }
            }
        }
        if declared == 0 {
            return Err(LintError::EmptyScope);
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A # covers: glob that matches nothing selects the suite for nothing: fix the path, or \
drop it when the file it named is gone."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn run(t: &TempDir, files: &[&str]) -> Result<Vec<String>, LintError> {
        let tree = Tree::from_paths(t.path(), files.iter().copied(), std::iter::empty::<&str>());
        CoversEntries.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_unresolvable_token_is_caught_then_clears() {
        let t = TempDir::new("cov");
        t.write("spira/lib.sh", "");
        t.write("spira/test-pc.sh", "#!/bin/bash\n# covers: spira/lib.sh (pc_bad)\n");
        assert_eq!(
            run(&t, &["spira/lib.sh", "spira/test-pc.sh"]).unwrap(),
            vec!["covers-entries: spira/test-pc.sh: # covers: token '(pc_bad)' matches no file in the tree"]
        );
        t.write("spira/test-pc.sh", "#!/bin/bash\n# covers: spira/lib.sh spira/*.sh UC-area-01 G-07\n");
        assert!(run(&t, &["spira/lib.sh", "spira/test-pc.sh"]).unwrap().is_empty());
    }

    #[test]
    fn continuation_lines_fold_and_directives_end_the_block() {
        let text = "#!/bin/bash\n# covers: a.sh\n#   b.sh c/*\n# tier: T1\n#   d.sh\n";
        assert_eq!(covers_of(text).unwrap(), vec!["a.sh", "b.sh", "c/*"]);
        assert_eq!(covers_of("# covers: a\n#  x: y\n").unwrap(), vec!["a", "x:", "y"]);
        assert_eq!(covers_of("# covers: a\n# hostreason: y\n").unwrap(), vec!["a"]);
        assert_eq!(covers_of("#covers: a\n# covers: b\n").unwrap(), vec!["a"]);
        assert!(covers_of("# tier: T1\n").is_none());
    }

    #[test]
    fn globs_follow_the_shell_not_git() {
        assert!(glob_match("spira/*.sh", "spira/a.sh"));
        assert!(!glob_match("spira/*.sh", "spira/hooks/a.sh"));
        assert!(!glob_match("spira/*", "spira/.hidden"));
        assert!(glob_match("spira-config/*", "spira-config/src"));
        assert!(glob_match("spira/test-[a-c]?.sh", "spira/test-b1.sh"));
        assert!(!glob_match("spira/test-[!a-c]?.sh", "spira/test-b1.sh"));
        assert!(is_catalogue_token("UC-cockpit-observability-20"));
        assert!(is_catalogue_token("G-07"));
        assert!(!is_catalogue_token("UC-x-1"));
        assert!(!is_catalogue_token("spira/UC-a-01.sh"));
    }

    #[test]
    fn a_directory_token_resolves_through_its_files() {
        let t = TempDir::new("cov-dir");
        t.write("spira/test-fixtures/ci/a.yml", "");
        t.write("spira/test-d.sh", "# covers: spira/test-fixtures/ci .github/workflows/gate.yml\n");
        assert_eq!(
            run(&t, &["spira/test-fixtures/ci/a.yml", "spira/test-d.sh"]).unwrap(),
            vec!["covers-entries: spira/test-d.sh: # covers: token '.github/workflows/gate.yml' matches no file in the tree"]
        );
    }

    #[test]
    fn no_declaration_anywhere_is_a_refusal() {
        let t = TempDir::new("cov-none");
        t.write("spira/test-a.sh", "echo\n");
        assert_eq!(run(&t, &["spira/test-a.sh"]), Err(LintError::EmptyScope));
    }
}
