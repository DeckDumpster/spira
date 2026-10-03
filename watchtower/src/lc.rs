//! Delivery and bead state as `spira-lc` answers it. A caller that cannot reach spira-lc gets
//! `None`, never an empty list: "cannot tell" must not read as "nothing queued".

use serde_json::Value;
use std::process::{Command, Stdio};

pub const PROGRAM_ENV: &str = "SPIRA_LC_BIN";
const HARNESS_ACTOR: &str = "harness";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeadRow {
    pub id: String,
    pub tip: String,
    pub since: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryRow {
    pub id: String,
    pub mode: String,
    pub entered_at: Option<i64>,
    pub version: String,
}

fn program() -> String {
    std::env::var(PROGRAM_ENV).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "spira-lc".to_string())
}

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn epoch(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn run(args: &[&str]) -> Option<String> {
    let out = Command::new(program()).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn rows(args: &[&str]) -> Option<Vec<Value>> {
    match serde_json::from_str::<Value>(&run(args)?).ok()? {
        Value::Array(a) => Some(a),
        _ => None,
    }
}

pub fn beads_in(state: &str) -> Option<Vec<BeadRow>> {
    Some(
        rows(&["list", "--state", state])?
            .iter()
            .map(|r| BeadRow { id: text(r, "bead_id"), tip: text(r, "tip"), since: epoch(r, "since") })
            .collect(),
    )
}

/// The newest `LANDED` bead and when it entered that state.
pub fn last_landed() -> Option<(String, i64)> {
    beads_in("LANDED")?.into_iter().filter_map(|b| Some((b.id, b.since?))).max_by_key(|(_, at)| *at)
}

pub fn pr_open() -> Option<Vec<DeliveryRow>> {
    Some(
        rows(&["list", "--delivery", "--state", "PR_OPEN"])?
            .iter()
            .map(|r| DeliveryRow {
                id: text(r, "bead_id"),
                mode: text(r, "mode"),
                entered_at: epoch(r, "entered_at"),
                version: text(r, "version"),
            })
            .collect(),
    )
}

/// The PrOpen `Requeued` event, as the harness: the exit that sends the bead back through
/// the landing pass for a rebase.
pub fn requeue_pr_open(id: &str, version: &str) -> bool {
    let Some(shown) = run(&["show", id]).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return false;
    };
    let tip = shown.get("bead").map(|b| text(b, "tip")).unwrap_or_default();
    let kind = serde_json::json!({"Requeued": {"tip": tip}}).to_string();
    run(&["event", "delivery", id, "--expect", "PR_OPEN", "--version", version, "--actor", HARNESS_ACTOR, "--kind", &kind]).is_some()
}

#[cfg(test)]
pub fn fixture_script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("spira-lc-stub.sh");
    testkit::write_exe(&p, &format!("#!/usr/bin/env bash\n{body}\n"));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_parse_rows_and_requeue_sends_the_event() {
        let d = testkit::TempDir::new("wt-lc");
        let stub = fixture_script(
            &d,
            r#"case "$*" in
  "list --state LANDED") echo '[{"bead_id":"sp-a","tip":"t","since":100},{"bead_id":"sp-b","tip":"t","since":300},{"bead_id":"sp-c","tip":"t","since":null}]' ;;
  "list --state CERTIFIED") echo '[{"bead_id":"sp-x","tip":"abc","since":"7"}]' ;;
  "list --delivery --state PR_OPEN") echo '[{"bead_id":"sp-p","mode":"pr","version":2,"entered_at":50}]' ;;
  "show sp-p") echo '{"bead":{"tip":"tt"}}' ;;
  "event delivery sp-p --expect PR_OPEN --version 2 --actor harness --kind {\"Requeued\":{\"tip\":\"tt\"}}") exit 0 ;;
  *) exit 7 ;;
esac"#,
        );
        let _env = testkit::env(&[(PROGRAM_ENV, stub.to_str())]);
        assert_eq!(last_landed(), Some(("sp-b".to_string(), 300)));
        let c = beads_in("CERTIFIED").unwrap();
        assert_eq!(c, vec![BeadRow { id: "sp-x".into(), tip: "abc".into(), since: Some(7) }]);
        let p = pr_open().unwrap();
        assert_eq!(p[0], DeliveryRow { id: "sp-p".into(), mode: "pr".into(), entered_at: Some(50), version: "2".into() });
        assert!(requeue_pr_open("sp-p", "2"));
        assert!(!requeue_pr_open("sp-p", "3"));
        assert_eq!(beads_in("SUBMITTED"), None);
    }

    #[test]
    fn an_unreachable_program_is_none_not_empty() {
        let d = testkit::TempDir::new("wt-lc-absent");
        let absent = d.join("absent");
        let _env = testkit::env(&[(PROGRAM_ENV, absent.to_str())]);
        assert_eq!(last_landed(), None);
        assert_eq!(pr_open(), None);
        assert!(!requeue_pr_open("sp-p", "2"));
    }
}
