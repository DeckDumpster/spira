//! Pure logic behind the `rule` binary. Contract: DESIGN.md. Kept separate from `main.rs`
//! (which does the `bd`/hook subprocess work) so it is unit-testable with no database, no
//! filesystem and no subprocess.

pub mod audit;
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

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The key of a statute in force, other than `key`, whose text equals `text` up to whitespace.
pub fn find_duplicate(
    memories: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
    text: &str,
) -> Option<String> {
    let want = normalized(text);
    memories
        .iter()
        .find(|(k, v)| k.as_str() != key && v.as_str().is_some_and(|t| normalized(t) == want))
        .map(|(k, _)| k.clone())
}

/// Whether the book holds `text` under `key` — the only proof a write that reported an error landed.
pub fn is_in_force(
    memories: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
    text: &str,
) -> bool {
    memories
        .get(key)
        .and_then(|v| v.as_str())
        .is_some_and(|t| normalized(t) == normalized(text))
}

/// The result of parsing `enact`'s trailing arguments (everything after `<slug>`).
#[derive(Debug, PartialEq, Eq)]
pub struct EnactArgs {
    pub dry_run: bool,
    pub text: String,
    pub tool: ToolDecl,
}

/// What performs the statute: a tool that exists now, a bead building it, or nothing because
/// the statute only constrains judgement (with the reason).
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ToolDecl {
    Tool(String),
    Mechanism(String),
    None(String),
}

fn is_bead_id(s: &str) -> bool {
    let mut parts = s.splitn(2, '-');
    let (p, rest) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    !p.is_empty()
        && !rest.is_empty()
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}

const TOOL_REFUSAL: &str = "rule: refusing — a statute must name what can comply with it. Pass --tool \"<command or component that performs it>\", or --mechanism <bead-id> (the bead filed to build it). A statute that only constrains judgement passes --tool none --reason \"<why no action is prescribed>\".";

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
    let (mut tool, mut mechanism, mut reason): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if matches!(arg.as_str(), "--tool" | "--mechanism" | "--reason") {
            let Some(v) = it.next().filter(|v| !v.starts_with("--")) else {
                return Err(EnactError::Message(format!("rule: enact: {arg} needs a value")));
            };
            let slot = match arg.as_str() {
                "--tool" => &mut tool,
                "--mechanism" => &mut mechanism,
                _ => &mut reason,
            };
            if slot.replace(v.clone()).is_some() {
                return Err(EnactError::Message(format!("rule: enact: {arg} given twice")));
            }
        } else if arg == "--dry-run" {
            dry_run = true;
        } else if arg.starts_with('-') {
            return Err(EnactError::Message(format!(
                "rule: enact does not take '{arg}' — usage: rule.sh enact <slug> \"<text>\" --tool <cmd>|--mechanism <bead>|--tool none --reason <why> [--dry-run]"
            )));
        } else if text.is_some() {
            return Err(EnactError::Message(format!(
                "rule: enact takes one quoted text argument; '{arg}' is a second one — quote the whole statute text"
            )));
        } else {
            text = Some(arg.clone());
        }
    }
    let Some(text) = text else {
        return Err(EnactError::Usage);
    };
    let refuse = |m: &str| Err(EnactError::Message(m.to_string()));
    let tool = match (tool, mechanism) {
        (None, None) => return refuse(TOOL_REFUSAL),
        (Some(_), Some(_)) => return refuse("rule: enact takes --tool or --mechanism, not both"),
        (Some(t), None) if t.trim().is_empty() => return refuse(TOOL_REFUSAL),
        (Some(t), None) if t == "none" => match reason.as_deref().map(str::trim) {
            Some(r) if !r.is_empty() => ToolDecl::None(r.to_string()),
            _ => return refuse("rule: --tool none needs --reason \"<why no action is prescribed>\""),
        },
        (Some(t), None) => ToolDecl::Tool(t),
        (None, Some(m)) if is_bead_id(&m) => ToolDecl::Mechanism(m),
        (None, Some(m)) => return Err(EnactError::Message(format!("rule: --mechanism '{m}' is not a bead id"))),
    };
    if reason.is_some() && !matches!(tool, ToolDecl::None(_)) {
        return refuse("rule: --reason belongs to --tool none");
    }
    Ok(EnactArgs { dry_run, text, tool })
}

/// The three-line refusal for a statute over the word budget (unchanged text from `rule.sh`).
pub fn word_limit_refusal(words: usize) -> String {
    format!(
        "rule: refusing — {words} words. A statute is one paragraph (~70 words);\n      every agent pays this context on every session. Put the case history\n      in the wiki and keep the scar here as a single clause."
    )
}

/// `rule.sh`'s own doc-comment usage block (`sed -n '3,10p' "$0" | sed 's/^# \{0,1\}//'`),
/// pinned verbatim since this binary has no bash header comment of its own to read.
pub const USAGE: &str = "rule.sh — enact, amend, or retire a statute in one command.\n\n  rule.sh enact <slug> \"<statute text>\" (--tool <cmd> | --mechanism <bead> | --tool none --reason <why>) [--dry-run]\n                                           write it, then synthesise it into the wiki\n  rule.sh audit [--file --for <persona> --repo <name> --max N]\n                                           check every statute's named tools exist in the release\n  rule.sh retire <slug>                    remove it\n  rule.sh list                             what is in force\n  rule.sh show <slug>                      one statute's full text\n\n`<slug>` is written without the `law-` prefix; it is added for you.";

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
                format!("rule: {key} is in force, wiki sync failed — wiki/notes/common-law.md was not committed; commit it manually.")
            }
        }
    }
}


/// The on-disk statute mirror: one `<key>.txt` per statute, whose whole content is the text.
/// Every read of the book goes here so a slow or stopped store cannot make law unreadable.
pub mod mirror {
    use std::collections::BTreeMap;
    use std::path::Path;

    fn storable(key: &str) -> bool {
        !key.is_empty() && !key.starts_with('.') && !key.contains('/')
    }

    /// Makes `dir` hold exactly `book`: each statute written atomically, any other `.txt` removed.
    pub fn sync(dir: &Path, book: &BTreeMap<String, serde_json::Value>) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        for (key, v) in book {
            let (Some(text), true) = (v.as_str(), storable(key)) else { continue };
            let tmp = dir.join(format!(".tmp-{key}-{}", std::process::id()));
            std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
            std::fs::rename(&tmp, dir.join(format!("{key}.txt")))
                .map_err(|e| format!("cannot place {key}: {e}"))?;
        }
        for entry in std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".txt") else { continue };
            if !book.get(key).is_some_and(|v| v.is_string()) {
                std::fs::remove_file(entry.path()).map_err(|e| format!("cannot remove {name}: {e}"))?;
            }
        }
        Ok(())
    }

    /// The mirrored book, or `None` when `dir` holds no statute (never mirrored, or unreadable).
    pub fn read(dir: &Path) -> Option<BTreeMap<String, serde_json::Value>> {
        let mut book = BTreeMap::new();
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".txt") else { continue };
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                book.insert(key.to_string(), serde_json::Value::String(text));
            }
        }
        (!book.is_empty()).then_some(book)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn duplicate_text_under_another_key_is_named() {
        let m: std::collections::BTreeMap<_, _> = [
            ("law-a".to_string(), json!("same  text")),
            ("law-b".to_string(), json!("other")),
        ]
        .into();
        assert_eq!(find_duplicate(&m, "law-new", "same text"), Some("law-a".into()));
        assert_eq!(find_duplicate(&m, "law-a", "same text"), None);
        assert_eq!(find_duplicate(&m, "law-new", "novel"), None);
    }

    #[test]
    fn in_force_requires_the_text_under_the_key() {
        let m: std::collections::BTreeMap<_, _> = [("law-a".to_string(), json!("t x"))].into();
        assert!(is_in_force(&m, "law-a", "t  x"));
        assert!(!is_in_force(&m, "law-a", "other"));
        assert!(!is_in_force(&m, "law-b", "t x"));
    }

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

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn refusal(v: &[&str]) -> String {
        match parse_enact_args(&a(v)).unwrap_err() {
            EnactError::Message(m) => m,
            EnactError::Usage => panic!("expected a named message"),
        }
    }

    #[test]
    fn enact_without_tool_or_mechanism_refuses_naming_both() {
        let m = refusal(&["text"]);
        assert!(m.contains("--tool") && m.contains("--mechanism"), "{m}");
    }

    #[test]
    fn tool_none_needs_a_reason() {
        assert!(refusal(&["text", "--tool", "none"]).contains("--reason"));
        let ok = parse_enact_args(&a(&["text", "--tool", "none", "--reason", "judgement only"])).unwrap();
        assert_eq!(ok.tool, ToolDecl::None("judgement only".to_string()));
    }

    #[test]
    fn mechanism_must_be_a_bead_id_and_excludes_tool() {
        assert!(refusal(&["text", "--mechanism", "Not A Bead"]).contains("not a bead id"));
        assert!(refusal(&["text", "--mechanism", "sp-abc", "--tool", "x"]).contains("not both"));
        assert!(refusal(&["text", "--tool", "x", "--reason", "y"]).contains("--reason"));
    }

    #[test]
    fn parse_enact_args_happy_path() {
        let got = parse_enact_args(&a(&["some text", "--tool", "queue open-batch"])).unwrap();
        assert_eq!(
            got,
            EnactArgs {
                dry_run: false,
                text: "some text".to_string(),
                tool: ToolDecl::Tool("queue open-batch".to_string()),
            }
        );
    }

    #[test]
    fn parse_enact_args_dry_run_flag() {
        let got = parse_enact_args(&a(&["--dry-run", "some text", "--mechanism", "sp-abc12"])).unwrap();
        assert!(got.dry_run);
        assert_eq!(got.text, "some text");
    }

    #[test]
    fn parse_enact_args_rejects_unknown_flag() {
        let err = parse_enact_args(&a(&["--nope", "text"])).unwrap_err();
        match err {
            EnactError::Message(m) => assert!(m.contains("does not take '--nope'"), "{m}"),
            EnactError::Usage => panic!("expected a named message, got Usage"),
        }
    }

    #[test]
    fn parse_enact_args_rejects_second_positional() {
        let err = parse_enact_args(&a(&["first", "second"])).unwrap_err();
        match err {
            EnactError::Message(m) => assert!(m.contains("is a second one"), "{m}"),
            EnactError::Usage => panic!("expected a named message, got Usage"),
        }
    }

    #[test]
    fn parse_enact_args_requires_text() {
        let err = parse_enact_args(&a(&["--dry-run", "--tool", "x"])).unwrap_err();
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
            "rule: law-x is in force, wiki sync failed — wiki/notes/common-law.md was not committed; commit it manually."
        );
    }
}

#[cfg(test)]
mod mirror_tests {
    use super::mirror;
    use std::collections::BTreeMap;

    fn book(pairs: &[(&str, &str)]) -> BTreeMap<String, serde_json::Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string()))).collect()
    }

    #[test]
    fn a_synced_book_reads_back_and_an_unmirrored_dir_reads_as_none() {
        let t = testkit::TempDir::new("mirror-rt");
        let d = t.path().to_path_buf();
        assert!(mirror::read(&d).is_none());
        mirror::sync(&d, &book(&[("law-a", "Alpha."), ("law-b", "Beta.")])).unwrap();
        assert_eq!(mirror::read(&d).unwrap(), book(&[("law-a", "Alpha."), ("law-b", "Beta.")]));
    }

    #[test]
    fn sync_replaces_text_and_drops_a_retired_statute() {
        let t = testkit::TempDir::new("mirror-drop");
        let d = t.path().to_path_buf();
        mirror::sync(&d, &book(&[("law-a", "Alpha."), ("law-b", "Beta.")])).unwrap();
        mirror::sync(&d, &book(&[("law-a", "Alpha amended.")])).unwrap();
        assert_eq!(mirror::read(&d).unwrap(), book(&[("law-a", "Alpha amended.")]));
    }
}

