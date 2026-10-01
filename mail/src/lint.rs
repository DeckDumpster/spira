//! `_lint_check`: the transport discipline every `send` runs before a message touches disk.
//! A pure function of its arguments plus the kind files (test-mail.sh's own framing) — every
//! rule below is independently testable and this module carries the unit tests moved out of
//! test-mail.sh's old in-process lint table (that table sourced mail.sh as bash; a compiled
//! binary cannot be sourced, so the table becomes Rust tests here instead, `cargo test`).
//!
//! Every refusal names the rule (law-a-refusal-names-the-rule) and the messages below are
//! byte-for-byte what mail.sh printed, since callers (and test-mail.sh's own `want` greps
//! on stderr substrings, where that suite still shells out) key off this exact text.

use std::path::Path;

use regex::Regex;

use crate::kinds;

pub struct LintInput<'a> {
    pub from: &'a str,
    pub subject: &'a str,
    pub kind: &'a str,
    pub default: &'a str,
    pub urgent: bool,
    pub body: &'a str,
    pub digest: bool,
}

/// Runs every rule (never short-circuiting on the first failure, matching mail.sh, whose
/// `fail=1` accumulates) and returns every refusal line joined by newlines, or `Ok(())`.
pub fn lint_check(input: &LintInput, kinds_dir: &Path, id_prefix: &str) -> Result<(), String> {
    let mut msgs: Vec<String> = Vec::new();
    let bead_re = bead_id_regex(id_prefix);

    if input.from.is_empty() {
        msgs.push("mail: lint: missing From — rule: every message must name a sender".to_string());
    } else if group_syntax_re().is_match(input.from) {
        msgs.push(
            "mail: lint: From has group syntax — rejected (RFC 6854 §3 requires a replyable mailbox). Use: Name <local@spira>.".to_string(),
        );
    } else if !input.from.contains('@') {
        msgs.push(format!(
            "mail: lint: From has no address: {0}. Use: {0} <local@spira>.",
            input.from
        ));
    }

    if input.subject.is_empty() {
        msgs.push("mail: lint: missing Subject — rule: every message must carry a subject".to_string());
    } else if subject_leads_with_bead_id(input.subject, &bead_re) {
        msgs.push(
            "mail: lint: Subject leads with a bead id — rule: Subject is the human topic; put the id in --bead".to_string(),
        );
    }

    if bead_re.is_match(input.body) {
        let metadata_line = Regex::new(r"^[a-z_-]+:\s").unwrap();
        for line in input.body.lines() {
            if !bead_re.is_match(line) {
                continue;
            }
            if metadata_line.is_match(line) {
                continue;
            }
            let stripped = bead_re.replace_all(line, "");
            if stripped.split_whitespace().count() < 4 {
                msgs.push(
                    "mail: lint: body names a bead id without saying what the work is — rule: describe the work, not just the id".to_string(),
                );
                break;
            }
        }
    }

    if !input.kind.is_empty() {
        match kinds::load(kinds_dir, input.kind) {
            None => msgs.push(format!(
                "mail: lint: unknown kind {} — rule: kind must be a file in {}",
                input.kind,
                kinds_dir.display()
            )),
            Some(k) => {
                if k.requires.iter().any(|h| h == "X-Spira-Default") && input.default.is_empty() {
                    msgs.push(format!(
                        "mail: lint: kind {} requires X-Spira-Default — rule: supply --default",
                        input.kind
                    ));
                }
                if k.requires.iter().any(|h| h == "X-Spira-Urgent") && !input.urgent {
                    msgs.push(format!(
                        "mail: lint: kind {} requires X-Spira-Urgent — rule: supply --urgent",
                        input.kind
                    ));
                }
                for section in &k.sections {
                    if kinds::section_empty(section, input.body) {
                        msgs.push(format!(
                            "mail: lint: section \"{}\" is empty — rule: every {} section must be filled",
                            section, input.kind
                        ));
                    }
                }
            }
        }
    }

    if input.urgent && kinds::section_empty("Why it is urgent", input.body) {
        msgs.push(
            "mail: lint: urgent message missing \"## Why it is urgent\" — rule: urgent messages must explain urgency".to_string(),
        );
    }

    if input.kind == "note" && input.from.contains("archivist@spira") && !input.digest {
        msgs.push(
            "mail: lint: archivist note refused — findings go to a bead or the wiki first; only the daily digest (--digest) may mail a note (law-fail-closed-at-the-source)".to_string(),
        );
    }

    if ask_below_re().is_match(input.body) && !question_or_decision_heading_re().is_match(input.body) {
        msgs.push(
            "mail: lint: body promises an ask below but has no ## Question or ## Decision section — rule: carry the ask in this message or remove the promise".to_string(),
        );
    }

    if msgs.is_empty() {
        Ok(())
    } else {
        Err(msgs.join("\n"))
    }
}

pub fn bead_id_regex(id_prefix: &str) -> Regex {
    Regex::new(&format!("{}-[a-z0-9]{{4,}}", regex::escape(id_prefix))).expect("bead id regex")
}

fn subject_leads_with_bead_id(subject: &str, bead_re: &Regex) -> bool {
    if let Some(m) = bead_re.find(subject) {
        if m.start() == 0 {
            let rest = &subject[m.end()..];
            return rest.is_empty() || rest.starts_with(':') || rest.starts_with(char::is_whitespace);
        }
    }
    false
}

fn group_syntax_re() -> Regex {
    Regex::new(r";\s*$").unwrap()
}

fn ask_below_re() -> Regex {
    Regex::new(r"(?i)\b(the) (ask|question|decision) below\b").unwrap()
}

fn question_or_decision_heading_re() -> Regex {
    Regex::new(r"(?m)^## (Question|Decision)").unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn kinds_tmp() -> testkit::TempDir {
        let d = testkit::TempDir::new("mail-lint-kinds");
        fs::write(d.path().join("note.md"), "---\n---\n\n## Note\n").unwrap();
        fs::write(d.path().join("question.md"), "---\nrequires: X-Spira-Default\n---\n\n## Question\n\n## Default\n").unwrap();
        fs::write(d.path().join("decision.md"), "---\nrequires: X-Spira-Default\n---\n\n## Decision\n\n## Default\n\n## What is blocked\n\n## Cost of the wrong choice\n").unwrap();
        fs::write(d.path().join("suit.md"), "---\n---\n\n## Suit\n\n## Grounds\n\n## Relief sought\n").unwrap();
        d
    }

    fn check(input: LintInput, kinds_dir: &Path) -> Result<(), String> {
        lint_check(&input, kinds_dir, "sp")
    }

    #[test]
    fn missing_from_is_refused() {
        let d = kinds_tmp();
        let r = check(
            LintInput { from: "", subject: "Hello", kind: "", default: "", urgent: false, body: "body", digest: false },
            d.path(),
        );
        let e = r.unwrap_err();
        assert!(e.contains("rule"), "{e}");
    }

    #[test]
    fn present_from_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Sender <s@s>", subject: "Hello", kind: "", default: "", urgent: false, body: "body", digest: false },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn subject_that_is_a_bead_id_is_refused() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Gate <g@g>", subject: "sp-q0k3k", kind: "", default: "", urgent: false, body: "body", digest: false },
            d.path()
        )
        .is_err());
    }

    #[test]
    fn subject_leading_with_bead_id_and_colon_is_refused() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Gate <g@g>", subject: "sp-q0k3k: landed", kind: "", default: "", urgent: false, body: "body", digest: false },
            d.path()
        )
        .is_err());
    }

    #[test]
    fn human_topic_subject_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Gate <g@g>", subject: "Mail delivery landed", kind: "", default: "", urgent: false, body: "body", digest: false },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn sparse_bead_id_context_is_refused() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Gate <g@g>", subject: "Landed", kind: "", default: "", urgent: false, body: "Fixed sp-q0k3k.", digest: false },
            d.path()
        )
        .is_err());
    }

    #[test]
    fn sufficient_bead_id_context_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput {
                from: "Gate <g@g>",
                subject: "Summary",
                kind: "",
                default: "",
                urgent: false,
                body: "Implemented Maildir mail delivery with atomic send and lint in sp-q0k3k.",
                digest: false
            },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn key_value_metadata_line_with_bead_id_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Gate <g@g>", subject: "Result", kind: "", default: "", urgent: false, body: "target: sp-q0k3k", digest: false },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn bare_display_name_is_refused_with_no_address() {
        let d = kinds_tmp();
        let e = check(
            LintInput { from: "Archivist", subject: "Hello", kind: "", default: "", urgent: false, body: "body", digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("no address"), "{e}");
        assert!(e.contains("Archivist"), "{e}");
    }

    #[test]
    fn group_syntax_is_refused_with_rfc_6854() {
        let d = kinds_tmp();
        let e = check(
            LintInput { from: "Archivist:;", subject: "Hello", kind: "", default: "", urgent: false, body: "body", digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("RFC 6854"), "{e}");
    }

    #[test]
    fn unknown_kind_is_refused_naming_it() {
        let d = kinds_tmp();
        let e = check(
            LintInput { from: "Sender <s@s>", subject: "Hello", kind: "nosuchkind", default: "", urgent: false, body: "body", digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("nosuchkind"), "{e}");
    }

    #[test]
    fn decision_without_default_is_refused() {
        let d = kinds_tmp();
        let body = "## Decision\n\nApprove.\n\n## Default\n\nYes.\n\n## What is blocked\n\nNothing.\n\n## Cost of the wrong choice\n\nLow.\n";
        let e = check(
            LintInput { from: "Gate <g@g>", subject: "Enable feature?", kind: "decision", default: "", urgent: false, body, digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("default"), "{e}");
    }

    #[test]
    fn decision_with_default_is_accepted() {
        let d = kinds_tmp();
        let body = "## Decision\n\nApprove.\n\n## Default\n\nYes.\n\n## What is blocked\n\nNothing.\n\n## Cost of the wrong choice\n\nLow.\n";
        assert!(check(
            LintInput { from: "Gate <g@g>", subject: "Enable feature?", kind: "decision", default: "yes", urgent: false, body, digest: false },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn note_with_empty_section_is_refused_naming_section() {
        let d = kinds_tmp();
        let e = check(
            LintInput { from: "Sender <s@s>", subject: "Empty note", kind: "note", default: "", urgent: false, body: "## Note\n\n", digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("Note"), "{e}");
    }

    #[test]
    fn suit_with_empty_grounds_is_refused() {
        let d = kinds_tmp();
        let body = "## Suit\n\nI claim the service is down.\n\n## Grounds\n\n## Relief sought\n\nFix it.\n";
        assert!(check(
            LintInput { from: "Sender <s@s>", subject: "Filing suit", kind: "suit", default: "", urgent: false, body, digest: false },
            d.path()
        )
        .is_err());
    }

    #[test]
    fn urgent_without_why_section_is_refused() {
        let d = kinds_tmp();
        let e = check(
            LintInput { from: "Sender <s@s>", subject: "Urgent matter", kind: "note", default: "", urgent: true, body: "## Note\n\nContent.\n", digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("urgent") || e.contains("urgency"), "{e}");
    }

    #[test]
    fn urgent_with_why_section_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput {
                from: "Sender <s@s>",
                subject: "Urgent matter",
                kind: "note",
                default: "",
                urgent: true,
                body: "## Note\n\nContent.\n\n## Why it is urgent\n\nThe system is on fire.\n",
                digest: false
            },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn archivist_per_finding_note_is_refused_law_named() {
        let d = kinds_tmp();
        let e = check(
            LintInput { from: "Archivist <archivist@spira>", subject: "What I found", kind: "note", default: "", urgent: false, body: "## Note\n\nFound something.\n", digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("law-fail-closed-at-the-source"), "{e}");
    }

    #[test]
    fn archivist_question_is_unaffected_by_the_note_rule() {
        let d = kinds_tmp();
        assert!(check(
            LintInput {
                from: "Archivist <archivist@spira>",
                subject: "Not a note",
                kind: "question",
                default: "default",
                urgent: false,
                body: "## Question\n\nOK?\n\n## Default\n\nYes.\n",
                digest: false
            },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn non_archivist_sender_is_unaffected_by_the_note_rule() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Sender <s@s>", subject: "What I found", kind: "note", default: "", urgent: false, body: "## Note\n\nFound something.\n", digest: false },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn archivist_note_with_digest_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Archivist <archivist@spira>", subject: "Today's digest", kind: "note", default: "", urgent: false, body: "## Note\n\nFound something.\n", digest: true },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn ask_below_promise_with_no_section_is_refused() {
        let d = kinds_tmp();
        assert!(check(
            LintInput {
                from: "Sentinel <sentinel@spira>",
                subject: "Some event",
                kind: "",
                default: "",
                urgent: false,
                body: "not retried until a human changes the approach; the ask below carries the failure\n",
                digest: false
            },
            d.path()
        )
        .is_err());
    }

    #[test]
    fn question_below_phrase_with_no_section_is_also_refused() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Sender <s@s>", subject: "An event", kind: "", default: "", urgent: false, body: "See the question below for details.\n", digest: false },
            d.path()
        )
        .is_err());
    }

    #[test]
    fn ask_below_promise_with_a_question_section_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput {
                from: "Sentinel <sentinel@spira>",
                subject: "Some event",
                kind: "",
                default: "",
                urgent: false,
                body: "the ask below carries the failure\n\n## Question\n\nChange the approach?\n",
                digest: false
            },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn body_with_no_promise_phrase_is_accepted() {
        let d = kinds_tmp();
        assert!(check(
            LintInput { from: "Sender <s@s>", subject: "An event", kind: "", default: "", urgent: false, body: "The bead was poisoned.\n", digest: false },
            d.path()
        )
        .is_ok());
    }

    #[test]
    fn editing_a_kind_file_changes_acceptance_with_no_code_change() {
        let d = testkit::TempDir::new("mail-lint-custom-kind");
        fs::write(d.path().join("custom.md"), "---\n---\n\n## Body\n").unwrap();
        let body = "## Body\n\nHello.\n";
        assert!(check(
            LintInput { from: "Sender <s@s>", subject: "Custom", kind: "custom", default: "", urgent: false, body, digest: false },
            d.path()
        )
        .is_ok());

        fs::write(d.path().join("custom.md"), "---\nrequires: X-Spira-Default\n---\n\n## Body\n").unwrap();
        let e = check(
            LintInput { from: "Sender <s@s>", subject: "Custom", kind: "custom", default: "", urgent: false, body, digest: false },
            d.path(),
        )
        .unwrap_err();
        assert!(e.contains("default"), "{e}");
    }
}
