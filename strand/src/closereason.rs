//! The statute-phrase predicates over a close reason, shared by the close-time fence (dwarf)
//! and the invalid-close detector so the two cannot disagree about what an unfinished close
//! is (law-no-close-reason-admits-unfinished).
//!
//! A match inside a quoted span ("…", '…', `…`) or a parenthetical, or on a line that names
//! one of the mention files, is a mention rather than an admission — for the fence only;
//! the detector scans raw so every occurrence reaches Mimir
//! (law-a-pattern-match-is-not-an-identity-check).

use std::sync::OnceLock;

use regex::Regex;

const MENTION_FILES: [&str; 2] = ["close-reason-flags", "test-ops-closing.sh"];

const FOLLOW_ON: [&str; 4] = ["builders should", "at scale", "the real fix", "follow-up"];

fn red_flags() -> &'static [Regex] {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    R.get_or_init(|| {
        [
            r"(?i)PERMANENT FIX NEEDED",
            r"(?i)\bmitigated[- ]only\b",
            r"\bTODO\b",
            r"(?i)\btemporar(?:y|ily)\s+(?:fix|workaround|mitigation|patch|hack|solution)",
            r"(?i)\btemporarily\s+(?:fixed|mitigated|patched|worked around)",
        ]
        .iter()
        .map(|p| Regex::new(p).unwrap())
        .collect()
    })
}

fn mask_mentions(line: &str) -> String {
    static R: OnceLock<Vec<Regex>> = OnceLock::new();
    let spans = R.get_or_init(|| [r#""[^"]*""#, r"'[^']*'", r"`[^`]*`", r"\([^)]*\)"].iter().map(|p| Regex::new(p).unwrap()).collect());
    let mut masked = line.to_string();
    for re in spans {
        masked = re.replace_all(&masked, |m: &regex::Captures| " ".repeat(m[0].len())).into_owned();
    }
    masked
}

fn lines_of(reason: &str) -> Vec<&str> {
    let v: Vec<&str> = reason.lines().collect();
    if v.is_empty() {
        vec![reason]
    } else {
        v
    }
}

/// The first admitted statute phrase, quote-masked and mention-aware (the fence's own view).
pub fn check_close_reason(reason: &str) -> Option<String> {
    for line in lines_of(reason) {
        if MENTION_FILES.iter().any(|f| line.contains(f)) {
            continue;
        }
        let masked = mask_mentions(line);
        for re in red_flags() {
            if let Some(m) = re.find(&masked) {
                return Some(line[m.start()..m.end()].to_string());
            }
        }
    }
    None
}

/// The first red-flag match in the raw reason: no masking, no mention-file skip.
pub fn detect_close_reason(reason: &str) -> Option<String> {
    red_flags().iter().find_map(|re| re.find(reason).map(|m| m.as_str().to_string()))
}

/// The first follow-on phrase when the reason cites no tracking reference — a bead id
/// (`<id_prefix>-<id>`), an `owner/repo#N` reference or an `https://` URL.
pub fn check_unfiled_follow(reason: &str, id_prefix: &str) -> Option<String> {
    let tracking = Regex::new(&format!(
        r"(?i)(?:\b{}-[a-z0-9]+|[A-Za-z0-9][A-Za-z0-9._-]*/[A-Za-z0-9][A-Za-z0-9._-]*#[0-9]+|https?://\S+)",
        regex::escape(id_prefix)
    ))
    .ok()?;
    let low = reason.to_lowercase();
    let hit = FOLLOW_ON.iter().find(|f| low.contains(&f.to_lowercase()))?;
    (!tracking.is_match(reason)).then(|| hit.to_string())
}
