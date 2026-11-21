//! `counter-label-ban` — no harness script writes an attempt/reclaim/requeue/timeout/recur
//! counter label, and `bump_requeue`/`bump_poison_cleared` in `spira/lib.sh` write none.
//! Counters are events, never labels. Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::bytes::Regex;

use crate::{Entry, Finding, LintError, Rule, Tree};

pub struct CounterLabelBan;

const NAME: &str = "counter-label-ban";
const LIB: &str = "spira/lib.sh";
const BUMPS: &[&str] = &["bump_requeue", "bump_poison_cleared"];

fn banned_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"label add.*(sp-attempt-|sp-reclaim|sp-requeue|sp-timeout-|sp-recur-)").expect("static regex"))
}

fn writes_label_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"label add|bump_counter").expect("static regex"))
}

fn is_comment(line: &[u8]) -> bool {
    line.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'#')
}

/// 1-based lines of non-comment `label add` writes naming a counter label.
pub fn label_writes(content: &[u8]) -> Vec<usize> {
    crate::lines(content)
        .iter()
        .enumerate()
        .filter(|(_, l)| !is_comment(l) && banned_re().is_match(l))
        .map(|(i, _)| i + 1)
        .collect()
}

/// The body of shell function `name` (from its `name()` line to a closing `}` line, or the same line), with the
/// line it starts on; `None` when the function is not defined.
pub fn function_body(content: &[u8], name: &str) -> Option<(usize, String)> {
    let head = format!("{name}()");
    let lines = crate::lines(content);
    let start = lines.iter().position(|l| l.starts_with(head.as_bytes()))?;
    let mut body = String::new();
    for (i, l) in lines[start..].iter().enumerate() {
        let one_line = i == 0 && l.ends_with(b"}");
        if !is_comment(l) {
            body.push_str(&String::from_utf8_lossy(l));
            body.push('\n');
        }
        if one_line || *l == b"}" {
            break;
        }
    }
    Some((start + 1, body))
}

impl Rule for CounterLabelBan {
    fn name(&self) -> &'static str {
        NAME
    }

    fn applies_to(&self, e: &Entry) -> bool {
        e.tracked
            && e.path.starts_with("spira/")
            && e.path.ends_with(".sh")
            && !e.path.starts_with("spira/test-")
            && !e.path.contains("/tests/")
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let files = crate::scope(tree, self)?;
        let mut out = Vec::new();
        for e in files {
            if e.path == LIB {
                continue;
            }
            let Some(content) = tree.content(e) else { continue };
            for line in label_writes(content) {
                out.push(Finding { rule: NAME, path: e.path.clone(), line: Some(line), message: "writes a counter label".into() });
            }
        }
        let lib = tree.entries.iter().find(|e| e.path == LIB).and_then(|e| tree.content(e));
        let Some(lib) = lib else {
            return Err(LintError::EmptyScope);
        };
        for f in BUMPS {
            match function_body(lib, f) {
                None => out.push(Finding { rule: NAME, path: LIB.into(), line: None, message: format!("{f} is not defined, so its label-free body cannot be checked") }),
                Some((line, body)) if writes_label_re().is_match(body.as_bytes()) => {
                    out.push(Finding { rule: NAME, path: LIB.into(), line: Some(line), message: format!("{f} writes a label") })
                }
                Some(_) => {}
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "Attempt, reclaim, requeue, timeout and recur counts are bd events, not labels. Write \
the event (bump_*), never `label add sp-attempt-*` and the like."
    }
}

pub fn rules() -> Vec<Box<dyn crate::Rule>> {
    vec![Box::new(CounterLabelBan)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tis6cs8)]
    #[test]
    fn a_counter_label_write_is_found_and_a_comment_or_other_label_is_not() {
        assert_eq!(label_writes(b"x\nbdq label add \"$b\" sp-attempt-3\n"), vec![2]);
        assert_eq!(label_writes(b"bdq label add b sp-reclaim\n"), vec![1]);
        assert!(label_writes(b"# bdq label add b sp-attempt-1\nbdq label add b plan\n").is_empty());
    }

    #[test_id(tetoj6o)]
    #[test]
    fn a_bump_that_writes_a_label_is_found_and_an_event_write_is_not() {
        let bad = b"bump_requeue() {\n  bdq label add \"$1\" x\n}\nother() { :; }\n";
        let (line, body) = function_body(bad, "bump_requeue").unwrap();
        assert_eq!(line, 1);
        assert!(writes_label_re().is_match(body.as_bytes()));
        let ok = b"bump_requeue() { _bump_write_event \"$1\" requeued; }\nbdq label add b sp-x\n";
        let (_, body) = function_body(ok, "bump_requeue").unwrap();
        assert!(!writes_label_re().is_match(body.as_bytes()));
        assert!(function_body(ok, "bump_poison_cleared").is_none());
    }

    fn run(files: &[(&str, &str)]) -> Result<Vec<Finding>, LintError> {
        let t = crate::testutil::TempDir::new("clb");
        for (p, c) in files {
            t.write(p, c);
        }
        let tree = Tree::from_paths(t.path(), files.iter().map(|(p, _)| *p), std::iter::empty::<&str>());
        CounterLabelBan.check(&tree)
    }

    const CLEAN_LIB: &str = "bump_requeue() { _bump_write_event \"$1\" requeued; }\nbump_poison_cleared() { :; }\n";

    #[test_id(tuzjml1)]
    #[test]
    fn check_finds_a_script_writer_and_ignores_suites_and_the_shim() {
        let bad = "bdq label add \"$b\" sp-attempt-2\n";
        let f = run(&[("spira/lib.sh", CLEAN_LIB), ("spira/a.sh", bad), ("spira/test-a.sh", bad)]).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "spira/a.sh");
        assert_eq!(f[0].line, Some(1));
    }

    #[test_id(t4ebwac)]
    #[test]
    fn check_finds_a_bump_that_writes_a_label_past_an_inner_brace() {
        let lib = "bump_requeue() {\n  x=\"${y}\"\n  # label add in prose\n  bdq label add \"$1\" l\n}\nbump_poison_cleared() { :; }\n";
        let f = run(&[("spira/lib.sh", lib)]).unwrap();
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("bump_requeue"));
        let prose = "bump_requeue() {\n  # never label add\n  :\n}\nbump_poison_cleared() { :; }\n";
        assert!(run(&[("spira/lib.sh", prose)]).unwrap().is_empty());
    }

    #[test_id(t85t08s)]
    #[test]
    fn check_reports_a_missing_bump_and_refuses_without_lib() {
        let f = run(&[("spira/lib.sh", "bump_requeue() { :; }\n")]).unwrap();
        assert_eq!(f.len(), 1);
        assert!(f[0].message.contains("bump_poison_cleared"));
        assert!(run(&[("spira/a.sh", "true\n")]).is_err());
    }
}
