//! bd status for a bead that is not a work bead (sp-mve9i). Design §3.4 makes bd `status`
//! inert for *work* beads — a task, bug or feature an aeon claims, whose state is the
//! lifecycle machine's (`lc_state`). An ask, an alert, an insight, an
//! intake mirror of a GitHub issue or an epic is never claimed or delivered: the machine
//! models none of its life, so bd's `status` is the only state it has, and opening or
//! closing it in bd is the whole of its lifecycle.
//!
//! Every Rust read of bd status for one of those goes through here and names the kind it
//! reads. That is the scope lifecycle-guard's `bd-status-read` rule leaves to bd (this file
//! is the rule's only exception outside the machine): a call site that reads bd status
//! without naming a non-work kind is refused at the gate, and one that names a kind says, in
//! the code a reviewer reads, which non-work bead it means. A work bead routed through here
//! is a review finding, not a loophole — the kind is the claim being made.

use serde_json::Value;

/// The non-work beads whose state bd alone holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An escalation to Ryan (`ask` label): open until answered.
    Ask,
    /// An auron/watchtower alert: open while the condition fires.
    Alert,
    /// A filed-closed observation (`bead file … insight`).
    Insight,
    /// gh-intake's mirror of a GitHub issue.
    Intake,
    /// A coordination or epic bead: closed by `spira-lc close-epic`, never delivered.
    Epic,
    /// A hold bead naming something waiting on a person (batcher-cut's round hold).
    Hold,
}

/// Which bd statuses a query wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// `open` only.
    Open,
    /// `open,in_progress`.
    Active,
    /// Everything but closed: `open,in_progress,blocked,deferred`.
    Live,
    /// `closed`.
    Closed,
}

/// The `--status` value for `which`.
pub fn status_filter(_kind: Kind, which: Which) -> &'static str {
    match which {
        Which::Open => "open",
        Which::Active => "open,in_progress",
        Which::Live => "open,in_progress,blocked,deferred",
        Which::Closed => "closed",
    }
}

/// `["--status", <filter>]` for a bd list over beads of `kind`.
pub fn status_args(kind: Kind, which: Which) -> [String; 2] {
    ["--status".to_string(), status_filter(kind, which).to_string()]
}

/// The bd status of a row (`""` when absent).
pub fn status_of(_kind: Kind, row: &Value) -> &str {
    row.get("status").and_then(Value::as_str).unwrap_or("")
}

/// bd `closed`.
pub fn is_closed(_kind: Kind, status: &str) -> bool {
    status == "closed"
}

/// bd `open`.
pub fn is_open(_kind: Kind, status: &str) -> bool {
    status == "open"
}

/// bd `in_progress`.
pub fn is_in_progress(_kind: Kind, status: &str) -> bool {
    status == "in_progress"
}

/// `open` or `in_progress`.
pub fn is_active(kind: Kind, status: &str) -> bool {
    is_open(kind, status) || is_in_progress(kind, status)
}

/// A row's bd status is `closed`.
pub fn row_closed(kind: Kind, row: &Value) -> bool {
    is_closed(kind, status_of(kind, row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_predicates() {
        assert_eq!(status_args(Kind::Ask, Which::Live), ["--status".to_string(), "open,in_progress,blocked,deferred".to_string()]);
        assert_eq!(status_filter(Kind::Alert, Which::Closed), "closed");
        let row: Value = serde_json::from_str(r#"{"id":"sp-a","status":"closed"}"#).unwrap();
        assert!(row_closed(Kind::Alert, &row));
        assert!(is_active(Kind::Alert, "in_progress") && !is_active(Kind::Alert, "closed"));
        assert_eq!(status_of(Kind::Ask, &serde_json::json!({})), "");
    }
}
