//! `deadlocked` against an in-memory world. No store, no git — the merge-status a real
//! `groomer deadlocked` would compute is simply asserted as input (`Candidate.ok`).

use super::*;
use crate::unpoison::BeadRecord;
use std::collections::BTreeMap;

#[derive(Default)]
struct Fake {
    beads: BTreeMap<String, BeadRecord>,
    lc: BTreeMap<String, LcRow>,
    events: Vec<crate::events::EventRow>,
    notes: Vec<(String, String)>,
    removed_labels: Vec<(String, String)>,
    lifted: BTreeMap<String, u32>,
    refuse_unhold_times: u32,
    lc_forbidden: bool,
}

impl Fake {
    fn bead(mut self, id: &str, status: &str, assignee: Option<&str>, labels: &[&str]) -> Self {
        self.beads.insert(
            id.into(),
            BeadRecord {
                id: id.into(),
                status: status.into(),
                assignee: assignee.map(str::to_string),
                labels: labels.iter().map(|s| s.to_string()).collect(),
            },
        );
        self
    }
    fn lc(
        mut self,
        id: &str,
        state: lifecycle::bead::BeadState,
        holds: &[lifecycle::bead::HoldKind],
        holder: Option<&str>,
    ) -> Self {
        self.lc.insert(
            id.into(),
            LcRow {
                state,
                version: 3,
                holds: holds.iter().copied().collect(),
                holder: holder.map(str::to_string),
            },
        );
        self
    }
    fn claims(mut self, id: &str, n: usize) -> Self {
        for i in 0..n {
            self.events.push(crate::events::EventRow::new(
                id,
                "claimed",
                "",
                &format!("2026-09-28T09:{i:02}:00Z"),
            ));
        }
        self
    }
}

impl World for Fake {
    fn bead(&mut self, id: &str) -> Result<Option<BeadRecord>, String> {
        Ok(self.beads.get(id).cloned())
    }
    fn events(&mut self, id: &str) -> Result<Vec<crate::events::EventRow>, String> {
        Ok(self
            .events
            .iter()
            .filter(|e| e.issue_id == id)
            .cloned()
            .collect())
    }
    fn lc_row(&mut self, id: &str) -> Result<Option<LcRow>, String> {
        assert!(
            !self.lc_forbidden,
            "spira-lc show called with lifecycle_enforce off ({id})"
        );
        Ok(self.lc.get(id).cloned())
    }
    fn lc_unhold_poison(&mut self, id: &str, row: &LcRow, _actor: &str) -> LcApply {
        if self.refuse_unhold_times > 0 {
            self.refuse_unhold_times -= 1;
            return LcApply::Refused("version mismatch".into());
        }
        let mut r = row.clone();
        r.holds.remove(&lifecycle::bead::HoldKind::Poison);
        self.lc.insert(id.to_string(), r);
        LcApply::Applied
    }
    fn write_event(&mut self, _id: &str, _event_type: &str, _value: &str) -> Result<(), String> {
        unreachable!("deadlocked never writes an event")
    }
    fn clear_ask_history(&mut self, _id: &str) -> Result<(), String> {
        unreachable!("deadlocked never touches ask history")
    }
    fn ask_history_exists(&mut self, _id: &str) -> bool {
        unreachable!("deadlocked never touches ask history")
    }
    fn remove_label(&mut self, id: &str, label: &str) -> Result<(), String> {
        self.removed_labels
            .push((id.to_string(), label.to_string()));
        if let Some(b) = self.beads.get_mut(id) {
            b.labels.retain(|l| l != label);
        }
        Ok(())
    }
    fn note(&mut self, id: &str, text: &str) -> Result<(), String> {
        self.notes.push((id.to_string(), text.to_string()));
        Ok(())
    }
    fn open_asks(&mut self) -> Result<Vec<crate::unpoison::AskRow>, String> {
        unreachable!("deadlocked never closes an ask")
    }
    fn close(&mut self, _id: &str, _reason: &str) -> Result<(), String> {
        unreachable!("deadlocked never closes an ask")
    }
    fn audit_len(&mut self) -> u64 {
        unreachable!("deadlocked never watches the audit log")
    }
    fn audit_read_from(&mut self, _offset: u64) -> Result<Vec<u8>, String> {
        unreachable!("deadlocked never watches the audit log")
    }
    fn now(&mut self) -> i64 {
        0
    }
    fn sleep(&mut self, _secs: u64) {
        unreachable!("deadlocked never sleeps")
    }
    fn mark_poison_lifted(&mut self, id: &str, attempts: u32) -> Result<(), String> {
        self.lifted.insert(id.to_string(), attempts);
        Ok(())
    }
}

fn cand(id: &str, ok: bool, why: &str) -> Candidate {
    Candidate {
        id: id.into(),
        ok,
        why: why.into(),
        branch: format!("spira/{id}"),
        base: "origin/main".into(),
    }
}

fn opts(apply: bool, enforce: bool) -> Opts {
    Opts {
        apply,
        actor: "groomer".into(),
        enforce,
    }
}

#[test]
fn not_poisoned_is_silent_and_uncounted() {
    let mut w = Fake::default().bead("sp-a", "open", None, &[]);
    let (code, out) = run(&opts(true, false), &[cand("sp-a", true, "")], &mut w);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "--- 0 poisoned bead(s) examined, 0 finished and landable, 0 restored\n"
    );
    assert!(w.notes.is_empty());
}

#[test]
fn off_mode_keeps_a_bead_whose_work_never_merges() {
    let mut w = Fake::default().bead("sp-k", "open", None, &["spira-poison"]);
    let (code, out) = run(
        &opts(false, false),
        &[cand(
            "sp-k",
            false,
            "no branch spira/sp-k — nothing was committed",
        )],
        &mut w,
    );
    assert_eq!(code, 0);
    assert!(
        out.contains("KEEP     sp-k no branch spira/sp-k — nothing was committed"),
        "{out}"
    );
    assert!(
        out.contains("--- 1 poisoned bead(s) examined, 0 finished and landable, 0 restored"),
        "{out}"
    );
    assert!(w.notes.is_empty());
    assert!(w.removed_labels.is_empty());
}

#[test]
fn dry_run_would_and_writes_nothing() {
    let mut w = Fake::default().bead("sp-s", "open", None, &["spira-poison"]);
    let (code, out) = run(&opts(false, false), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 0);
    assert!(out.contains("WOULD    sp-s finished on spira/sp-s and it merges into origin/main — would lift the poison"), "{out}");
    assert!(
        out.contains("--- 1 poisoned bead(s) examined, 1 finished and landable, 0 restored"),
        "{out}"
    );
    assert!(out.contains("dry run"));
    assert!(w.notes.is_empty());
}

#[test]
fn off_mode_apply_restores_and_leaves_the_attempt_record_standing() {
    let mut w = Fake::default()
        .bead("sp-s", "open", None, &["spira-poison"])
        .claims("sp-s", 3);
    let (code, out) = run(&opts(true, false), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(
            "RESTORED sp-s poison lifted; spira/sp-s is finished and merges into origin/main"
        ),
        "{out}"
    );
    assert_eq!(
        w.removed_labels,
        vec![("sp-s".to_string(), "spira-poison".to_string())]
    );
    assert_eq!(w.notes.len(), 1);
    assert!(
        w.notes[0]
            .1
            .contains("Poison lifted by spira-claim deadlocked"),
        "{}",
        w.notes[0].1
    );
    assert!(!w.beads["sp-s"].labels.contains(&"spira-poison".to_string()));
    // sp-wiyr2: the record of the count this lift happened at, so the very next CHECK 4
    // pass — reading the same 3 charged attempts, since deadlocked never floors them —
    // does not poison the bead right back.
    assert_eq!(w.lifted.get("sp-s"), Some(&3));
}

#[test]
fn on_mode_apply_unholds_and_never_touches_the_label() {
    use lifecycle::bead::{BeadState, HoldKind};
    let mut w = Fake::default().bead("sp-s", "open", None, &[]).lc(
        "sp-s",
        BeadState::Ready,
        &[HoldKind::Poison],
        None,
    );
    let (code, out) = run(&opts(true, true), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("RESTORED sp-s"), "{out}");
    assert!(!w.lc["sp-s"].holds.contains(&HoldKind::Poison));
    assert!(
        w.removed_labels.is_empty(),
        "no label was carried, so nothing to remove"
    );
}

#[test]
fn on_mode_never_calls_spira_lc_when_off() {
    let mut w = Fake {
        lc_forbidden: true,
        ..Fake::default()
    }
    .bead("sp-a", "open", None, &[]);
    let (code, _) = run(&opts(true, false), &[cand("sp-a", true, "")], &mut w);
    assert_eq!(code, 0);
}

#[test]
fn live_work_refuses_and_touches_nothing() {
    let mut w = Fake::default().bead("sp-s", "in_progress", Some("aeon-1"), &["spira-poison"]);
    let (code, out) = run(&opts(true, false), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 3);
    assert!(out.contains("held by aeon-1 (in_progress)"), "{out}");
    assert!(w.notes.is_empty());
    assert!(w.removed_labels.is_empty());
}

/// sp-mve9i: on, bd's in_progress is not the claim — only the lifecycle row's holder is.
#[test]
fn on_bd_in_progress_without_a_working_row_is_not_held() {
    use lifecycle::bead::{BeadState, HoldKind};
    let mut w = Fake::default().bead("sp-s", "in_progress", Some("aeon-1"), &[]).lc("sp-s", BeadState::Ready, &[HoldKind::Poison], None);
    let (code, out) = run(&opts(false, true), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("WOULD    sp-s"), "{out}");
}

#[test]
fn lifecycle_working_holder_is_also_held() {
    use lifecycle::bead::{BeadState, HoldKind};
    let mut w = Fake::default().bead("sp-s", "open", None, &[]).lc(
        "sp-s",
        BeadState::Working,
        &[HoldKind::Poison],
        Some("aeon-2"),
    );
    let (code, out) = run(&opts(true, true), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 3);
    assert!(out.contains("held by aeon-2 (lifecycle WORKING)"), "{out}");
}

#[test]
fn unhold_refused_once_then_retried_from_a_fresh_read() {
    use lifecycle::bead::{BeadState, HoldKind};
    let mut w = Fake::default().bead("sp-s", "open", None, &[]).lc(
        "sp-s",
        BeadState::Ready,
        &[HoldKind::Poison],
        None,
    );
    w.refuse_unhold_times = 1;
    let (code, out) = run(&opts(true, true), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("RESTORED sp-s"), "{out}");
    assert!(!w.lc["sp-s"].holds.contains(&HoldKind::Poison));
}

#[test]
fn still_poisoned_after_apply_is_refused() {
    // remove_label is a no-op double of what really happened: the fake bead keeps the label,
    // so verify still finds it and the run must not report success.
    struct Stuck(Fake);
    impl World for Stuck {
        fn bead(&mut self, id: &str) -> Result<Option<BeadRecord>, String> {
            World::bead(&mut self.0, id)
        }
        fn events(&mut self, id: &str) -> Result<Vec<crate::events::EventRow>, String> {
            World::events(&mut self.0, id)
        }
        fn lc_row(&mut self, id: &str) -> Result<Option<LcRow>, String> {
            World::lc_row(&mut self.0, id)
        }
        fn lc_unhold_poison(&mut self, id: &str, row: &LcRow, actor: &str) -> LcApply {
            World::lc_unhold_poison(&mut self.0, id, row, actor)
        }
        fn write_event(&mut self, id: &str, t: &str, v: &str) -> Result<(), String> {
            World::write_event(&mut self.0, id, t, v)
        }
        fn clear_ask_history(&mut self, id: &str) -> Result<(), String> {
            World::clear_ask_history(&mut self.0, id)
        }
        fn ask_history_exists(&mut self, id: &str) -> bool {
            World::ask_history_exists(&mut self.0, id)
        }
        fn remove_label(&mut self, _id: &str, _label: &str) -> Result<(), String> {
            Ok(()) // pretend to remove it, but never actually do it — the stuck case
        }
        fn note(&mut self, id: &str, t: &str) -> Result<(), String> {
            World::note(&mut self.0, id, t)
        }
        fn open_asks(&mut self) -> Result<Vec<crate::unpoison::AskRow>, String> {
            World::open_asks(&mut self.0)
        }
        fn close(&mut self, id: &str, r: &str) -> Result<(), String> {
            World::close(&mut self.0, id, r)
        }
        fn audit_len(&mut self) -> u64 {
            World::audit_len(&mut self.0)
        }
        fn audit_read_from(&mut self, o: u64) -> Result<Vec<u8>, String> {
            World::audit_read_from(&mut self.0, o)
        }
        fn now(&mut self) -> i64 {
            World::now(&mut self.0)
        }
        fn sleep(&mut self, s: u64) {
            World::sleep(&mut self.0, s)
        }
        fn mark_poison_lifted(&mut self, id: &str, attempts: u32) -> Result<(), String> {
            World::mark_poison_lifted(&mut self.0, id, attempts)
        }
    }
    let mut w = Stuck(Fake::default().bead("sp-s", "open", None, &["spira-poison"]));
    let (code, out) = run(&opts(true, false), &[cand("sp-s", true, "")], &mut w);
    assert_eq!(code, 3);
    assert!(
        out.contains("REFUSED  sp-s: the poison hold would not come off"),
        "{out}"
    );
}

#[test]
fn several_candidates_one_kept_one_restored_summary_counts_both() {
    let mut w = Fake::default()
        .bead("sp-k", "open", None, &["spira-poison"])
        .bead("sp-r", "open", None, &["spira-poison"]);
    let cands = vec![cand("sp-k", false, "no branch"), cand("sp-r", true, "")];
    let (code, out) = run(&opts(true, false), &cands, &mut w);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("KEEP     sp-k"), "{out}");
    assert!(out.contains("RESTORED sp-r"), "{out}");
    assert!(
        out.contains("--- 2 poisoned bead(s) examined, 1 finished and landable, 1 restored"),
        "{out}"
    );
}

#[test]
fn parse_candidates_reads_a_json_array() {
    let v = parse_candidates(
        r#"[{"id":"sp-a","ok":true,"why":"","branch":"spira/sp-a","base":"origin/main"}]"#,
    )
    .unwrap();
    assert_eq!(v, vec![cand("sp-a", true, "")]);
    assert_eq!(parse_candidates("").unwrap(), Vec::new());
    assert_eq!(parse_candidates("null").unwrap(), Vec::new());
    assert!(
        parse_candidates(r#"[{"ok":true}]"#).is_err(),
        "a row without an id is rejected"
    );
}
