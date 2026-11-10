//! A carry body as keyed rows: [`split`] writes the irreducible parts of a JSON carry, [`join`]
//! reads them back. What a [`Part`] does not name is not kept, and its serde default stands in
//! for it after a boot until the first tick rebuilds it.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// The longest row key; the store's column holds 255.
const KEY_MAX: usize = 200;

#[derive(Clone, Copy, Debug)]
pub enum Part {
    /// A top-level field, one row.
    Field(&'static str),
    /// A top-level object field, one row per entry, each entry kept without the fields in `drop`.
    Map { name: &'static str, drop: &'static [&'static str] },
}

/// A stable digest of `s`, for a row key that must not carry the text itself.
pub fn digest(s: &str) -> String {
    format!("{:016x}{:016x}", crate::event::fnv1a(s), crate::event::fnv1a(&format!("\u{0}{s}")))
}

/// The row key for `name`'s entry `key`: the key itself when it fits, else its digest.
pub fn entry_key(name: &str, key: &str) -> String {
    if key.len() <= KEY_MAX {
        format!("{name}/{key}")
    } else {
        format!("{name}/#{}", digest(key))
    }
}

pub fn split(body: &str, parts: &[Part]) -> BTreeMap<String, String> {
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(body) else { return BTreeMap::new() };
    let mut rows = BTreeMap::new();
    for part in parts {
        match *part {
            Part::Field(name) => {
                if let Some(v) = obj.get(name) {
                    rows.insert(name.to_string(), v.to_string());
                }
            }
            Part::Map { name, drop } => {
                let Some(Value::Object(entries)) = obj.get(name) else { continue };
                for (k, v) in entries {
                    let mut v = v.clone();
                    if let Value::Object(o) = &mut v {
                        drop.iter().for_each(|d| {
                            o.remove(*d);
                        });
                    }
                    rows.insert(entry_key(name, k), serde_json::json!({ "key": k, "value": v }).to_string());
                }
            }
        }
    }
    rows
}

pub fn join(rows: &BTreeMap<String, String>, parts: &[Part]) -> String {
    let mut obj = Map::new();
    for part in parts {
        match *part {
            Part::Field(name) => {
                if let Some(v) = rows.get(name).and_then(|v| serde_json::from_str(v).ok()) {
                    obj.insert(name.to_string(), v);
                }
            }
            Part::Map { name, .. } => {
                let prefix = format!("{name}/");
                let mut entries = Map::new();
                for (_, v) in rows.range(prefix.clone()..).take_while(|(k, _)| k.starts_with(&prefix)) {
                    if let Ok(Value::Object(mut e)) = serde_json::from_str::<Value>(v) {
                        if let (Some(Value::String(k)), Some(v)) = (e.remove("key"), e.remove("value")) {
                            entries.insert(k, v);
                        }
                    }
                }
                obj.insert(name.to_string(), Value::Object(entries));
            }
        }
    }
    Value::Object(obj).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTS: &[Part] = &[Part::Field("n"), Part::Map { name: "repos", drop: &["cache"] }];

    #[test]
    fn a_split_carry_joins_back_without_what_was_dropped() {
        let body = r#"{"n":3,"repos":{"o/r":{"a":1,"cache":"x"},"p":{"a":2,"cache":"y"}}}"#;
        let rows = split(body, PARTS);
        assert_eq!(rows.keys().collect::<Vec<_>>(), ["n", "repos/o/r", "repos/p"]);
        assert_eq!(join(&rows, PARTS), r#"{"n":3,"repos":{"o/r":{"a":1},"p":{"a":2}}}"#);
    }

    #[test]
    fn a_long_entry_key_is_digested_and_still_joins() {
        let long = "k".repeat(300);
        let body = format!(r#"{{"repos":{{"{long}":{{"a":1}}}}}}"#);
        let rows = split(&body, PARTS);
        assert!(rows.keys().all(|k| k.len() < 255), "{rows:?}");
        assert_eq!(join(&rows, PARTS), body);
    }
}
