// tsd — pure core: the row format shared by every run/tsd/ file family.
//
// A row is a JSON object: the envelope (ts, host, family — the producer's own clock stamp
// and host id, law-producers-stamp-their-own-clock) merged with whatever fields the producer
// declares (law-producers-declare-what-they-know). No engine reads or writes here — this
// crate only formats and validates the line; the IO seam (tsd-write's main.rs) appends it.

use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

const ENVELOPE_KEYS: [&str; 3] = ["ts", "host", "family"];

/// A family name is the file a producer's rows live in: lowercase, digits, hyphens,
/// starting with a letter. It becomes a path component, so anything else is refused rather
/// than sanitized — a sanitized `../../etc` is still a traversal, just a quieter one.
pub fn valid_family(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Where a family's rows are appended: <root>/tsd/<family>.jsonl.
pub fn family_path(root: &Path, family: &str) -> PathBuf {
    root.join("tsd").join(format!("{family}.jsonl"))
}

/// Parse one `key=value` CLI argument, JSON-sniffing the value: `duration_ms=1500` becomes
/// a number, `ok=true` a bool, anything else a string. For a value a caller wants kept as a
/// string regardless of shape (a git SHA that happens to be all digits, an id), see
/// [`parse_field_str`].
pub fn parse_field(raw: &str) -> Result<(String, Value), String> {
    let (key, val) = split_field(raw)?;
    let value = serde_json::from_str::<Value>(val).unwrap_or_else(|_| Value::String(val.to_string()));
    Ok((key, value))
}

/// Parse one `key=value` CLI argument, always as a string — no JSON-sniffing.
pub fn parse_field_str(raw: &str) -> Result<(String, Value), String> {
    let (key, val) = split_field(raw)?;
    Ok((key, Value::String(val.to_string())))
}

fn split_field(raw: &str) -> Result<(String, &str), String> {
    let (key, val) = raw
        .split_once('=')
        .ok_or_else(|| format!("field {raw:?} is not key=value"))?;
    if key.is_empty() {
        return Err(format!("field {raw:?} has an empty key"));
    }
    Ok((key.to_string(), val))
}

/// Build one JSONL row (no trailing newline): the envelope merged with the producer's
/// fields. A producer field that collides with an envelope key is refused — ts and host are
/// the producer's own stamp, not a value it can also pass as a field.
pub fn build_row(ts: &str, host: &str, family: &str, fields: &[(String, Value)]) -> Result<String, String> {
    for (k, _) in fields {
        if ENVELOPE_KEYS.contains(&k.as_str()) {
            return Err(format!(
                "field {k:?} collides with the envelope — pass it as --ts/--host, not --field"
            ));
        }
    }
    let mut row = Map::new();
    row.insert("ts".to_string(), Value::String(ts.to_string()));
    row.insert("host".to_string(), Value::String(host.to_string()));
    row.insert("family".to_string(), Value::String(family.to_string()));
    for (k, v) in fields {
        row.insert(k.clone(), v.clone());
    }
    serde_json::to_string(&Value::Object(row)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_family_accepts_lowercase_hyphenated_names() {
        assert!(valid_family("suite-timing"));
        assert!(valid_family("landing-event"));
        assert!(valid_family("a1"));
    }

    #[test]
    fn valid_family_refuses_anything_that_could_escape_the_directory() {
        assert!(!valid_family(""));
        assert!(!valid_family("Suite"));
        assert!(!valid_family("-leading"));
        assert!(!valid_family("has space"));
        assert!(!valid_family("../etc"));
        assert!(!valid_family("a/b"));
        assert!(!valid_family("a.b"));
    }

    #[test]
    fn family_path_nests_under_tsd() {
        let p = family_path(Path::new("/run"), "suite-timing");
        assert_eq!(p, PathBuf::from("/run/tsd/suite-timing.jsonl"));
    }

    #[test]
    fn parse_field_json_sniffs_scalars() {
        assert_eq!(parse_field("duration_ms=1500").unwrap().1, Value::from(1500));
        assert_eq!(parse_field("ok=true").unwrap().1, Value::from(true));
        assert_eq!(
            parse_field("suite=test-foo.sh").unwrap().1,
            Value::from("test-foo.sh")
        );
    }

    #[test]
    fn parse_field_str_never_sniffs() {
        // A SHA that happens to be all digits must not silently become a number.
        assert_eq!(
            parse_field_str("tip=00000000000000000000000000000000000042")
                .unwrap()
                .1,
            Value::from("00000000000000000000000000000000000042")
        );
    }

    #[test]
    fn parse_field_refuses_missing_equals() {
        assert!(parse_field("no-equals-here").is_err());
    }

    #[test]
    fn parse_field_refuses_empty_key() {
        assert!(parse_field("=value").is_err());
    }

    #[test]
    fn build_row_merges_envelope_and_fields() {
        let fields = vec![
            ("suite".to_string(), Value::from("test-foo.sh")),
            ("duration_ms".to_string(), Value::from(1500)),
        ];
        let line = build_row("2026-09-25T00:00:00Z", "h1", "suite-timing", &fields).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["ts"], "2026-09-25T00:00:00Z");
        assert_eq!(v["host"], "h1");
        assert_eq!(v["family"], "suite-timing");
        assert_eq!(v["suite"], "test-foo.sh");
        assert_eq!(v["duration_ms"], 1500);
    }

    #[test]
    fn build_row_refuses_field_colliding_with_envelope() {
        for key in ENVELOPE_KEYS {
            let fields = vec![(key.to_string(), Value::from("spoofed"))];
            assert!(
                build_row("t", "h1", "fam", &fields).is_err(),
                "expected a collision refusal for {key}"
            );
        }
    }
}
