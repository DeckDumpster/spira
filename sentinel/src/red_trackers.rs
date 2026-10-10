//! A publish-red tracker closes itself once every bead it depends on has landed. The
//! tracker has no deliverable of its own: its failures are fixed by the beads it depends on,
//! so the dependency edges are the failure-to-fix map. A tracker with no edge is left alone —
//! nothing says what would fix it.

use crate::pass::Sentinel;
use crate::store;
use spira_config::{lc_state, lifecycle_row};

/// Trackers whose every `blocks` dependency is LANDED, with those dependencies.
pub fn decide(snap: &store::Snapshot, label: &str) -> Vec<(String, Vec<String>)> {
    if label.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for b in snap.list.iter().filter(|b| b.has(label)) {
        if snap.lc_state(&b.id).is_none_or(lc_state::is_terminal) {
            continue;
        }
        let fixes: Vec<String> = b
            .dependencies
            .iter()
            .filter(|d| d.r#type.as_deref() == Some("blocks"))
            .filter_map(|d| d.target().map(str::to_string))
            .collect();
        if !fixes.is_empty() && fixes.iter().all(|f| snap.lc_state(f) == Some("LANDED")) {
            out.push((b.id.clone(), fixes));
        }
    }
    out
}

impl<'a> Sentinel<'a> {
    pub fn close_landed_red_trackers(&self, snap: &store::Snapshot, dry: bool) -> usize {
        let label = self.cfg.red_tracker.clone();
        let closes = decide(snap, &label);
        for (id, fixes) in &closes {
            let reason = format!(
                "No code change: every failure this tracker names is fixed by a landed bead ({}).",
                fixes.join(", ")
            );
            if dry {
                self.h.print(&format!("would close {id}: {reason}"));
                continue;
            }
            match lifecycle_row::close_with(&self.cfg.lc_bin, id, &reason, "sentinel", None) {
                Ok(()) => self.log(&format!("close_landed_red_trackers: {id} — closed ({})", fixes.join(", "))),
                Err(e) => self.log(&format!("close_landed_red_trackers: {id} — close failed: {e}")),
            }
        }
        closes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LcRow;

    const L: &str = "spira-red-tracker";

    fn world(fix_states: &[(&str, &str)], tracker_state: &str) -> store::Snapshot {
        let deps: Vec<String> = fix_states
            .iter()
            .map(|(id, _)| format!(r#"{{"depends_on_id":"{id}","type":"blocks"}}"#))
            .collect();
        let mut beads = vec![format!(
            r#"{{"id":"t","status":"open","labels":["{L}"],"dependencies":[{}]}}"#,
            deps.join(",")
        )];
        beads.extend(fix_states.iter().map(|(id, _)| format!(r#"{{"id":"{id}","status":"open"}}"#)));
        let mut rows = vec![("t", tracker_state)];
        rows.extend(fix_states.iter().copied());
        let rows: Vec<LcRow> = rows
            .iter()
            .map(|(id, st)| LcRow { bead_id: id.to_string(), state: st.to_string(), ..Default::default() })
            .collect();
        store::Snapshot::from_json(&format!("[{}]", beads.join(",")), None).with_lc(Some(&rows))
    }

    #[test]
    fn closes_when_both_fixes_have_landed() {
        let s = world(&[("fa", "LANDED"), ("fb", "LANDED")], "READY");
        assert_eq!(decide(&s, L), vec![("t".to_string(), vec!["fa".to_string(), "fb".to_string()])]);
    }

    #[test]
    fn stays_open_while_one_fix_is_unlanded() {
        for pending in ["WORKING", "SUBMITTED", "CERTIFIED", "READY", "DROPPED"] {
            let s = world(&[("fa", "LANDED"), ("fb", pending)], "READY");
            assert!(decide(&s, L).is_empty(), "{pending}");
        }
    }

    #[test]
    fn a_tracker_with_no_fix_edge_or_already_terminal_is_left_alone() {
        assert!(decide(&world(&[], "READY"), L).is_empty());
        assert!(decide(&world(&[("fa", "LANDED")], "LANDED"), L).is_empty());
        assert!(decide(&world(&[("fa", "LANDED")], "READY"), "").is_empty());
        assert!(decide(&world(&[("fa", "LANDED")], "READY"), "other-label").is_empty());
    }
}
