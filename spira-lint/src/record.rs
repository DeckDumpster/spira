//! The typed record `--findings` writes: one JSON object per finding, so a reader attributes a
//! red from fields, never from the finding's text (law-state-is-never-read-from-text).

use serde::{Deserialize, Serialize};

use crate::Finding;

pub const FINDINGS_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingRecord {
    pub schema: u32,
    pub rule: String,
    pub path: String,
    pub line: Option<usize>,
    pub message: String,
    pub rule_source: String,
    pub allow_file: Option<String>,
}

impl FindingRecord {
    /// `rule` is the defining module's path and the rule's allow file, as the build knows them.
    pub fn of(f: &Finding, rule: Option<(&str, Option<&str>)>) -> FindingRecord {
        let (source, allow) = rule.unwrap_or(("", None));
        FindingRecord {
            schema: FINDINGS_SCHEMA,
            rule: f.rule.to_string(),
            path: f.path.clone(),
            line: f.line,
            message: f.message.clone(),
            rule_source: source.to_string(),
            allow_file: allow.map(str::to_string),
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("a record of strings serializes")
    }

    /// The record on one line, refusing another schema version or an unknown field.
    pub fn parse(line: &str) -> Result<FindingRecord, String> {
        let r: FindingRecord = serde_json::from_str(line).map_err(|e| e.to_string())?;
        if r.schema != FINDINGS_SCHEMA {
            return Err(format!("schema {} is not {FINDINGS_SCHEMA}", r.schema));
        }
        Ok(r)
    }

    /// The finding as the plain text spira-lint prints it.
    pub fn display(&self) -> String {
        match self.line {
            Some(n) => format!("{}: {}:{}: {}", self.rule, self.path, n, self.message),
            None => format!("{}: {}: {}", self.rule, self.path, self.message),
        }
    }
}
