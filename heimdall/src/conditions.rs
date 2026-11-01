//! The control-plane conditions heimdall writes its alerts as: `spira-lc condition
//! set|clear|list`, one `conditions/heimdall/<key>` per cause. A trait so the reconciliation
//! logic (`alerts.rs`) can be driven by an in-memory fake in tests.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::util::{self, Spec};

/// The condition type every alert carries; the pane lists this type.
pub const ALERT: &str = "Alert";
/// The write probe's condition: written False on every pass, so it never reads as firing.
pub const PROBE_KEY: &str = "heimdall-write-probe";
pub const PROBE: &str = "WriteProbe";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Condition {
    #[serde(default)]
    pub key: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub flaps: u64,
}

impl Condition {
    pub fn firing(&self) -> bool {
        self.status == "True"
    }
}

/// Whether a failed call timed out (contention — "saturated", not "down") or failed outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Saturated,
    Other,
}

pub type R<T> = Result<T, Failure>;

pub trait Conditions: Send + Sync {
    fn list(&self) -> R<Vec<Condition>>;
    /// Writes `key` as `status` (True: firing).
    fn set(&self, key: &str, kind: &str, reason: &str, message: &str, firing: bool) -> R<()>;
    /// Moves a firing `key` to False; nothing to clear is success.
    fn clear(&self, key: &str, reason: &str, message: &str) -> R<()>;
}

/// The conditions through the release's own `spira-lc`, bounded by `timeout`.
pub struct LcConditions<'a> {
    pub bin: String,
    pub env: &'a BTreeMap<String, String>,
    pub timeout: Duration,
}

fn fail_of(code: i32) -> Failure {
    if code == 124 {
        Failure::Saturated
    } else {
        Failure::Other
    }
}

impl LcConditions<'_> {
    fn call(&self, args: &[&str]) -> R<String> {
        let mut argv = vec!["condition".to_string()];
        argv.extend(args.iter().map(|s| s.to_string()));
        argv.extend(["--owner".into(), "heimdall".into()]);
        let o = util::run(Spec { prog: &self.bin, args: argv, env: Some(self.env), cwd: None, stdin: None, timeout: Some(self.timeout) });
        if o.success() {
            Ok(o.stdout)
        } else {
            Err(fail_of(o.code))
        }
    }
}

/// The rows of a `condition list` answer.
pub fn parse_list(text: &str) -> Option<Vec<Condition>> {
    let v: Value = serde_json::from_str(util::json_only(text).trim()).ok()?;
    let rows = v.get("conditions")?.as_array()?;
    Some(rows.iter().filter_map(|r| serde_json::from_value(r.clone()).ok()).collect())
}

impl Conditions for LcConditions<'_> {
    fn list(&self) -> R<Vec<Condition>> {
        parse_list(&self.call(&["list"])?).ok_or(Failure::Other)
    }

    fn set(&self, key: &str, kind: &str, reason: &str, message: &str, firing: bool) -> R<()> {
        let status = if firing { "True" } else { "False" };
        self.call(&["set", key, "--type", kind, "--reason", reason, "--message", message, "--status", status]).map(|_| ())
    }

    fn clear(&self, key: &str, reason: &str, message: &str) -> R<()> {
        self.call(&["clear", key, "--reason", reason, "--message", message]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_answer_is_read_past_a_notice_line() {
        let out = "notice\n{\"rev\":3,\"conditions\":[{\"key\":\"db-down\",\"owner\":\"heimdall\",\"type\":\"Alert\",\"status\":\"True\",\"flaps\":2}]}\n";
        let rows = parse_list(out).unwrap();
        assert_eq!(rows, vec![Condition { key: "db-down".into(), kind: "Alert".into(), status: "True".into(), flaps: 2 }]);
        assert!(rows[0].firing());
        assert!(parse_list("no json").is_none());
    }

    #[test]
    fn a_timeout_is_saturation() {
        assert_eq!(fail_of(124), Failure::Saturated);
        assert_eq!(fail_of(2), Failure::Other);
    }
}
