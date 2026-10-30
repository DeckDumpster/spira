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

/// A retirement record: who took a SOP off the shelf, when, why, and the text it held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retired {
    pub by: String,
    pub at: String,
    pub epoch: u64,
    pub reason: String,
    pub text: String,
}

impl Retired {
    pub fn to_json(&self) -> String {
        serde_json::json!({"by": self.by, "at": self.at, "epoch": self.epoch, "reason": self.reason, "text": self.text}).to_string()
    }

    fn from_json(raw: &str) -> Option<Retired> {
        let v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        Some(Retired { by: s("by")?, at: s("at")?, epoch: v.get("epoch")?.as_u64()?, reason: s("reason")?, text: s("text")? })
    }
}

/// The memory key holding the retirement record of `key`. Not `sop-` prefixed, so `parse`
/// can never read a record as a live SOP.
pub fn retired_key(key: &str) -> String {
    format!("retired-{}", slugify(key))
}

/// Retirement records by SOP key (`sop-<slug>`); `None` when `raw` is unreadable, as `parse`.
pub fn parse_retired(raw: &str) -> Option<BTreeMap<String, Retired>> {
    if raw.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = v.as_object()?;
    Some(
        obj.iter()
            .filter_map(|(k, v)| {
                let sop = k.strip_prefix("retired-sop-")?;
                Some((format!("sop-{sop}"), Retired::from_json(v.as_str()?)?))
            })
            .collect(),
    )
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
    fn a_retirement_record_is_never_a_live_sop() {
        let r = Retired { by: "a".into(), at: "t".into(), epoch: 1, reason: "why".into(), text: "SYMPTOM: s".into() };
        let raw = serde_json::json!({ retired_key("x"): r.to_json(), "sop-y": "live" }).to_string();
        assert_eq!(retired_key("sop-x"), "retired-sop-x");
        assert_eq!(parse(&raw).unwrap().keys().collect::<Vec<_>>(), vec!["sop-y"]);
        assert_eq!(parse_retired(&raw).unwrap()["sop-x"], r);
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
