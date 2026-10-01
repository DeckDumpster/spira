//! The bead store, with lib.sh `bdq`'s rules for every call the aeon makes itself:
//! refuse an empty SPIRA_DB (bd would auto-discover the operator's real store), a timeout
//! (BD_TIMEOUT, 180 s), one retry on a pooled connection the server already dropped
//! ("invalid connection"), and SPIRA_BDJSON_FIXTURE → bdsim.py.
//!
//! bdq's create-time checks (repo label, destructive vocabulary, schema delete) and the czar
//! fence (reopen/update/close by a czar with a class) guard calls the aeon never makes
//! itself: it never creates, and its one `update` (the claim) precedes SPIRA_CZAR_CLASS.
//! Reopens go through lib.sh's bead_reopen, where the fence still applies.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::ports::{Bd, Env};
use crate::util::{self, Out, Spec};

pub struct BdCli<'a> {
    pub bd: String,
    pub db: String,
    pub timeout_s: u64,
    pub conn_retries: u32,
    pub fixture: Option<String>,
    pub home: PathBuf,
    pub env: &'a Env,
}

impl Bd for BdCli<'_> {
    fn bd(&self, args: &[String]) -> Out {
        let env = self.env.child();
        if let Some(fx) = self.fixture.as_ref().filter(|f| !f.is_empty()) {
            let mut a = vec![self.home.join("bdsim.py").display().to_string(), fx.clone()];
            a.extend(args.iter().cloned());
            return util::run(Spec { prog: "python3", args: a, env: Some(&env), cwd: None, stdin: None, timeout: None });
        }
        if self.db.is_empty() {
            return Out::fail(1, "bdq: refusing - SPIRA_DB is empty/unset (would fall through to bd auto-discovery)");
        }
        let mut a = vec!["-C".to_string(), self.db.clone()];
        a.extend(args.iter().cloned());
        let tries = self.conn_retries.max(1);
        let mut last = Out::default();
        for t in 1..=tries {
            last = util::run(Spec {
                prog: &self.bd,
                args: a.clone(),
                env: Some(&env),
                cwd: None,
                stdin: None,
                timeout: Some(Duration::from_secs(self.timeout_s.max(1))),
            });
            if last.code == 0 || t >= tries || !last.stderr.contains("invalid connection") {
                break;
            }
        }
        last
    }
}

// ---- helpers every module uses -------------------------------------------------------

pub fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// `bdjson`: the call with `--json`, stderr dropped, output through json_only.
pub fn json(bd: &dyn Bd, a: &[&str]) -> String {
    let mut v = args(a);
    v.push("--json".into());
    let o = bd.bd(&v);
    util::json_only(&o.stdout)
}

/// A bd row, only the fields read. `bd show` and `bd list` spell the dependency type
/// differently (`dependency_type` vs `type`); both are accepted.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct BeadRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub issue_type: Option<String>,
    #[serde(default)]
    pub dependencies: Option<Vec<Dep>>,
    #[serde(default)]
    pub close_reason: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Dep {
    #[serde(default)]
    pub dependency_type: Option<String>,
    #[serde(default, rename = "type")]
    pub typ: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub title: Option<String>,
}

impl Dep {
    /// Either spelling `bd show`/`bd list` use for the dependency type.
    pub fn kind(&self) -> Option<&str> {
        self.dependency_type.as_deref().filter(|s| !s.is_empty()).or(self.typ.as_deref())
    }
}

impl BeadRow {
    pub fn labels(&self) -> &[String] {
        self.labels.as_deref().unwrap_or(&[])
    }
    pub fn has_label(&self, l: &str) -> bool {
        self.labels().iter().any(|x| x == l)
    }
    pub fn label_value(&self, prefix: &str) -> Option<String> {
        self.labels().iter().find_map(|l| l.strip_prefix(prefix).map(|v| v.to_string()))
    }
    pub fn superseded(&self) -> bool {
        self.dependencies.as_deref().unwrap_or(&[]).iter().any(|d| d.kind() == Some("supersedes"))
    }
    /// Every `delivers:TYPE` joined with `;` (the verdict's `delivers`).
    pub fn delivers(&self) -> String {
        self.labels().iter().filter_map(|l| l.strip_prefix("delivers:")).collect::<Vec<_>>().join(";")
    }
}

/// The first row of a JSON blob that is either one object or a list of them.
pub fn first_row(json: &str) -> Option<BeadRow> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let row = match v {
        serde_json::Value::Array(mut a) => {
            if a.is_empty() {
                return None;
            }
            a.remove(0)
        }
        o @ serde_json::Value::Object(_) => o,
        _ => return None,
    };
    serde_json::from_value(row).ok()
}

/// `bd show <id> --json`, first row.
pub fn show(bd: &dyn Bd, id: &str) -> Option<BeadRow> {
    first_row(&json(bd, &["show", id]))
}

/// `delivers:beads`' own check: how many of `bd children <id> --json`'s rows are real work,
/// not a bare event record (an aeon's own state-change log is not a deliverable). Unparseable
/// or empty input counts as zero, never an error — the caller treats zero as "not satisfied".
pub fn child_work_count(json: &str) -> i64 {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json.trim()) else { return 0 };
    let rows: Vec<serde_json::Value> = match v {
        serde_json::Value::Array(a) => a,
        o @ serde_json::Value::Object(_) => vec![o],
        _ => return 0,
    };
    rows.iter()
        .filter(|r| r.get("id").and_then(|i| i.as_str()).is_some_and(|s| !s.is_empty()) && r.get("issue_type").and_then(|t| t.as_str()) != Some("event"))
        .count() as i64
}

/// `bdq note <id> <text> >/dev/null 2>&1`.
pub fn note(bd: &dyn Bd, id: &str, text: &str) {
    let _ = bd.bd(&args(&["note", id, text]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_row_accepts_object_or_list() {
        assert_eq!(first_row(r#"[{"id":"sp-a","status":"open"}]"#).unwrap().id, "sp-a");
        assert_eq!(first_row(r#"{"id":"sp-b"}"#).unwrap().id, "sp-b");
        assert!(first_row("[]").is_none());
        assert!(first_row("not json").is_none());
    }

    #[test]
    fn supersedes_in_either_spelling() {
        let a = first_row(r#"{"id":"a","dependencies":[{"dependency_type":"supersedes"}]}"#).unwrap();
        let b = first_row(r#"{"id":"b","dependencies":[{"type":"supersedes"}]}"#).unwrap();
        let c = first_row(r#"{"id":"c","dependencies":[{"type":"blocks"}]}"#).unwrap();
        assert!(a.superseded() && b.superseded() && !c.superseded());
    }

    #[test]
    fn delivers_joins_types() {
        let r = first_row(r#"{"id":"a","labels":["delivers:note","x","delivers:beads"]}"#).unwrap();
        assert_eq!(r.delivers(), "note;beads");
        assert_eq!(r.label_value("delivers:").as_deref(), Some("note"));
    }

    #[test]
    fn child_work_count_excludes_event_records() {
        let j = r#"[{"id":"sp-a","issue_type":"task"},{"id":"sp-b","issue_type":"event"},{"id":"","issue_type":"task"}]"#;
        assert_eq!(child_work_count(j), 1);
        assert_eq!(child_work_count("[]"), 0);
        assert_eq!(child_work_count(""), 0);
        assert_eq!(child_work_count("not json"), 0);
        assert_eq!(child_work_count(r#"{"id":"sp-a","issue_type":"task"}"#), 1);
    }

    #[test]
    fn empty_db_is_refused_not_autodiscovered() {
        let env = Env::new(Default::default(), Default::default());
        let bd = BdCli { bd: "bd".into(), db: String::new(), timeout_s: 1, conn_retries: 2, fixture: None, home: "/nonexistent".into(), env: &env };
        let o = bd.bd(&args(&["show", "sp-a"]));
        assert_eq!(o.code, 1);
        assert!(o.stderr.contains("refusing"));
    }
}
