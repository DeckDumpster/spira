//! CHECK 3c — coordination beads with open children (DESIGN.md §4, CHECK 3b/3c).
//!
//! Replaces lib.sh `mark_open_children`, which asked `bd children <id>` once per candidate:
//! 221 candidates cost 302 s of a 60 s pass budget on production (sp-du8bv). The answer is
//! already in the pass's one `bd list --all` snapshot — every row carries its `parent` and
//! its `parent-child` dependency — so the whole decision is one walk over rows the pass has
//! in memory, and the store is touched only for the label writes that actually change
//! something (law: every stage CPU/memory/IO bound, never bead-store bound).
//!
//! The contract is lib.sh's, with a bead's state read from its lifecycle row instead of bd
//! `status` (design §3.4, sp-mve9i — bd holds content, spira-lc holds state):
//! - candidates are the union of the (scope-filtered) ready set and the beads currently
//!   carrying the label whose lifecycle row is claimable (READY/REWORK — bd `open`);
//! - a candidate with any open child gains the label if it lacks it. A child is open while
//!   its lifecycle row is not terminal (LANDED/SUPERSEDED/DROPPED/DONE). A child with no
//!   row is not a work bead the machine tracks: an epic/event child is read through
//!   `spira_config::nonwork` (bd status is an epic's only state); any other rowless child
//!   counts as OPEN — the conservative answer, since a parent wrongly kept out of dispatch
//!   is a visible stall while a parent wrongly dispatched builds on unfinished work;
//! - a parent with work of its own (a recorded tip, or a live child whose stack names it)
//!   is not a coordination bead and is never labeled: its children wait on its rework;
//! - a labeled candidate whose children are all done (or who has none) loses it;
//! - the same log lines, byte for byte.

use std::collections::{BTreeSet, HashSet};

use crate::model::Bead;
use crate::pass::Sentinel;
use crate::store;
use spira_config::lc_state;
use spira_config::nonwork::{self, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Add(String),
    Remove(String),
}

/// Whether a child is still open (module doc): its lifecycle row is not terminal; rowless,
/// an epic/event by its bd status, anything else open.
fn child_open(snap: &store::Snapshot, b: &Bead) -> bool {
    match snap.lc_state(&b.id) {
        Some(state) => !lc_state::is_terminal(state),
        None if matches!(b.typ(), "epic" | "event") => !nonwork::is_closed(Kind::Epic, &b.status),
        None => true,
    }
}

/// Every bead that has at least one open child, by either link bd records: the row's
/// `parent` field or a `parent-child` dependency naming the parent.
pub fn open_parents(snap: &store::Snapshot) -> HashSet<&str> {
    let mut out = HashSet::new();
    for b in snap.list.iter().filter(|b| child_open(snap, b)) {
        if let Some(p) = b.parent.as_deref().filter(|p| !p.is_empty() && *p != b.id) {
            out.insert(p);
        }
        for d in &b.dependencies {
            if d.r#type.as_deref() == Some("parent-child") {
                if let Some(t) = d.target().filter(|t| !t.is_empty() && *t != b.id) {
                    out.insert(t);
                }
            }
        }
    }
    out
}

/// The label decision over one snapshot. `ready` is the ready set already narrowed to the
/// scope; `snap.list` is the whole store. Output order: ready candidates first, then
/// labeled-only ones, each in their input order (lib.sh's `dict.fromkeys(ready + labeled)`).
pub fn decide(snap: &store::Snapshot, ready: &[Bead], label: &str) -> Vec<Change> {
    if label.is_empty() {
        return Vec::new();
    }
    let open = open_parents(snap);
    let labeled: Vec<&str> = snap
        .list
        .iter()
        .filter(|b| snap.lc_state(&b.id).is_some_and(lc_state::is_claimable) && b.has(label))
        .map(|b| b.id.as_str())
        .collect();
    let labeled_set: HashSet<&str> = labeled.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in ready
        .iter()
        .map(|b| b.id.as_str())
        .chain(labeled.iter().copied())
    {
        if id.is_empty() || !seen.insert(id) {
            continue;
        }
        let has_open = open.contains(id) && !snap.has_own_work(id);
        let currently = labeled_set.contains(id);
        if has_open && !currently {
            out.push(Change::Add(id.to_string()));
        } else if !has_open && currently {
            out.push(Change::Remove(id.to_string()));
        }
    }
    out
}

/// The ready rows the claim query would return: the pass's broad ready snapshot narrowed to
/// SPIRA_SCOPE_LABEL, exactly as mark_queue_waiters re-applies it.
pub fn scoped(ready: &[Bead], scope: &str) -> Vec<Bead> {
    ready
        .iter()
        .filter(|b| scope.is_empty() || b.has(scope))
        .cloned()
        .collect()
}

impl<'a> Sentinel<'a> {
    /// CHECK 3c over the pass's snapshot. `dry` prints the decision and writes nothing.
    pub fn mark_open_children(&self, snap: &store::Snapshot, dry: bool) -> usize {
        let label = self.cfg.open_children.clone();
        if label.is_empty() {
            return 0;
        }
        if snap.list.is_empty() {
            self.h.log_err(
                "WARN mark_open_children: no store snapshot this pass — the open-children label is not re-evaluated",
            );
            return 0;
        }
        // The broad ready snapshot is read once per pass; without it, one live claim query.
        let ready = match &snap.ready {
            Some(r) => scoped(r, &self.cfg.scope),
            None => store::read_json(&self.bd(), self.h, &store::ready_args(&self.cfg))
                .map(|(_, v)| v)
                .unwrap_or_default(),
        };
        let changes = decide(snap, &ready, &label);
        // Re-read before writing (fresh.rs): one live read for every bead about to change.
        let live = if dry || changes.is_empty() {
            None
        } else {
            let ids: Vec<&str> = changes
                .iter()
                .map(|c| match c {
                    Change::Add(i) | Change::Remove(i) => i.as_str(),
                })
                .collect();
            self.reread(&ids)
        };
        for c in &changes {
            if !dry {
                let (Change::Add(id) | Change::Remove(id)) = c;
                let was = snap.lc_state(id);
                if self.still("CHECK3c", id, was, live.as_ref()).is_none() {
                    continue;
                }
            }
            match c {
                Change::Add(id) => {
                    if dry {
                        self.h.print(&format!("would add {label} to {id}"));
                        continue;
                    }
                    self.bd().quiet(self.h, &["label", "add", id, &label], None);
                    self.log(&format!(
                        "mark_open_children: {id} — has an open child, excluded from dispatch"
                    ));
                }
                Change::Remove(id) => {
                    if dry {
                        self.h.print(&format!("would remove {label} from {id}"));
                        continue;
                    }
                    self.bd()
                        .quiet(self.h, &["label", "remove", id, &label], None);
                    self.log(&format!(
                        "mark_open_children: {id} — children all closed, re-enters dispatch"
                    ));
                }
            }
        }
        changes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    const L: &str = "spira-open-children";

    fn beads(j: &str) -> Vec<Bead> {
        parse_beads(j).unwrap()
    }

    /// The store `j`, with each bead's lifecycle row in the state its fixture `status`
    /// stands for (open READY, in_progress WORKING, closed LANDED) — the fixtures predate
    /// the lifecycle read; `lc_disagrees_with_bd` is the test where the two differ.
    fn snap(j: &str) -> store::Snapshot {
        let rows: Vec<crate::model::LcRow> = beads(j)
            .iter()
            .map(|b| crate::model::LcRow {
                bead_id: b.id.clone(),
                state: match b.status.as_str() {
                    "in_progress" => "WORKING",
                    "closed" => "LANDED",
                    _ => "READY",
                }
                .into(),
                ..Default::default()
            })
            .collect();
        store::Snapshot::from_json(j, None).with_lc(Some(&rows))
    }

    fn lc_snap(j: &str, rows: &[(&str, &str)]) -> store::Snapshot {
        let rows: Vec<crate::model::LcRow> = rows
            .iter()
            .map(|(id, st)| crate::model::LcRow { bead_id: id.to_string(), state: st.to_string(), ..Default::default() })
            .collect();
        store::Snapshot::from_json(j, None).with_lc(Some(&rows))
    }

    /// sp-mve9i: whether a child is open is its lifecycle row, never bd's status. bd says
    /// k1 is closed and k2 open; the machine says k1 is WORKING and k2 LANDED.
    #[test]
    fn lc_disagrees_with_bd() {
        let j = r#"[{"id":"p","status":"open"},{"id":"q","status":"open","labels":["spira-open-children"]},
                    {"id":"k1","status":"closed","parent":"p"},{"id":"k2","status":"open","parent":"q"}]"#;
        let s = lc_snap(j, &[("p", "READY"), ("q", "READY"), ("k1", "WORKING"), ("k2", "LANDED")]);
        assert_eq!(
            decide(&s, &beads(r#"[{"id":"p"}]"#), L),
            vec![Change::Add("p".into()), Change::Remove("q".into())]
        );
        // A labeled candidate is one the machine says is claimable, whatever bd says.
        let s = lc_snap(j, &[("p", "READY"), ("q", "SUBMITTED"), ("k1", "WORKING"), ("k2", "LANDED")]);
        assert_eq!(decide(&s, &beads(r#"[{"id":"p"}]"#), L), vec![Change::Add("p".into())]);
    }

    /// A rowless child: an epic by its bd status (nonwork), anything else counts open.
    #[test]
    fn a_rowless_child_is_open_unless_it_is_a_closed_epic() {
        let j = r#"[{"id":"p","status":"open"},{"id":"e","status":"closed","issue_type":"epic","parent":"p"},
                    {"id":"r","status":"open"},{"id":"x","status":"closed","issue_type":"task","parent":"r"}]"#;
        let s = lc_snap(j, &[("p", "READY"), ("r", "READY")]);
        assert_eq!(decide(&s, &beads(r#"[{"id":"p"},{"id":"r"}]"#), L), vec![Change::Add("r".into())]);
    }

    #[test]
    fn a_rework_parent_with_a_stacked_child_is_never_labeled() {
        let j = r#"[{"id":"p","status":"open"},{"id":"k","status":"open","parent":"p"},
                    {"id":"c","status":"open"},{"id":"ck","status":"open","parent":"c"}]"#;
        let mk = |id: &str, st: &str, tip: Option<&str>, stack: &[&str]| crate::model::LcRow {
            bead_id: id.into(),
            state: st.into(),
            tip: tip.map(Into::into),
            stack: stack.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let rows = [
            mk("p", "REWORK", Some("abc"), &[]),
            mk("k", "REWORK", None, &["p"]),
            mk("c", "READY", None, &[]),
            mk("ck", "READY", None, &[]),
        ];
        let s = store::Snapshot::from_json(j, None).with_lc(Some(&rows));
        assert_eq!(decide(&s, &beads(r#"[{"id":"p"},{"id":"c"}]"#), L), vec![Change::Add("c".into())]);
        let labeled = r#"[{"id":"p","status":"open","labels":["spira-open-children"]},{"id":"k","status":"open","parent":"p"}]"#;
        let s = store::Snapshot::from_json(labeled, None).with_lc(Some(&[mk("p", "REWORK", Some("abc"), &[]), mk("k", "REWORK", None, &["p"])]));
        assert_eq!(decide(&s, &[], L), vec![Change::Remove("p".into())]);
    }

    #[test]
    fn a_parent_with_an_open_child_gains_the_label() {
        let list = snap(
            r#"[{"id":"p","status":"open","labels":["plan"]},
                {"id":"k1","status":"open","dependencies":[{"depends_on_id":"p","type":"parent-child"}]},
                {"id":"k2","status":"closed","parent":"p"},
                {"id":"lonely","status":"open"}]"#,
        );
        let ready = beads(r#"[{"id":"p"},{"id":"lonely"}]"#);
        assert_eq!(decide(&list, &ready, L), vec![Change::Add("p".into())]);
    }

    #[test]
    fn the_parent_field_alone_counts_as_a_link() {
        let list =
            snap(r#"[{"id":"p","status":"open"},{"id":"k","status":"in_progress","parent":"p"}]"#);
        let ready = beads(r#"[{"id":"p"}]"#);
        assert_eq!(decide(&list, &ready, L), vec![Change::Add("p".into())]);
    }

    #[test]
    fn already_labeled_with_an_open_child_is_left_alone() {
        let list = snap(
            r#"[{"id":"p","status":"open","labels":["spira-open-children"]},
                {"id":"k","status":"open","parent":"p"}]"#,
        );
        let ready = beads(r#"[{"id":"p"}]"#);
        assert!(decide(&list, &ready, L).is_empty());
    }

    #[test]
    fn every_child_closed_removes_the_label_even_off_the_ready_set() {
        let list = snap(
            r#"[{"id":"p","status":"open","labels":["spira-open-children"]},
                {"id":"k1","status":"closed","parent":"p"},
                {"id":"k2","status":"closed","dependencies":[{"depends_on_id":"p","type":"parent-child"}]}]"#,
        );
        assert_eq!(decide(&list, &[], L), vec![Change::Remove("p".into())]);
    }

    #[test]
    fn one_of_two_children_open_keeps_the_label() {
        let list = snap(
            r#"[{"id":"p","status":"open","labels":["spira-open-children"]},
                {"id":"k1","status":"closed","parent":"p"},
                {"id":"k2","status":"open","parent":"p"}]"#,
        );
        assert!(decide(&list, &beads(r#"[{"id":"p"}]"#), L).is_empty());
    }

    #[test]
    fn a_blocks_dependency_is_not_a_child() {
        let list = snap(
            r#"[{"id":"p","status":"open"},
                {"id":"k","status":"open","dependencies":[{"depends_on_id":"p","type":"blocks"}]}]"#,
        );
        assert!(decide(&list, &beads(r#"[{"id":"p"}]"#), L).is_empty());
    }

    #[test]
    fn a_labeled_bead_not_open_is_not_a_candidate() {
        // lib.sh read the labeled set with `--status open`; an in_progress labeled bead is
        // neither re-labeled nor cleared unless it is also ready.
        let list = snap(r#"[{"id":"p","status":"in_progress","labels":["spira-open-children"]}]"#);
        assert!(decide(&list, &[], L).is_empty());
    }

    #[test]
    fn an_empty_label_disables_the_check() {
        let list = snap(r#"[{"id":"p","status":"open"},{"id":"k","status":"open","parent":"p"}]"#);
        assert!(decide(&list, &beads(r#"[{"id":"p"}]"#), "").is_empty());
    }

    #[test]
    fn candidates_are_deduplicated_ready_first() {
        let list = snap(
            r#"[{"id":"a","status":"open","labels":["spira-open-children"]},
                {"id":"b","status":"open"},
                {"id":"kb","status":"open","parent":"b"}]"#,
        );
        let ready = beads(r#"[{"id":"b"},{"id":"a"},{"id":"b"}]"#);
        assert_eq!(
            decide(&list, &ready, L),
            vec![Change::Add("b".into()), Change::Remove("a".into())]
        );
    }

    #[test]
    fn scope_narrows_the_ready_set() {
        let ready = beads(r#"[{"id":"a","labels":["spira"]},{"id":"b","labels":["other"]}]"#);
        let s: Vec<String> = scoped(&ready, "spira").into_iter().map(|b| b.id).collect();
        assert_eq!(s, vec!["a"]);
        assert_eq!(scoped(&ready, "").len(), 2);
    }

    /// The scale that made the bash version cost 302 s: the decision over 8,500 rows and
    /// 220 candidates is pure computation and must stay far inside one second.
    #[test]
    fn eight_thousand_rows_decide_in_well_under_a_second() {
        let mut rows = Vec::new();
        for i in 0..8500 {
            let parent = if i % 7 == 0 {
                format!(",\"parent\":\"b{}\"", i / 7)
            } else {
                String::new()
            };
            let st = if i % 3 == 0 { "open" } else { "closed" };
            rows.push(format!("{{\"id\":\"b{i}\",\"status\":\"{st}\"{parent}}}"));
        }
        let list = snap(&format!("[{}]", rows.join(",")));
        let ready = beads(&format!(
            "[{}]",
            (0..220)
                .map(|i| format!("{{\"id\":\"b{i}\"}}"))
                .collect::<Vec<_>>()
                .join(",")
        ));
        let t = std::time::Instant::now();
        let n = decide(&list, &ready, L).len();
        assert!(n > 0);
        assert!(t.elapsed().as_millis() < 1000, "took {:?}", t.elapsed());
    }
}
