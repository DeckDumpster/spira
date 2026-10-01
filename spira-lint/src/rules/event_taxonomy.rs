//! `event-taxonomy` — every event kind a `spira_event` call site emits is declared, every
//! declared kind fits the events table, and the named call sites are still wired.
//! Ported from `spira/test-event-taxonomy.sh`. Contract: DESIGN.md.

use std::sync::OnceLock;

use regex::Regex;

use crate::{allow_lines, direct_child, Entry, Finding, LintError, Rule, Tree};

pub struct EventTaxonomy;

const NAME: &str = "event-taxonomy";
/// The declared vocabulary: one kind per line.
pub const KINDS_FILE: &str = "spira-lint/event-kinds";

/// Call sites no suite can drive: (file, kind). A shell site must contain
/// `spira_event <kind> `; a Rust site the literal `"<kind>"`.
pub const WIRED: &[(&str, &str)] = &[
    ("gate-check/src/main.rs", "ci.failed"),
    ("sentinel/src/check4.rs", "bead.poisoned"),
    ("strand/src/check.rs", "branch.reclaimed"),
    ("landing-pass/src/push.rs", "bead.landed"),
    ("landing-pass/src/push.rs", "bead.reopened"),
];

/// A kind is lowercase dotted segments of `[a-z0-9]`, at most 32 characters (the column).
pub fn kind_valid(k: &str) -> Result<(), String> {
    if k.is_empty()
        || !k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.')
        || k.starts_with('.')
        || k.ends_with('.')
        || k.contains("..")
    {
        return Err("not lowercase dotted segments".into());
    }
    if k.len() > 32 {
        return Err(format!("{} chars, over the 32-char column", k.len()));
    }
    Ok(())
}

/// Every `spira_event <kind>` in the non-comment lines of one shell file: (line, kind).
pub fn emitted(text: &str) -> Vec<(usize, String)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"spira_event ([a-z0-9.]+)").expect("static regex"));
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim_start().starts_with('#') {
            continue;
        }
        for c in re.captures_iter(l) {
            out.push((i + 1, c[1].to_string()));
        }
    }
    out
}

impl Rule for EventTaxonomy {
    fn name(&self) -> &'static str {
        NAME
    }

    fn allow_file(&self) -> Option<&'static str> {
        Some(KINDS_FILE)
    }

    /// `spira/*.sh`, directly in spira/.
    fn applies_to(&self, e: &Entry) -> bool {
        direct_child(&e.path, "spira").is_some_and(|b| b.ends_with(".sh"))
    }

    fn check(&self, tree: &Tree) -> Result<Vec<Finding>, LintError> {
        let kinds_text = tree.read_text(KINDS_FILE);
        let kinds: Vec<String> = allow_lines(&kinds_text).iter().map(|l| l.trim().to_string()).collect();
        if kinds.is_empty() {
            return Err(LintError::BadAllow {
                file: KINDS_FILE.into(),
                line: 0,
                reason: "declares no event kinds — refusing to judge call sites against nothing".into(),
            });
        }
        let mut out = Vec::new();
        let mut any_site = false;
        for e in crate::scope(tree, self)? {
            let Some(c) = tree.content(e) else { continue };
            for (line, k) in emitted(&String::from_utf8_lossy(c)) {
                any_site = true;
                if !kinds.contains(&k) {
                    out.push(Finding {
                        rule: NAME,
                        path: e.path.clone(),
                        line: Some(line),
                        message: format!("emits undeclared kind '{k}' — declare it in {KINDS_FILE}, or fix the call site"),
                    });
                }
            }
        }
        // The WIRED check below proves the matcher is not measuring nothing just as well as a
        // shell `spira_event <kind>` call does — and, as the wave-4 Rust rewrite peels function
        // families out of lib.sh one at a time, every shell call site is eventually retired on
        // purpose (sp-8kqww: rapid_recur_check's `spira_event aeon.rapid` was the last one in
        // spira/*.sh). Scoping the positive control to shell-only made it fire on a retirement
        // that had nothing to do with the taxonomy itself.
        let mut any_wired = false;
        for (file, k) in WIRED {
            let needle = if file.ends_with(".rs") { format!("\"{k}\"") } else { format!("spira_event {k} ") };
            if tree.text_of(file).is_some_and(|t| t.contains(&needle)) {
                any_wired = true;
            } else {
                out.push(Finding {
                    rule: NAME,
                    path: file.to_string(),
                    line: None,
                    message: format!("no call site emits {k} (expected {needle:?})"),
                });
            }
        }
        if !any_site && !any_wired {
            // The positive control: a matcher that finds no call site measures itself.
            return Err(LintError::EmptyScope);
        }
        for (i, l) in kinds_text.lines().enumerate() {
            let k = l.trim();
            if k.is_empty() || k.starts_with('#') {
                continue;
            }
            if let Err(why) = kind_valid(k) {
                out.push(Finding { rule: NAME, path: KINDS_FILE.into(), line: Some(i + 1), message: format!("kind '{k}': {why}") });
            }
        }
        Ok(out)
    }

    fn hint(&self) -> &'static str {
        "A kind nobody declared is refused at send time. Adding a kind is a deliberate act: \
declare it in spira-lint/event-kinds (lowercase dotted, <= 32 chars)."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const KINDS: &str = "# kinds\nci.failed\nbead.poisoned\nbranch.reclaimed\nbead.landed\nbead.reopened\n";

    fn wired(t: &TempDir) -> Vec<String> {
        // Every WIRED entry is a Rust site now (gate-check moved to Rust, sp-ubw2o), so this
        // fixture also needs one `spira/*.sh` call site of its own — otherwise the first
        // scan phase finds no shell site at all and its own positive control (EmptyScope)
        // fires, which is a fact about this fixture, not about the rule being exercised.
        t.write("spira/an-example.sh", "spira_event bead.landed \"$id\"\n");
        t.write("gate-check/src/main.rs", "world.spira_event(\"ci.failed\", &blocked, &summary, esc);\n");
        t.write("sentinel/src/check4.rs", "emit(\"bead.poisoned\");\n");
        t.write("strand/src/check.rs", "emit(\"branch.reclaimed\");\n");
        t.write("landing-pass/src/push.rs", "emit(\"bead.landed\"); emit(\"bead.reopened\");\n");
        vec![
            "spira/an-example.sh".into(),
            "gate-check/src/main.rs".into(),
            "sentinel/src/check4.rs".into(),
            "strand/src/check.rs".into(),
            "landing-pass/src/push.rs".into(),
        ]
    }

    fn run(t: &TempDir, mut files: Vec<String>, extra: &[&str]) -> Result<Vec<String>, LintError> {
        files.extend(extra.iter().map(|s| s.to_string()));
        let tree = Tree::from_paths(t.path(), files, std::iter::empty::<String>());
        EventTaxonomy.check(&tree).map(|v| v.iter().map(|f| f.to_string()).collect())
    }

    #[test]
    fn a_planted_undeclared_kind_is_caught_and_a_declared_one_is_not() {
        let t = TempDir::new("ev");
        t.write(KINDS_FILE, KINDS);
        let files = wired(&t);
        t.write("spira/x.sh", "  spira_event bead.landed \"$id\"\n# spira_event not.real is a comment\n");
        assert!(run(&t, files.clone(), &["spira/x.sh"]).unwrap().is_empty());
        t.write("spira/x.sh", "if true; then spira_event bead.landd \"$id\"; fi\n");
        assert_eq!(
            run(&t, files, &["spira/x.sh"]).unwrap(),
            vec!["event-taxonomy: spira/x.sh:1: emits undeclared kind 'bead.landd' — declare it in spira-lint/event-kinds, or fix the call site"]
        );
    }

    #[test]
    fn declared_kinds_must_fit_the_column() {
        assert!(kind_valid("aeon.claimed").is_ok());
        assert!(kind_valid("Aeon.claimed").is_err());
        assert!(kind_valid(".x").is_err());
        assert!(kind_valid("x.").is_err());
        assert!(kind_valid("a..b").is_err());
        assert!(kind_valid(&"a".repeat(33)).is_err());
        let t = TempDir::new("ev-bad");
        t.write(KINDS_FILE, &format!("{KINDS}Bad.Kind\n"));
        let files = wired(&t);
        let got = run(&t, files, &[]).unwrap();
        assert_eq!(got, vec!["event-taxonomy: spira-lint/event-kinds:7: kind 'Bad.Kind': not lowercase dotted segments"]);
    }

    #[test]
    fn an_unwired_call_site_is_caught() {
        let t = TempDir::new("ev-wire");
        t.write(KINDS_FILE, KINDS);
        let files = wired(&t);
        t.write("strand/src/check.rs", "// branch.reclaimed moved\n");
        assert_eq!(
            run(&t, files, &[]).unwrap(),
            vec!["event-taxonomy: strand/src/check.rs: no call site emits branch.reclaimed (expected \"\\\"branch.reclaimed\\\"\")"]
        );
    }

    // sp-8kqww (wave 4.33): rapid_recur_check's `spira_event aeon.rapid` was the last literal
    // shell call site anywhere in spira/*.sh — every other family the wave peels out of
    // lib.sh will eventually retire its own shell call sites the same way. The positive
    // control must not fire just because the shell side of the taxonomy went fully native;
    // the WIRED Rust sites prove the matcher measures something on their own.
    #[test]
    fn wired_rust_sites_alone_satisfy_the_positive_control() {
        let t = TempDir::new("ev-rust-only");
        t.write(KINDS_FILE, KINDS);
        // A spira/*.sh direct child exists (so the rule's own file-scope is non-empty, as it
        // always is on the real tree — hundreds of them), but none of them calls
        // `spira_event` any more: the shell side of the taxonomy is fully retired.
        t.write("spira/unrelated.sh", "echo hi\n");
        t.write("gate-check/src/main.rs", "world.spira_event(\"ci.failed\", &blocked, &summary, esc);\n");
        t.write("sentinel/src/check4.rs", "emit(\"bead.poisoned\");\n");
        t.write("strand/src/check.rs", "emit(\"branch.reclaimed\");\n");
        t.write("landing-pass/src/push.rs", "emit(\"bead.landed\"); emit(\"bead.reopened\");\n");
        let files = vec!["spira/unrelated.sh".into(), "gate-check/src/main.rs".into(), "sentinel/src/check4.rs".into(), "strand/src/check.rs".into(), "landing-pass/src/push.rs".into()];
        assert_eq!(run(&t, files, &[]), Ok(vec![]));
    }

    #[test]
    fn no_call_site_at_all_is_a_refusal_not_a_pass() {
        let t = TempDir::new("ev-none");
        t.write(KINDS_FILE, KINDS);
        t.write("spira/x.sh", "echo\n");
        assert_eq!(run(&t, vec!["spira/x.sh".into()], &[]), Err(LintError::EmptyScope));
        t.write(KINDS_FILE, "");
        assert!(matches!(run(&t, vec!["spira/x.sh".into()], &[]), Err(LintError::BadAllow { .. })));
    }
}
