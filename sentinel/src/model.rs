//! The data the sentinel reads (DESIGN.md §3), with serde. Every reader is lenient the way
//! the python it replaces was: unknown fields ignored, null or absent → default.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

fn nullable<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// A bead row from `bd list --json` / `bd show --json`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Bead {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub notes: Option<Value>,
    #[serde(default, deserialize_with = "nullable")]
    pub status: String,
    #[serde(default)]
    pub priority: Option<Value>,
    #[serde(default)]
    pub issue_type: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub labels: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub close_reason: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub dependency_count: u64,
    #[serde(default, deserialize_with = "nullable")]
    pub dependencies: Vec<Dep>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Dep {
    #[serde(default)]
    pub depends_on_id: Option<String>,
    /// `bd show`'s shape names the target `id`.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, alias = "dependency_type")]
    pub r#type: Option<String>,
    /// Present only in `bd show`'s dependency objects.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
}

impl Dep {
    pub fn target(&self) -> Option<&str> {
        self.depends_on_id.as_deref().or(self.id.as_deref())
    }
}

impl Bead {
    pub fn has(&self, l: &str) -> bool {
        self.labels.iter().any(|x| x == l)
    }
    pub fn label_value(&self, prefix: &str) -> Option<&str> {
        self.labels.iter().find_map(|l| l.strip_prefix(prefix))
    }
    pub fn typ(&self) -> &str {
        self.issue_type.as_deref().unwrap_or("")
    }
    pub fn labels_csv(&self) -> String {
        self.labels.join(",")
    }
}

/// `bd … --json` payloads can carry warnings before the JSON (lib.sh `json_only`): drop
/// every line before the first that starts with `[` or `{`.
pub fn json_only(s: &str) -> &str {
    let mut off = 0;
    for line in s.split_inclusive('\n') {
        if line.starts_with('[') || line.starts_with('{') {
            return &s[off..];
        }
        off += line.len();
    }
    ""
}

/// A list or a single object → rows. Err when it is not JSON at all.
pub fn parse_beads(s: &str) -> Result<Vec<Bead>, String> {
    let body = json_only(s);
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let items = match v {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        o => vec![o],
    };
    Ok(items
        .into_iter()
        .filter_map(|i| serde_json::from_value::<Bead>(i).ok())
        .collect())
}

/// One `spira-lc list` row. Dolt returns columns string-valued, so each field accepts a
/// string or its native JSON type; `holds` is a JSON array or a JSON-encoded string of one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LcRow {
    pub bead_id: String,
    pub state: String,
    pub holder: Option<String>,
    pub lease_until: Option<i64>,
    pub holds: Vec<String>,
    pub version: Option<String>,
}

fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        other => Some(other.to_string()),
    }
}

pub fn parse_holds(v: Option<&Value>) -> Vec<String> {
    let arr = match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) if !s.trim().is_empty() => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    arr.into_iter()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect()
}

pub fn parse_lc_rows(s: &str) -> Result<Vec<LcRow>, String> {
    let v: Value =
        serde_json::from_str(s.trim()).map_err(|e| format!("spira-lc list: not JSON: {e}"))?;
    let Value::Array(a) = v else {
        return Ok(Vec::new());
    };
    Ok(a.iter()
        .map(|r| LcRow {
            bead_id: scalar(r.get("bead_id")).unwrap_or_default(),
            state: scalar(r.get("state")).unwrap_or_default(),
            holder: scalar(r.get("holder")).filter(|h| !h.is_empty()),
            lease_until: scalar(r.get("lease_until"))
                .and_then(|x| x.trim().parse::<f64>().ok())
                .map(|f| f as i64),
            holds: parse_holds(r.get("holds")),
            version: scalar(r.get("version")),
        })
        .filter(|r| !r.bead_id.is_empty())
        .collect())
}

/// Python's `str()` of a JSON scalar, for the bead_context header ("P0", "PNone").
pub fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(o) => o.to_string(),
    }
}

/// `KEY=<digits or ->` lines of a status file (the old `sed -n … | eval`).
pub fn status_fields(text: &str, prefix: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            let ok_key =
                k.starts_with(prefix) && k[prefix.len()..].chars().all(|c| c.is_ascii_uppercase());
            let ok_val = v.chars().all(|c| c.is_ascii_digit() || c == '-');
            (ok_key && ok_val).then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

/// A landstate record: `<STATE> <tip> …`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandState {
    pub state: String,
    pub tip: String,
}

impl LandState {
    pub fn parse(text: &str) -> LandState {
        let mut w = text.lines().next().unwrap_or("").split_whitespace();
        LandState {
            state: w.next().unwrap_or("").into(),
            tip: w.next().unwrap_or("").into(),
        }
    }
    /// LANDED with a real tip — the only record that proves anything.
    pub fn landed_tip(&self) -> Option<&str> {
        (self.state == "LANDED" && !self.tip.is_empty() && self.tip != "none")
            .then_some(self.tip.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beads_tolerate_nulls_warnings_and_single_objects() {
        let s = "warning: something\n[{\"id\":\"a\",\"labels\":null,\"status\":\"open\",\"dependencies\":null},{\"id\":\"b\",\"dependencies\":[{\"issue_id\":\"b\",\"depends_on_id\":\"a\",\"type\":\"blocks\"}]}]";
        let v = parse_beads(s).unwrap();
        assert_eq!(v.len(), 2);
        assert!(v[0].labels.is_empty());
        assert_eq!(v[1].dependencies[0].target(), Some("a"));
        assert_eq!(v[1].dependencies[0].r#type.as_deref(), Some("blocks"));
        let one = parse_beads(
            "{\"id\":\"x\",\"dependencies\":[{\"id\":\"y\",\"dependency_type\":\"supersedes\"}]}",
        )
        .unwrap();
        assert_eq!(one[0].dependencies[0].target(), Some("y"));
        assert_eq!(one[0].dependencies[0].r#type.as_deref(), Some("supersedes"));
        assert!(parse_beads("nope").is_err());
        assert!(parse_beads("").is_err());
    }

    #[test]
    fn lc_rows_accept_strings_and_encoded_holds() {
        let s = r#"[{"bead_id":"a","state":"WORKING","holder":"","lease_until":"1700","holds":"[\"wait\"]","version":"4"},
                    {"bead_id":"b","state":"READY","holder":"x","lease_until":null,"holds":["poison"],"version":7}]"#;
        let r = parse_lc_rows(s).unwrap();
        assert_eq!(r[0].holder, None);
        assert_eq!(r[0].lease_until, Some(1700));
        assert_eq!(r[0].holds, vec!["wait"]);
        assert_eq!(r[1].version.as_deref(), Some("7"));
        assert_eq!(r[1].holds, vec!["poison"]);
        assert!(parse_lc_rows("cannot tell").is_err());
    }

    #[test]
    fn status_fields_only_take_digit_values() {
        let f = status_fields(
            "SP_LAND_AT=100\nSP_LAND_RC=2\nSP_LAND_X=abc\nOTHER=1\nSP_LAND_moved=3\n",
            "SP_LAND_",
        );
        assert_eq!(
            f,
            vec![
                ("SP_LAND_AT".into(), "100".into()),
                ("SP_LAND_RC".into(), "2".into())
            ]
        );
    }

    #[test]
    fn landstate_proves_only_a_landed_tip() {
        assert_eq!(
            LandState::parse("LANDED abc123 x\n").landed_tip(),
            Some("abc123")
        );
        assert_eq!(LandState::parse("LANDED none").landed_tip(), None);
        assert_eq!(LandState::parse("CERTIFIED abc").landed_tip(), None);
    }
}
