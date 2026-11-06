//! CHECK 1, `pilgrimage.sh check` in Rust: an epic of the partition whose every child is
//! closed is announced to its subscribers and then closed; one that grows again is reopened
//! by clearing its marker. Pure: the epic set comes from the content rows (an epic and the
//! beads naming it as `parent` or by a `parent-child` dependency), the markers and the live
//! unlanded branches are the caller's.
//!
//! What the script did and this keeps: an epic with no children is EMPTY, not complete; the
//! partition is AND-scoped over `SPIRA_EPIC_LABELS`; the close is gated on every delivery,
//! so a subscriber that cannot be reached holds the epic open for the next pass; and a live
//! push-mode branch of a child that the lifecycle record does not call LANDED defers the
//! close (the sp-qj8n assertion).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::model::Bead;

/// One open epic of the partition, as `bd epic status` reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Epic {
    pub id: String,
    pub title: String,
    pub total: usize,
    pub closed: usize,
    pub children: Vec<String>,
}

impl Epic {
    /// Zero of zero is empty, not complete.
    pub fn complete(&self) -> bool {
        self.total > 0 && self.closed >= self.total
    }
}

/// The open epics carrying every label of `labels`, with their children from the same rows.
pub fn epics(beads: &[Bead], labels: &[String]) -> Vec<Epic> {
    let mut kids: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for b in beads {
        let mut parents: BTreeSet<&str> = b.parent.as_deref().filter(|p| !p.is_empty()).into_iter().collect();
        parents.extend(b.dependencies.iter().filter(|d| d.r#type.as_deref() == Some("parent-child")).filter_map(|d| d.target()));
        for p in parents.into_iter().filter(|p| *p != b.id) {
            kids.entry(p).or_default().insert(b.id.as_str());
        }
    }
    let closed = |id: &str| beads.iter().find(|b| b.id == id).is_some_and(|b| b.status == "closed");
    beads
        .iter()
        .filter(|b| b.typ() == "epic" && b.status != "closed" && labels.iter().all(|l| b.has(l)))
        .map(|e| {
            let children: Vec<String> = kids.get(e.id.as_str()).map(|s| s.iter().map(|c| c.to_string()).collect()).unwrap_or_default();
            Epic { id: e.id.clone(), title: e.title.clone().unwrap_or_default(), total: children.len(), closed: children.iter().filter(|c| closed(c)).count(), children }
        })
        .collect()
}

/// What one pass does for one epic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    /// It grew again since it was announced: the marker is cleared.
    Reopened { id: String },
    /// Complete, but a child's live push-mode branch is not LANDED: wait for the landing pass.
    Deferred { id: String, branches: Vec<String> },
    /// Complete and unannounced: notify `subscribers`, then mark and close.
    Complete { id: String, subject: String, body: String, total: usize, subscribers: Vec<String> },
}

/// The subject line the script printed, and the one the pass counted.
pub fn subject(e: &Epic) -> String {
    format!("PILGRIMAGE COMPLETE — {}: {}", e.id, e.title)
}

/// `cmd_check`: each epic against its marker (`announced`), its live unlanded branches
/// (`unlanded`, by epic) and its subscribers (`subscribers(id)`: empty is silenced).
pub fn check(epics: &[Epic], announced: &BTreeSet<String>, unlanded: &BTreeMap<String, Vec<String>>, subscribers: &dyn Fn(&str) -> Vec<String>) -> Vec<Step> {
    let mut out = vec![];
    for e in epics {
        if !e.complete() {
            if announced.contains(&e.id) {
                out.push(Step::Reopened { id: e.id.clone() });
            }
            continue;
        }
        if announced.contains(&e.id) {
            continue;
        }
        if let Some(b) = unlanded.get(&e.id).filter(|b| !b.is_empty()) {
            out.push(Step::Deferred { id: e.id.clone(), branches: b.clone() });
            continue;
        }
        let body = format!("All {} child beads closed: {} ", e.total, e.children.join(" "));
        out.push(Step::Complete { id: e.id.clone(), subject: subject(e), body, total: e.total, subscribers: subscribers(&e.id) });
    }
    out
}

/// The close reason the script handed `spira-lc close-epic`.
pub fn close_reason(total: usize, subscribers: &[String]) -> String {
    let subs = if subscribers.is_empty() { "none".to_string() } else { subscribers.join(",") };
    format!("Pilgrimage complete: all {total} child beads closed. Subscribers notified: {subs}.")
}

/// The note left on an epic held open because a subscriber could not be reached.
pub const UNDELIVERED_NOTE: &str = "Pilgrimage complete, but a subscriber could not be notified. Left open deliberately; the next sentinel pass retries.";

/// Where one address delivers, as `deliver` read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Address {
    Operator,
    Bead(String),
    Log,
    Unknown(String),
}

pub fn address(addr: &str, operator_actor: &str) -> Address {
    match addr {
        a if a == operator_actor || a == "operator" || a == "cockpit" => Address::Operator,
        a if a.starts_with("bead:") => Address::Bead(a.trim_start_matches("bead:").to_string()),
        "log" => Address::Log,
        a => Address::Unknown(a.to_string()),
    }
}

/// The operator notice's body, as the script's heredoc wrote it.
pub fn note_body(subject: &str, id: &str, body: &str) -> String {
    format!("## Note\n{subject}\n\ntarget: {id}\n\n{body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    fn world() -> Vec<Bead> {
        parse_beads(r#"[{"id":"e1","title":"Mountain","status":"open","issue_type":"epic","labels":["spira"]},
            {"id":"c1","status":"closed","parent":"e1"},
            {"id":"c2","status":"closed","dependencies":[{"depends_on_id":"e1","type":"parent-child"}]},
            {"id":"e2","title":"Other","status":"open","issue_type":"epic","labels":["gastown"]},
            {"id":"e3","title":"Empty","status":"open","issue_type":"epic","labels":["spira"]}]"#)
        .unwrap()
    }

    #[test]
    fn the_partition_is_label_scoped_and_an_empty_epic_is_not_complete() {
        let es = epics(&world(), &["spira".into()]);
        assert_eq!(es.iter().map(|e| (e.id.as_str(), e.total, e.complete())).collect::<Vec<_>>(), [("e1", 2, true), ("e3", 0, false)]);
    }

    #[test]
    fn complete_announces_deferred_waits_reopened_clears() {
        let es = epics(&world(), &["spira".into()]);
        let subs = |_: &str| vec!["ryan".to_string()];
        let steps = check(&es, &BTreeSet::new(), &BTreeMap::new(), &subs);
        assert!(matches!(&steps[..], [Step::Complete { id, subject, .. }] if id == "e1" && subject == "PILGRIMAGE COMPLETE — e1: Mountain"), "{steps:?}");
        let unlanded: BTreeMap<String, Vec<String>> = [("e1".to_string(), vec!["spira/c1".to_string()])].into();
        assert!(matches!(&check(&es, &BTreeSet::new(), &unlanded, &subs)[..], [Step::Deferred { .. }]));
        assert!(check(&es, &["e1".to_string()].into(), &BTreeMap::new(), &subs).is_empty(), "announced once");
        let grown = vec![Epic { closed: 1, ..es[0].clone() }];
        assert_eq!(check(&grown, &["e1".to_string()].into(), &BTreeMap::new(), &subs), vec![Step::Reopened { id: "e1".into() }]);
    }

    #[test]
    fn addresses_and_the_close_reason_read_as_the_script_wrote_them() {
        assert_eq!(address("ryan", "ryan"), Address::Operator);
        assert_eq!(address("bead:sp-x", "ryan"), Address::Bead("sp-x".into()));
        assert_eq!(address("pager", "ryan"), Address::Unknown("pager".into()));
        assert_eq!(close_reason(2, &[]), "Pilgrimage complete: all 2 child beads closed. Subscribers notified: none.");
    }
}
