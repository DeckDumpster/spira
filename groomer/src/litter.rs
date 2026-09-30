//! The litter / described-unmapped-repo predicate `sweep`'s unmapped-repo branch decides
//! on: a bead with no description is litter (closeable mechanically); one with a
//! description is left for the model pass. Replaces `groomer-litter-predicate.py` — this
//! is pure JSON-in, judgement-out, so it is ported as a function rather than a subprocess.
//!
//! A malformed or unreadable payload predicates `has_content = false` (closeable is the
//! fail-open direction only in the sense that `sweep` already requires unmapped-repo AND
//! no description; a payload this could not even parse is treated the same as "no
//! description", matching the Python this replaces) and an empty `meta`.

/// `bd show <id> --json`'s payload, judged. `has_content`: the bead carries a non-blank
/// description. `meta`: a one-line `created_at: …, assignee: …` for the log line, with
/// both fields sanitised so a value that could break a shell word or a log line cannot.
pub struct Verdict {
    pub has_content: bool,
    pub meta: String,
}

/// Keep alphanumerics and the listed extra characters; replace everything else
/// (including any that would need shell-quoting) with `_`. Mirrors the Python's
/// `re.sub(r"[^A-Za-z0-9:.T_-]", "_", …)` / `re.sub(r"[^A-Za-z0-9._@-]", "_", …)`.
fn sanitize(s: &str, extra: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || extra.contains(c) { c } else { '_' })
        .collect()
}

fn str_field<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

/// `bd show --json` prints an array for `show` (one element) in every version this crate
/// has seen; accept a bare object too, since that is the more natural shape to hand this
/// function directly in a test or a future caller.
fn first_bead(v: &serde_json::Value) -> Option<&serde_json::Value> {
    match v {
        serde_json::Value::Array(a) => a.first(),
        serde_json::Value::Object(_) => Some(v),
        _ => None,
    }
}

/// Judge one `bd show --json` payload, already parsed. `raw` is kept for the caller that
/// has a byte string and wants the same fail-open behaviour on malformed JSON as
/// [`judge_text`] gives a caller with text.
pub fn judge(v: &serde_json::Value) -> Verdict {
    let Some(bead) = first_bead(v) else {
        return Verdict { has_content: false, meta: String::new() };
    };
    let has_content = str_field(bead, "description").map(|d| !d.trim().is_empty()).unwrap_or(false);
    let created_at = str_field(bead, "created_at").filter(|s| !s.is_empty()).unwrap_or("unknown");
    let assignee = str_field(bead, "assignee").filter(|s| !s.is_empty()).unwrap_or("unknown");
    let meta = format!(
        "created_at: {}, assignee: {}",
        sanitize(created_at, ":.T_-"),
        sanitize(assignee, "._@-"),
    );
    Verdict { has_content, meta }
}

/// Judge raw, possibly-unparseable stdin text (the CLI's entry point — `bd show --json`
/// piped in, same as the Python read `sys.stdin`).
pub fn judge_text(raw: &str) -> Verdict {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => judge(&v),
        Err(_) => Verdict { has_content: false, meta: String::new() },
    }
}

#[cfg(test)]
mod litter_tests {
    use super::*;

    #[test]
    fn described_bead_has_content() {
        let v = judge_text(r#"[{"description": "fix the thing", "created_at": "2026-09-01T00:00:00Z", "assignee": "ryan"}]"#);
        assert!(v.has_content);
        assert_eq!(v.meta, "created_at: 2026-09-01T00:00:00Z, assignee: ryan");
    }

    #[test]
    fn blank_description_is_not_content() {
        let v = judge_text(r#"[{"description": "   "}]"#);
        assert!(!v.has_content);
    }

    #[test]
    fn missing_description_is_not_content() {
        let v = judge_text(r#"[{}]"#);
        assert!(!v.has_content);
        assert_eq!(v.meta, "created_at: unknown, assignee: unknown");
    }

    #[test]
    fn bare_object_is_accepted_like_a_one_element_array() {
        let v = judge_text(r#"{"description": "x"}"#);
        assert!(v.has_content);
    }

    #[test]
    fn malformed_json_fails_open_to_no_content_and_empty_meta() {
        let v = judge_text("not json");
        assert!(!v.has_content);
        assert_eq!(v.meta, "");
    }

    #[test]
    fn empty_array_fails_open() {
        let v = judge_text("[]");
        assert!(!v.has_content);
        assert_eq!(v.meta, "");
    }

    #[test]
    fn dangerous_characters_in_metadata_are_sanitised() {
        let v = judge_text(r#"[{"created_at": "2026-09-01; rm -rf /", "assignee": "a$(b)"}]"#);
        assert!(!v.meta.contains(';'));
        assert!(!v.meta.contains('$'));
        assert!(!v.meta.contains('('));
        assert!(v.meta.starts_with("created_at: 2026-09-01"));
    }

    #[test]
    fn allowed_punctuation_survives_sanitising() {
        let v = judge_text(r#"[{"created_at": "2026-09-01T00:00:00Z", "assignee": "a.b@c-d_e"}]"#);
        assert_eq!(v.meta, "created_at: 2026-09-01T00:00:00Z, assignee: a.b@c-d_e");
    }
}
