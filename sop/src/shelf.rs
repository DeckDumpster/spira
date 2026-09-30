//! The shelf: every `sop-*` key on `bdjson memories`, as a string map. One parse, shared by
//! every subcommand that reads it, so each pays the same cost and reads the same rules.

use std::collections::BTreeMap;

/// `None` when `raw` is empty/whitespace, does not parse as JSON, or does not parse as an
/// object — the "unreadable" case every caller must treat as distinct from a genuinely
/// empty shelf (`Some(empty map)`), per `bd memories --json` printing nothing on a failed
/// query and `{}` on an honestly empty one.
pub fn parse(raw: &str) -> Option<BTreeMap<String, String>> {
    if raw.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = v.as_object()?;
    Some(
        obj.iter()
            .filter_map(|(k, v)| {
                if !k.starts_with("sop-") {
                    return None;
                }
                v.as_str().map(|s| (k.clone(), s.trim().to_string()))
            })
            .collect(),
    )
}

/// `list`/`match`'s reading of the shelf: any read or parse failure is silently treated as
/// an EMPTY shelf, not an unreadable one — matching the bash's `shelf()` (as opposed to
/// `digest`/`lint`/`applied`'s stricter `_sop_shelf_raw`, which this module's `parse`
/// mirrors). Low-stakes, informational commands only; nothing here can lose data by
/// misreading "broken" as "empty".
pub fn parse_or_empty(raw: &str) -> BTreeMap<String, String> {
    parse(raw).unwrap_or_default()
}

pub fn slugify(s: &str) -> String {
    format!("sop-{}", s.strip_prefix("sop-").unwrap_or(s))
}

/// The `SYMPTOM:` line's value, for `list`'s one-line-per-SOP summary.
pub fn symptom_of(text: &str) -> String {
    let re = regex::Regex::new(r"(?m)^\s*SYMPTOM:\s*(.+)$").expect("static");
    re.captures(text).map(|c| c[1].trim().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_raw_is_unreadable() {
        assert!(parse("").is_none());
        assert!(parse("   ").is_none());
    }

    #[test]
    fn unparseable_raw_is_unreadable() {
        assert!(parse("not json").is_none());
    }

    #[test]
    fn a_non_object_is_unreadable() {
        assert!(parse("[1,2,3]").is_none());
    }

    #[test]
    fn an_empty_object_is_a_real_empty_shelf() {
        assert_eq!(parse("{}"), Some(BTreeMap::new()));
    }

    #[test]
    fn only_sop_prefixed_string_keys_survive_and_are_stripped() {
        let raw = r#"{"sop-a": "  text  ", "law-b": "not a sop", "sop-c": 5}"#;
        let m = parse(raw).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m["sop-a"], "text");
    }

    #[test]
    fn slugify_adds_or_keeps_prefix() {
        assert_eq!(slugify("disk-full"), "sop-disk-full");
        assert_eq!(slugify("sop-disk-full"), "sop-disk-full");
    }

    #[test]
    fn symptom_of_extracts_the_line() {
        assert_eq!(symptom_of("SYMPTOM: disk is full\nCHECK: df\n"), "disk is full");
        assert_eq!(symptom_of("CHECK: df\n"), "");
    }
}
