//! The records strand consumes and produces (DESIGN.md §3). Every field bd may omit or send
//! as `null` has a default, so a row missing a field reads as "absent", never as a parse
//! failure that would drop the whole store on the floor.

use serde::{Deserialize, Deserializer, Serialize};

/// `null` and absent both become the type's default — bd sends `"labels": null` for a bead
/// with no labels, and serde's own `default` covers only the absent case.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// A bead's status. Only `closed`, `in_progress` and `deferred` carry meaning here; every
/// other spelling is kept verbatim so a report can still name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Open,
    InProgress,
    Deferred,
    Closed,
    Other(String),
}

impl Default for Status {
    fn default() -> Self {
        Status::Other(String::new())
    }
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        Ok(Status::parse(s.as_deref().unwrap_or("")))
    }
}

impl Status {
    pub fn parse(s: &str) -> Status {
        match s {
            "open" => Status::Open,
            "in_progress" => Status::InProgress,
            "deferred" => Status::Deferred,
            "closed" => Status::Closed,
            other => Status::Other(other.to_string()),
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Status::Open => "open",
            Status::InProgress => "in_progress",
            Status::Deferred => "deferred",
            Status::Closed => "closed",
            Status::Other(s) => s,
        }
    }
}

/// One dependency edge. `bd list --json` spells it `{depends_on_id, type}`; `bd show --json`
/// embeds the target bead and spells the edge type `dependency_type`, with the target's id
/// in `id`. Both shapes are accepted.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Dependency {
    #[serde(default, deserialize_with = "null_default")]
    pub depends_on_id: Option<String>,
    #[serde(default, rename = "type", alias = "dependency_type", deserialize_with = "null_default")]
    pub dep_type: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub id: Option<String>,
}

impl Dependency {
    /// The target of a `blocks` edge, or None for every other edge type.
    pub fn blocks_target(&self) -> Option<&str> {
        if self.dep_type.as_deref() != Some("blocks") {
            return None;
        }
        self.depends_on_id.as_deref().or(self.id.as_deref()).filter(|s| !s.is_empty())
    }
}

/// Notes arrive as a string on these beads, and as a list of strings or `{text}` objects
/// from older stores. Normalised to lines.
#[derive(Debug, Clone, Default)]
pub struct Notes(pub Vec<String>);

impl<'de> Deserialize<'de> for Notes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let lines = match v {
            serde_json::Value::String(s) => {
                s.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect()
            }
            serde_json::Value::Array(a) => a
                .into_iter()
                .map(|x| match x {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Object(o) => o
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    other => other.to_string(),
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(Notes(lines))
    }
}

/// A bead, cut down to what the classifier and the escalation body read.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Bead {
    pub id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Status,
    #[serde(default, deserialize_with = "null_default")]
    pub issue_type: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub labels: Vec<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub parent: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub assignee: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub lease_expires_at: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub dependencies: Vec<Dependency>,
    // Escalation body only (bd show).
    #[serde(default, deserialize_with = "null_default")]
    pub priority: Option<i64>,
    #[serde(default, deserialize_with = "null_default")]
    pub created_at: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub description: Option<String>,
    #[serde(default)]
    pub notes: Option<Notes>,
}

impl Bead {
    pub fn has(&self, label: &str) -> bool {
        !label.is_empty() && self.labels.iter().any(|l| l == label)
    }
    pub fn is_closed(&self) -> bool {
        self.status == Status::Closed
    }
    pub fn is_epic(&self) -> bool {
        self.issue_type.as_deref() == Some("epic")
    }
    pub fn blocks_targets(&self) -> impl Iterator<Item = &str> {
        self.dependencies.iter().filter_map(Dependency::blocks_target)
    }
}

/// Parse a bd JSON payload: a list of beads, or one bead object. A payload with non-JSON
/// preamble (bd's own warnings on stdout) is read from its first `[` or `{` line, as
/// lib.sh's json_only did. `Err` means the payload is not a bead list — the caller must
/// treat that as "could not read", never as "empty".
pub fn parse_beads(raw: &str) -> Result<Vec<Bead>, String> {
    let start = raw
        .lines()
        .scan(0usize, |off, line| {
            let here = *off;
            *off += line.len() + 1;
            Some((here, line))
        })
        .find(|(_, l)| l.starts_with('[') || l.starts_with('{'))
        .map(|(o, _)| o);
    let Some(start) = start else {
        return if raw.trim().is_empty() {
            Err("empty reply".into())
        } else {
            Err("no JSON in reply".into())
        };
    };
    let v: serde_json::Value =
        serde_json::from_str(&raw[start..]).map_err(|e| format!("bad JSON: {e}"))?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Null => Vec::new(),
        obj @ serde_json::Value::Object(_) => vec![obj],
        other => return Err(format!("expected a bead list, got {other}")),
    };
    // One malformed row is skipped, not fatal: the rest of the store is still the truth
    // about the rest of the beads.
    Ok(items
        .into_iter()
        .filter_map(|x| serde_json::from_value::<Bead>(x).ok())
        .filter(|b| !b.id.is_empty())
        .collect())
}

/// How a row is handled by `check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Act,
    Escalate,
    Info,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::Act => "act",
            Disposition::Escalate => "escalate",
            Disposition::Info => "info",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "act" => Some(Disposition::Act),
            "escalate" => Some(Disposition::Escalate),
            "info" => Some(Disposition::Info),
            _ => None,
        }
    }
}

/// One classifier row: `kind id disposition detail action` (DESIGN.md §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: String,
    pub id: String,
    pub disp: Disposition,
    pub detail: String,
    pub action: String,
}

impl Row {
    pub fn new(kind: &str, id: &str, disp: Disposition, detail: String, action: String) -> Row {
        Row { kind: kind.into(), id: id.into(), disp, detail, action }
    }
    #[cfg_attr(not(test), allow(dead_code))] // the saved-TSV shape `check --from` reads
    pub fn to_tsv(&self) -> String {
        [
            self.kind.as_str(),
            self.id.as_str(),
            self.disp.as_str(),
            &self.detail.replace('\t', " "),
            &self.action.replace('\t', " "),
        ]
        .join("\t")
    }
    /// Parse one saved TSV line (`check --from`). Blank lines and lines with fewer than five
    /// fields or an unknown disposition are rejected.
    pub fn from_tsv(line: &str) -> Option<Row> {
        let mut f = line.splitn(5, '\t');
        let kind = f.next()?.to_string();
        let id = f.next()?.to_string();
        let disp = Disposition::parse(f.next()?)?;
        let detail = f.next()?.to_string();
        let action = f.next().unwrap_or("").to_string();
        if kind.is_empty() {
            return None;
        }
        Some(Row { kind, id, disp, detail, action })
    }
}

/// A row attributed to its partition — the partition is part of a strand's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartRow {
    pub part: String,
    pub row: Row,
}

/// A row aged against the episode state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgedRow {
    pub part: String,
    pub row: Row,
    pub age: i64,
    pub acted: i64,
    pub escalated: i64,
}

/// One entry of `$SPIRA_RUN/strands.json` (DESIGN.md §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEntry {
    #[serde(default)]
    pub first: i64,
    #[serde(default)]
    pub acted: i64,
    #[serde(default)]
    pub escalated: i64,
}

/// `report --json` (DESIGN.md §3.4).
#[derive(Debug, Serialize)]
pub struct ReportJson {
    pub sentinel_timer: String,
    pub last_pass_seconds: i64,
    pub strands: Vec<ReportStrand>,
}

#[derive(Debug, Serialize)]
pub struct ReportStrand {
    pub partition: String,
    pub kind: String,
    pub id: String,
    pub disposition: String,
    pub age_seconds: i64,
    pub acted_at: i64,
    pub escalated_at: i64,
    pub detail: String,
    pub action: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_shape_and_null_fields() {
        let raw = r#"[{"id":"a","status":"open","labels":null,"dependencies":[
            {"issue_id":"a","depends_on_id":"b","type":"blocks"},
            {"issue_id":"a","depends_on_id":"e","type":"parent-child"}]}]"#;
        let b = parse_beads(raw).unwrap();
        assert_eq!(b.len(), 1);
        assert!(b[0].labels.is_empty());
        assert_eq!(b[0].blocks_targets().collect::<Vec<_>>(), vec!["b"]);
    }

    #[test]
    fn parses_show_shape() {
        let raw = r#"{"id":"a","status":"in_progress","notes":"x\n\ny",
            "dependencies":[{"id":"b","dependency_type":"blocks","status":"closed"}]}"#;
        let b = parse_beads(raw).unwrap();
        assert_eq!(b[0].status, Status::InProgress);
        assert_eq!(b[0].blocks_targets().collect::<Vec<_>>(), vec!["b"]);
        assert_eq!(b[0].notes.as_ref().unwrap().0, vec!["x", "y"]);
    }

    #[test]
    fn preamble_is_skipped_and_garbage_is_an_error() {
        assert_eq!(parse_beads("warning: x\n[]").unwrap().len(), 0);
        assert!(parse_beads("").is_err());
        assert!(parse_beads("Error: no database").is_err());
    }

    #[test]
    fn tsv_round_trip() {
        let r = Row::new("stuck", "sp-1", Disposition::Escalate, "a\tb".into(), "fix".into());
        let line = r.to_tsv();
        assert_eq!(line, "stuck\tsp-1\tescalate\ta b\tfix");
        assert_eq!(Row::from_tsv(&line).unwrap().detail, "a b");
        assert!(Row::from_tsv("stuck\tsp-1\tmaybe\td\ta").is_none());
        assert!(Row::from_tsv("").is_none());
    }
}
