//! A work bead's state, read from the lifecycle machine (design bead-lifecycle-state-machine
//! §3.4, sp-mve9i): bd holds content, spira-lc holds state. Every Rust decision that used to
//! read a bd row's `status` or `assignee` — "is it closed", "is it in progress", "who holds
//! it" — reads the bead's lifecycle row here instead: `spira-lc list` for a pass over many
//! beads, `spira-lc show <id>` for one. Both are the machine's primitives; neither falls back
//! to bd, because a fallback to bd status would be the very read this module replaces.
//!
//! The state predicates below are the bd vocabulary's lifecycle equivalents, named for the
//! decision rather than the word: bd `closed` on a work bead meant "the builder is done with
//! it" ([`past_builder`]) or "nothing more will happen to it" ([`is_terminal`]); bd
//! `in_progress` meant "an aeon holds it" ([`is_working`]); bd `open` meant "claimable"
//! ([`is_claimable`]).

use serde_json::Value;
use std::collections::HashMap;
use std::process::{Command, Stdio};

use crate::lifecycle_row::lc_bin;

/// The `wait` hold's reason that makes it a timed snooze: `snooze-until:<epoch seconds>`.
pub const SNOOZE_PREFIX: &str = "snooze-until:";

pub fn snooze_reason(until: i64) -> String {
    format!("{SNOOZE_PREFIX}{until}")
}

/// The `wait` hold's reason for an aeon checkpointed across a world stop:
/// `checkpoint-until:<epoch seconds>`. It keeps the bead unclaimable like a snooze, and
/// `world start` lifts it; the expiry only bounds a stop that is never followed by a start.
pub const CHECKPOINT_PREFIX: &str = "checkpoint-until:";

pub fn checkpoint_reason(until: i64) -> String {
    format!("{CHECKPOINT_PREFIX}{until}")
}

pub fn is_checkpoint(reason: &str) -> bool {
    reason.starts_with(CHECKPOINT_PREFIX)
}

pub fn snooze_until(reason: &str) -> Option<i64> {
    reason.strip_prefix(SNOOZE_PREFIX).or_else(|| reason.strip_prefix(CHECKPOINT_PREFIX))?.trim().parse().ok()
}

/// One `spira_lifecycle.bead` row, as `spira-lc list` / `show` print it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub bead_id: String,
    pub state: String,
    pub holder: Option<String>,
    pub lease_until: Option<i64>,
    pub holds: Vec<String>,
}

impl Row {
    pub fn terminal(&self) -> bool {
        is_terminal(&self.state)
    }
    pub fn past_builder(&self) -> bool {
        past_builder(&self.state)
    }
    pub fn working(&self) -> bool {
        is_working(&self.state)
    }
    pub fn claimable(&self) -> bool {
        is_claimable(&self.state)
    }
    pub fn held(&self, kind: &str) -> bool {
        self.holds.iter().any(|h| h == kind)
    }
}

/// No outgoing transition, for any actor (`lifecycle::bead::BeadState::is_terminal`).
pub fn is_terminal(state: &str) -> bool {
    matches!(state, "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE")
}

/// The builder has handed the bead on (or it is over): what bd `closed` (with or without the
/// submitted label) meant for a work bead. Everything but READY, WORKING and REWORK.
pub fn past_builder(state: &str) -> bool {
    !state.is_empty() && !matches!(state, "READY" | "WORKING" | "REWORK")
}

/// An aeon holds the bead: what bd `in_progress` meant.
pub fn is_working(state: &str) -> bool {
    state == "WORKING"
}

/// Nobody holds it and it waits for a builder: what bd `open` meant for a work bead.
pub fn is_claimable(state: &str) -> bool {
    matches!(state, "READY" | "REWORK")
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

fn holds(v: Option<&Value>) -> Vec<String> {
    let arr = match v {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) if !s.trim().is_empty() => match serde_json::from_str::<Value>(s) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    arr.into_iter().filter_map(|x| x.as_str().map(str::to_string)).collect()
}

fn row_of(r: &Value) -> Row {
    Row {
        bead_id: scalar(r.get("bead_id")).unwrap_or_default(),
        state: scalar(r.get("state")).unwrap_or_default(),
        holder: scalar(r.get("holder")).filter(|h| !h.is_empty()),
        lease_until: scalar(r.get("lease_until")).and_then(|x| x.trim().parse::<f64>().ok()).map(|f| f as i64),
        holds: holds(r.get("holds")),
    }
}

/// `spira-lc list`'s JSON array.
pub fn parse_rows(text: &str) -> Result<Vec<Row>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc list: not JSON: {e}"))?;
    let Value::Array(a) = v else {
        return Err("spira-lc list: not a JSON array".into());
    };
    Ok(a.iter().map(row_of).filter(|r| !r.bead_id.is_empty()).collect())
}

/// `spira-lc show <id>`'s `{"bead": {...}, "delivery": ...}`.
pub fn parse_show(text: &str) -> Result<Option<Row>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("spira-lc show: not JSON: {e}"))?;
    Ok(v.get("bead").filter(|b| b.is_object()).map(row_of).filter(|r| !r.bead_id.is_empty()))
}

fn run(bin: &str, args: &[&str]) -> Result<(i32, String), String> {
    // The call-deadline cap (spira-lint): 5 s, the same bound sending's lifecycle read keeps.
    let out = Command::new("timeout")
        .arg("5")
        .arg(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {bin}: {e}"))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if code != 0 && code != 1 {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.lines().find(|l| !l.trim().is_empty()).unwrap_or("no message");
        return Err(format!("{bin} {} exited {code}: {why}", args.join(" ")));
    }
    Ok((code, stdout))
}

/// Every lifecycle row (`spira-lc list`). Err when the machine cannot answer: a caller that
/// cannot read the state must not decide as if it had (law-a-control-that-cannot-check-must-refuse).
pub fn list_with(bin: &str) -> Result<Vec<Row>, String> {
    parse_rows(&list_raw_with(bin)?)
}

/// `spira-lc list`'s stdout, unparsed, for a caller that keeps the last good answer.
pub fn list_raw_with(bin: &str) -> Result<String, String> {
    match run(bin, &["list"])? {
        (0, out) => Ok(out),
        (rc, _) => Err(format!("{bin} list exited {rc}")),
    }
}

pub fn list_raw() -> Result<String, String> {
    list_raw_with(&lc_bin())
}

pub fn list() -> Result<Vec<Row>, String> {
    list_with(&lc_bin())
}

/// One bead's row (`spira-lc show <id>`): Ok(None) when the machine has no row for it.
pub fn row_with(bin: &str, id: &str) -> Result<Option<Row>, String> {
    match run(bin, &["show", id])? {
        (0, out) => parse_show(&out),
        _ => Ok(None),
    }
}

pub fn row(id: &str) -> Result<Option<Row>, String> {
    row_with(&lc_bin(), id)
}

/// The rows keyed by bead id.
pub fn index(rows: Vec<Row>) -> HashMap<String, Row> {
    rows.into_iter().map(|r| (r.bead_id.clone(), r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_reason_snoozes_like_a_timed_wait_and_is_told_apart() {
        let r = checkpoint_reason(1_900_000_000);
        assert_eq!(snooze_until(&r), Some(1_900_000_000));
        assert!(is_checkpoint(&r) && !is_checkpoint(&snooze_reason(5)));
        assert_eq!(snooze_until(&snooze_reason(5)), Some(5));
    }

    #[test]
    fn predicates_name_the_old_bd_decisions() {
        for s in ["SUBMITTED", "CERTIFIED", "IN_DELIVERY", "LANDED", "SUPERSEDED", "DROPPED", "DONE"] {
            assert!(past_builder(s), "{s}");
        }
        for s in ["READY", "WORKING", "REWORK", ""] {
            assert!(!past_builder(s), "{s}");
        }
        assert!(is_terminal("LANDED") && !is_terminal("SUBMITTED"));
        assert!(is_working("WORKING") && !is_working("READY"));
        assert!(is_claimable("READY") && is_claimable("REWORK") && !is_claimable("WORKING"));
    }

    #[test]
    fn list_and_show_parse_holds_either_way() {
        let rows = parse_rows(
            r#"[{"bead_id":"sp-a","state":"WORKING","holder":"aeon-1","lease_until":"17.0","holds":"[\"wait\"]"},
                {"bead_id":"sp-b","state":"READY","holder":null,"holds":[]}]"#,
        )
        .unwrap();
        assert_eq!(rows[0].holds, vec!["wait"]);
        assert_eq!(rows[0].lease_until, Some(17));
        assert_eq!(rows[1].holder, None);
        let one = parse_show(r#"{"bead":{"bead_id":"sp-c","state":"LANDED","holds":["poison"]},"delivery":null}"#).unwrap().unwrap();
        assert!(one.terminal() && one.held("poison"));
        assert!(parse_rows("{}").is_err());
    }

    #[test]
    fn a_machine_that_cannot_answer_is_an_error_and_no_row_is_none() {
        let t = testkit::TempDir::new("lcstate");
        let bin = t.path().join("lc");
        testkit::write_exe(&bin, "#!/bin/sh\ncase \"$1\" in show) exit 1;; list) echo 'cannot tell' >&2; exit 2;; esac\n");
        let bin = bin.to_string_lossy().into_owned();
        assert_eq!(row_with(&bin, "sp-x"), Ok(None));
        assert!(list_with(&bin).unwrap_err().contains("exited 2"));
    }
}
