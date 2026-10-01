//! Pure logic behind the `rule` binary. Contract: DESIGN.md. Kept separate from `main.rs`
//! (which does the `bd`/hook subprocess work) so it is unit-testable with no database, no
//! filesystem and no subprocess.

pub mod memories;

use std::collections::BTreeMap;

/// `slugify("foo") == "law-foo"`, `slugify("law-foo") == "law-foo"` — the `law-` prefix is
/// stripped once (if present) then re-added, matching the bash `printf 'law-%s' "${1#law-}"`.
pub fn slugify(raw: &str) -> String {
    format!("law-{}", raw.strip_prefix("law-").unwrap_or(raw))
}

/// `wc -w` — whitespace-separated token count, the same measure the bash used for the
/// ~130-word statute-length refusal.
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The result of parsing `enact`'s trailing arguments (everything after `<slug>`).
#[derive(Debug, PartialEq, Eq)]
pub struct EnactArgs {
    pub dry_run: bool,
    pub text: String,
}

/// An `enact`-argument refusal: either the bash's `usage()` (printed to stdout, exit 1) or
/// a named refusal (printed to stderr, exit 1) — the two streams the bash kept distinct.
#[derive(Debug, PartialEq, Eq)]
pub enum EnactError {
    Usage,
    Message(String),
}

/// Parses `enact`'s arguments exactly as the bash did: one quoted text argument, optionally
/// `--dry-run`, anything else refused by name before anything is written (sp-dnrjw: a stray
/// flag no longer becomes law silently).
pub fn parse_enact_args(rest: &[String]) -> Result<EnactArgs, EnactError> {
    let mut dry_run = false;
    let mut text: Option<String> = None;
    for arg in rest {
        if arg == "--dry-run" {
            dry_run = true;
        } else if arg.starts_with('-') {
            return Err(EnactError::Message(format!(
                "rule: enact does not take '{arg}' — usage: rule.sh enact <slug> \"<text>\" [--dry-run]"
            )));
        } else if text.is_some() {
            return Err(EnactError::Message(format!(
                "rule: enact takes one quoted text argument; '{arg}' is a second one — quote the whole statute text"
            )));
        } else {
            text = Some(arg.clone());
        }
    }
    match text {
        Some(text) => Ok(EnactArgs { dry_run, text }),
        None => Err(EnactError::Usage),
    }
}

/// The three-line refusal for a statute over the word budget (unchanged text from `rule.sh`).
pub fn word_limit_refusal(words: usize) -> String {
    format!(
        "rule: refusing — {words} words. A statute is one paragraph (~70 words);\n      every agent pays this context on every session. Put the case history\n      in the wiki and keep the scar here as a single clause."
    )
}

/// `rule.sh`'s own doc-comment usage block (`sed -n '3,10p' "$0" | sed 's/^# \{0,1\}//'`),
/// pinned verbatim since this binary has no bash header comment of its own to read.
pub const USAGE: &str = "rule.sh — enact, amend, or retire a statute in one command.\n\n  rule.sh enact <slug> \"<statute text>\" [--dry-run]   write it, then synthesise it into the wiki\n  rule.sh retire <slug>                    remove it\n  rule.sh list                             what is in force\n  rule.sh show <slug>                      one statute's full text\n\n`<slug>` is written without the `law-` prefix; it is added for you.";

/// One `list` line: `  {key:<44} {words:>3}w  {first 64 chars of the first sentence}`.
pub fn list_line(key: &str, text: &str) -> String {
    let words = word_count(text);
    let first_sentence = text.split('.').next().unwrap_or(text);
    let clipped: String = first_sentence.chars().take(64).collect();
    format!("  {key:<44} {words:>3}w  {clipped}")
}

/// `list`'s full body: every `law-`-prefixed string value, sorted by key, one [`list_line`]
/// each, then the trailing count line. `memories` is the parsed `bd memories --json` object.
pub fn list_body(memories: &BTreeMap<String, serde_json::Value>) -> String {
    let laws: Vec<(&String, &str)> = memories
        .iter()
        .filter(|(k, _)| k.starts_with("law-"))
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s)))
        .collect();
    let mut out = String::new();
    for (k, v) in &laws {
        out.push_str(&list_line(k, v));
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&format!("{} statutes in force\n", laws.len()));
    out
}

/// One outcome of `commit_common_law`: whether the wiki checkout was committed, skipped (no
/// checkout configured), or the commit itself failed. Mirrors the bash's 0/1/2 return codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed,
    Skipped,
    Failed,
}

impl CommitOutcome {
    /// The line printed for this outcome (to stdout for Committed/Skipped, stderr for
    /// Failed — `main.rs` picks the stream; this just picks the words).
    pub fn message(self, verb: &str, key: &str) -> String {
        match self {
            CommitOutcome::Committed => {
                format!("wiki/notes/common-law.md committed (law: {verb} {key}).")
            }
            CommitOutcome::Skipped => {
                "Commit wiki/notes/common-law.md to replicate it off this box.".to_string()
            }
            CommitOutcome::Failed => {
                "wiki/notes/common-law.md commit FAILED — commit it manually.".to_string()
            }
        }
    }
}

/// The "database write succeeded, the wiki page was NOT regenerated" refusal's first line —
/// distinct wording for `enact` ("IS in the book") vs `retire` ("IS removed from the
/// book"), a distinction test-statute-projection.sh's sp-p0xyt case pins.
pub fn write_succeeded_line(verb: &str) -> &'static str {
    match verb {
        "retire" => "Statute IS removed from the book — the database write succeeded.",
        _ => "Statute IS in the book — the database write succeeded.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slugify_adds_prefix_once() {
        assert_eq!(slugify("foo"), "law-foo");
        assert_eq!(slugify("law-foo"), "law-foo");
        assert_eq!(slugify("law-law-foo"), "law-law-foo");
    }

    #[test]
    fn word_count_counts_whitespace_tokens() {
        assert_eq!(word_count("one two  three\tfour"), 4);
        assert_eq!(word_count(""), 0);
    }

    #[test]
    fn parse_enact_args_happy_path() {
        let got = parse_enact_args(&["some text".to_string()]).unwrap();
        assert_eq!(
            got,
            EnactArgs {
                dry_run: false,
                text: "some text".to_string()
            }
        );
    }

    #[test]
    fn parse_enact_args_dry_run_flag() {
        let got = parse_enact_args(&["--dry-run".to_string(), "some text".to_string()]).unwrap();
        assert!(got.dry_run);
        assert_eq!(got.text, "some text");
    }

    #[test]
    fn parse_enact_args_rejects_unknown_flag() {
        let err = parse_enact_args(&["--nope".to_string(), "text".to_string()]).unwrap_err();
        match err {
            EnactError::Message(m) => assert!(m.contains("does not take '--nope'"), "{m}"),
            EnactError::Usage => panic!("expected a named message, got Usage"),
        }
    }

    #[test]
    fn parse_enact_args_rejects_second_positional() {
        let err = parse_enact_args(&["first".to_string(), "second".to_string()]).unwrap_err();
        match err {
            EnactError::Message(m) => assert!(m.contains("is a second one"), "{m}"),
            EnactError::Usage => panic!("expected a named message, got Usage"),
        }
    }

    #[test]
    fn parse_enact_args_requires_text() {
        let err = parse_enact_args(&["--dry-run".to_string()]).unwrap_err();
        assert_eq!(err, EnactError::Usage);
    }

    #[test]
    fn word_limit_refusal_names_the_count() {
        assert!(word_limit_refusal(131).contains("131 words"));
    }

    #[test]
    fn list_line_truncates_to_first_sentence_64_chars() {
        let long = "a".repeat(100);
        let line = list_line("law-x", &format!("{long}. more."));
        assert!(line.starts_with("  law-x"));
        assert_eq!(word_count(&format!("{long}. more.")), 2);
        // exactly 64 'a's after the two label columns
        let tail = line.rsplit("  ").next().unwrap();
        assert_eq!(tail.chars().count(), 64);
    }

    #[test]
    fn list_body_filters_non_law_and_non_string() {
        let mut m = BTreeMap::new();
        m.insert("law-a".to_string(), json!("Statute A text."));
        m.insert("law-b".to_string(), json!("Statute B text."));
        m.insert("sop-c".to_string(), json!("not a law"));
        m.insert("law-d".to_string(), json!(42));
        let body = list_body(&m);
        assert!(body.contains("law-a"));
        assert!(body.contains("law-b"));
        assert!(!body.contains("sop-c"));
        assert!(!body.contains("law-d"));
        assert!(body.contains("2 statutes in force"));
    }

    #[test]
    fn list_body_sorted_by_key() {
        let mut m = BTreeMap::new();
        m.insert("law-zzz".to_string(), json!("Z."));
        m.insert("law-aaa".to_string(), json!("A."));
        let body = list_body(&m);
        let pos_a = body.find("law-aaa").unwrap();
        let pos_z = body.find("law-zzz").unwrap();
        assert!(pos_a < pos_z);
    }

    #[test]
    fn write_succeeded_line_differs_by_verb() {
        assert_eq!(
            write_succeeded_line("enact"),
            "Statute IS in the book — the database write succeeded."
        );
        assert_eq!(
            write_succeeded_line("retire"),
            "Statute IS removed from the book — the database write succeeded."
        );
    }

    #[test]
    fn commit_outcome_messages_match_bash_text() {
        assert_eq!(
            CommitOutcome::Committed.message("enact", "law-x"),
            "wiki/notes/common-law.md committed (law: enact law-x)."
        );
        assert_eq!(
            CommitOutcome::Skipped.message("enact", "law-x"),
            "Commit wiki/notes/common-law.md to replicate it off this box."
        );
        assert_eq!(
            CommitOutcome::Failed.message("enact", "law-x"),
            "wiki/notes/common-law.md commit FAILED — commit it manually."
        );
    }
}
