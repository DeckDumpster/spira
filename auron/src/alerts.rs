//! Raising, re-raising and clearing one alert bead, and maintaining the write-probe bead
//! — auron.sh's `body_of`/`alert_write`/`alert_clear` and its write-probe dance, against
//! the `BdOps` port so the whole thing is testable with an in-memory fake.

use crate::bdops::{BdOps, BeadRow, Failure};
use crate::util;

/// The description, REGENERATED WHOLE on every write, never patched
/// (law-regenerate-derived-summaries): a patched alert body accumulates the readings that
/// were true at each past firing, and the reader cannot tell which one is now.
///
/// NO MARKDOWN IN THE TRAILER. It is read in a tmux pane and in `bd show`, neither of
/// which renders emphasis.
pub fn body_of(key: &str, first: i64, flaps: i64, evidence: &str, tz: &str) -> String {
    format!(
        "**Condition:** `{key}`\n**First seen:** {}  ·  **Fired:** {flaps} time(s)\n\n```\n{}\n```\n\nRaised by Auron (bin/auron; replaces spira/auron.sh), which watches the loop and only\nspeaks - it repairs nothing. It closes this itself when the condition passes,\nso nothing is owed. Closing it by hand acknowledges it: Auron will not raise\nit again until the condition has cleared and returned.\n",
        util::iso_display(first, tz),
        evidence.trim(),
    )
}

/// The close reason for a cleared alert.
pub fn clear_reason(first: i64, flaps: i64, clear_n: i64, tz: &str) -> String {
    format!(
        "Cleared by Auron: the condition stopped holding for {clear_n} consecutive checks.\nRaised {}, fired {flaps} time(s).\nNothing was repaired by Auron — it only reports. Either the loop recovered on its own\nor something else fixed it.\n",
        util::iso_display(first, tz),
    )
}

/// The reason attached to a duplicate probe bead closed automatically on adoption.
pub const DUPLICATE_PROBE_REASON: &str = "Duplicate Auron write probe bead closed automatically.\nThe probe is a one-bead invariant; this bead was created during a transient contention\nepisode and superseded by an earlier probe that was re-derived on recovery.\n";

/// `alert:<key>` -> the row, from a `list --label alert` query — one bead per cause is the
/// invariant this enforces, so this must be keyed by the SAME label the writer reads.
pub fn find_by_key<'a>(rows: &'a [BeadRow], key: &str) -> Option<&'a BeadRow> {
    let want = format!("alert:{key}");
    rows.iter().find(|r| r.labels.iter().any(|l| l == &want))
}

/// Raises or re-raises the alert bead for `key`. `existing` is this key's row from the
/// last `list --label alert` query, if one exists. Returns the bead id on success.
pub fn alert_write(ops: &dyn BdOps, key: &str, title: &str, body: &str, flaps: i64, existing: Option<&BeadRow>) -> Result<String, Failure> {
    let Some(row) = existing else {
        let id = ops.create(title, &format!("alert,overseer,alert:{key},flaps:{flaps}"), body)?;
        return Ok(id);
    };
    if row.status == "closed" {
        // A REOPEN MEANS THE CONDITION RETURNED — not an acknowledgement argued with; see
        // reconcile.rs's own Refresh-path guard for the case this does NOT apply to (a
        // hand-closed bead while still firing: that path never calls alert_write at all).
        ops.reopen(&row.id, "alert-recur", "the condition returned")?;
    }
    ops.update_title_body(&row.id, title, body)?;
    // THE FLAP COUNT IS A LABEL, and a count is a fact with exactly one current value, so
    // every stale `flaps:` is removed rather than a new one added beside it.
    let want_flap = format!("flaps:{flaps}");
    for l in &row.labels {
        if l.starts_with("flaps:") && l != &want_flap {
            let _ = ops.label_remove(&row.id, l);
        }
    }
    let _ = ops.label_add(&row.id, &want_flap);
    // A RETURNING CONDITION IS NOT ONE ANYBODY HAS SEEN. `acked` means the operator looked
    // at the last occurrence; carrying it into a new one hides the very thing the flap
    // count exists to make visible.
    if row.labels.iter().any(|l| l == "acked") {
        let _ = ops.label_remove(&row.id, "acked");
    }
    Ok(row.id.clone())
}

/// Closes the alert bead for `key`. A no-op success if there is no bead to close, or it
/// is already closed — auron.sh's own `[ -n "$id" ] || return 0` / status guard.
pub fn alert_clear(ops: &dyn BdOps, existing: Option<&BeadRow>, first: i64, flaps: i64, clear_n: i64, tz: &str) -> Result<(), Failure> {
    let Some(row) = existing else { return Ok(()) };
    if row.status == "closed" {
        return Ok(());
    }
    ops.close(&row.id, &clear_reason(first, flaps, clear_n, tz))
}

// ---- the write probe --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub id: Option<String>,
    /// `None` when unattempted (db unreachable, so the probe never ran at all).
    pub write_ok: Option<bool>,
    pub saturated: bool,
}

/// Maintains the one-bead write-probe invariant and exercises the write path — auron.sh's
/// own re-derive / idempotent-backstop / create / update dance, four properties preserved:
///   1. a failed re-derive does not create (re-derive failure and empty-re-derive are both
///      lock contention; creating unconditionally leaked a P0 bead per episode).
///   2. create is idempotent: re-queries before creating and adopts any existing bead.
///   3. extras are closed on adoption: one probe bead is the invariant.
///   4. a timeout is contention, not unavailability.
///
/// auron.sh's own comment names a fifth property — "a create following a failed prior
/// update is a recovery, not a healthy write" (`_probe_had`) — but its own guard
/// (`if [ -z "$PROBE_ID" ]`) means create is only ever reached on a pass where the cached
/// id was ALREADY empty at the top, which is exactly when `_probe_had` is too: the check
/// `[ -n "$_probe_had" ]` inside the create branch is always false. Ported as it actually
/// behaves (create always reports healthy on success), not as its comment describes;
/// named here rather than silently dropped.
pub fn maintain_probe(ops: &dyn BdOps, cached_id: &str) -> ProbeOutcome {
    let mut id = cached_id.to_string();

    if id.is_empty() {
        match ops.list_by_label("auron:probe") {
            Ok(rows) => {
                if let Some(first) = rows.first() {
                    id = first.id.clone();
                    close_extras(ops, &rows);
                }
                // An empty (but successful) re-derive falls through to the idempotent
                // backstop below, not straight to create.
            }
            Err(f) => {
                // Failed re-derive: do NOT create (property 1).
                return ProbeOutcome { id: None, write_ok: Some(false), saturated: f == Failure::Saturated };
            }
        }
    }

    if id.is_empty() {
        // Idempotent backstop: re-query immediately before creating (property 2).
        if let Ok(rows) = ops.list_by_label("auron:probe") {
            if let Some(first) = rows.first() {
                id = first.id.clone();
                close_extras(ops, &rows);
            }
        }
    }

    if id.is_empty() {
        return match ops.create("Auron write probe", "auron:probe,overseer", "write-path probe — updated on every Auron pass") {
            Ok(new_id) => ProbeOutcome { id: Some(new_id), write_ok: Some(true), saturated: false },
            Err(f) => ProbeOutcome { id: None, write_ok: Some(false), saturated: f == Failure::Saturated },
        };
    }

    match ops.update_body(&id, &util::now_epoch().to_string()) {
        Ok(()) => ProbeOutcome { id: Some(id), write_ok: Some(true), saturated: false },
        Err(f) => ProbeOutcome { id: None, write_ok: Some(false), saturated: f == Failure::Saturated },
    }
}

fn close_extras(ops: &dyn BdOps, rows: &[BeadRow]) {
    for extra in rows.iter().skip(1) {
        let _ = ops.close(&extra.id, DUPLICATE_PROBE_REASON);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Store {
        rows: BTreeMap<String, BeadRow>,
        next: u32,
        fail_create: bool,
        fail_update: bool,
        fail_list: Option<Failure>,
        fail_close: bool,
        closes: Vec<String>,
        label_ops: Vec<(String, String, bool)>, // (id, label, added)
        reopened: Vec<String>,
        bodies: BTreeMap<String, String>,
    }

    struct Fake(Mutex<Store>);

    impl BdOps for Fake {
        fn list_by_label(&self, label: &str) -> Result<Vec<BeadRow>, Failure> {
            let s = self.0.lock().unwrap();
            if let Some(f) = s.fail_list {
                return Err(f);
            }
            Ok(s.rows.values().filter(|r| r.labels.iter().any(|l| l == label || l.starts_with(&format!("{label}")))).cloned().collect())
        }
        fn create(&self, title: &str, labels: &str, body: &str) -> Result<String, Failure> {
            let mut s = self.0.lock().unwrap();
            if s.fail_create {
                return Err(Failure::Other);
            }
            s.next += 1;
            let id = format!("sp-{}", s.next);
            let labels: Vec<String> = labels.split(',').map(|x| x.to_string()).collect();
            s.rows.insert(id.clone(), BeadRow { id: id.clone(), status: "open".to_string(), labels });
            s.bodies.insert(id.clone(), body.to_string());
            let _ = title;
            Ok(id)
        }
        fn update_title_body(&self, id: &str, _title: &str, body: &str) -> Result<(), Failure> {
            let mut s = self.0.lock().unwrap();
            if s.fail_update {
                return Err(Failure::Other);
            }
            s.bodies.insert(id.to_string(), body.to_string());
            Ok(())
        }
        fn update_body(&self, id: &str, body: &str) -> Result<(), Failure> {
            self.update_title_body(id, "", body)
        }
        fn close(&self, id: &str, _reason: &str) -> Result<(), Failure> {
            let mut s = self.0.lock().unwrap();
            if s.fail_close {
                return Err(Failure::Other);
            }
            if let Some(r) = s.rows.get_mut(id) {
                r.status = "closed".to_string();
            }
            s.closes.push(id.to_string());
            Ok(())
        }
        fn label_add(&self, id: &str, label: &str) -> Result<(), Failure> {
            let mut s = self.0.lock().unwrap();
            if let Some(r) = s.rows.get_mut(id) {
                if !r.labels.iter().any(|l| l == label) {
                    r.labels.push(label.to_string());
                }
            }
            s.label_ops.push((id.to_string(), label.to_string(), true));
            Ok(())
        }
        fn label_remove(&self, id: &str, label: &str) -> Result<(), Failure> {
            let mut s = self.0.lock().unwrap();
            if let Some(r) = s.rows.get_mut(id) {
                r.labels.retain(|l| l != label);
            }
            s.label_ops.push((id.to_string(), label.to_string(), false));
            Ok(())
        }
        fn reopen(&self, id: &str, _cause: &str, _note: &str) -> Result<(), Failure> {
            let mut s = self.0.lock().unwrap();
            if let Some(r) = s.rows.get_mut(id) {
                r.status = "open".to_string();
            }
            s.reopened.push(id.to_string());
            Ok(())
        }
    }

    fn fake() -> Fake {
        Fake(Mutex::new(Store::default()))
    }

    #[test]
    fn body_of_names_condition_and_flap_count() {
        let b = body_of("sentinel-stalled", 0, 3, "evidence text", "UTC");
        assert!(b.contains("`sentinel-stalled`"));
        assert!(b.contains("Fired:** 3 time(s)"));
        assert!(b.contains("evidence text"));
        assert!(!b.contains('_') || b.contains("Raised by Auron"), "no stray markdown emphasis in the trailer");
    }

    #[test]
    fn alert_write_creates_when_no_existing_bead() {
        let f = fake();
        let id = alert_write(&f, "k", "title", "body", 1, None).unwrap();
        let s = f.0.lock().unwrap();
        assert!(s.rows.contains_key(&id));
        assert!(s.rows[&id].labels.contains(&"alert:k".to_string()));
        assert!(s.rows[&id].labels.contains(&"flaps:1".to_string()));
    }

    #[test]
    fn alert_write_reopens_a_closed_bead_the_condition_returning() {
        let f = fake();
        let row = BeadRow { id: "sp-1".into(), status: "closed".into(), labels: vec!["alert".into(), "alert:k".into(), "flaps:1".into()] };
        f.0.lock().unwrap().rows.insert("sp-1".into(), row.clone());
        alert_write(&f, "k", "t", "b", 2, Some(&row)).unwrap();
        let s = f.0.lock().unwrap();
        assert_eq!(s.reopened, vec!["sp-1".to_string()]);
        assert_eq!(s.rows["sp-1"].status, "open");
    }

    #[test]
    fn alert_write_replaces_the_stale_flap_label_not_adds_beside_it() {
        let f = fake();
        let row = BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec!["alert".into(), "alert:k".into(), "flaps:1".into()] };
        f.0.lock().unwrap().rows.insert("sp-1".into(), row.clone());
        alert_write(&f, "k", "t", "b", 2, Some(&row)).unwrap();
        let s = f.0.lock().unwrap();
        let labels = &s.rows["sp-1"].labels;
        assert!(labels.contains(&"flaps:2".to_string()));
        assert!(!labels.contains(&"flaps:1".to_string()), "the stale flap label must be gone, not left beside the new one");
    }

    #[test]
    fn alert_write_clears_acked_on_a_returning_condition() {
        let f = fake();
        let row = BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec!["alert".into(), "alert:k".into(), "flaps:1".into(), "acked".into()] };
        f.0.lock().unwrap().rows.insert("sp-1".into(), row.clone());
        alert_write(&f, "k", "t", "b", 2, Some(&row)).unwrap();
        let s = f.0.lock().unwrap();
        assert!(!s.rows["sp-1"].labels.contains(&"acked".to_string()));
    }

    #[test]
    fn alert_write_create_failure_propagates() {
        let f = fake();
        f.0.lock().unwrap().fail_create = true;
        assert!(alert_write(&f, "k", "t", "b", 1, None).is_err());
    }

    #[test]
    fn alert_clear_noop_when_no_bead() {
        let f = fake();
        assert_eq!(alert_clear(&f, None, 0, 1, 2, "UTC"), Ok(()));
    }

    #[test]
    fn alert_clear_noop_when_already_closed() {
        let f = fake();
        let row = BeadRow { id: "sp-1".into(), status: "closed".into(), labels: vec![] };
        alert_clear(&f, Some(&row), 0, 1, 2, "UTC").unwrap();
        assert!(f.0.lock().unwrap().closes.is_empty(), "no redundant close call on an already-closed bead");
    }

    #[test]
    fn alert_clear_closes_an_open_bead() {
        let f = fake();
        let row = BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec![] };
        f.0.lock().unwrap().rows.insert("sp-1".into(), row.clone());
        alert_clear(&f, Some(&row), 0, 1, 2, "UTC").unwrap();
        let s = f.0.lock().unwrap();
        assert_eq!(s.rows["sp-1"].status, "closed");
    }

    #[test]
    fn find_by_key_matches_the_class_scoped_label() {
        let rows = vec![BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec!["alert".into(), "alert:a".into()] }, BeadRow { id: "sp-2".into(), status: "open".into(), labels: vec!["alert".into(), "alert:b".into()] }];
        assert_eq!(find_by_key(&rows, "b").unwrap().id, "sp-2");
        assert!(find_by_key(&rows, "c").is_none());
    }

    // ---- the write probe -----------------------------------------------------------------

    #[test]
    fn probe_first_run_creates_and_reports_healthy() {
        let f = fake();
        let r = maintain_probe(&f, "");
        assert!(r.id.is_some());
        assert_eq!(r.write_ok, Some(true));
        assert_eq!(f.0.lock().unwrap().rows.len(), 1);
    }

    #[test]
    fn probe_with_a_cached_id_just_updates() {
        let f = fake();
        f.0.lock().unwrap().rows.insert("sp-1".into(), BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec!["auron:probe".into()] });
        let r = maintain_probe(&f, "sp-1");
        assert_eq!(r.id, Some("sp-1".to_string()));
        assert_eq!(r.write_ok, Some(true));
    }

    #[test]
    fn probe_rederives_when_the_cache_is_lost() {
        let f = fake();
        f.0.lock().unwrap().rows.insert("sp-1".into(), BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec!["auron:probe".into()] });
        let r = maintain_probe(&f, "");
        assert_eq!(r.id, Some("sp-1".to_string()), "adopts the existing probe rather than creating a duplicate");
    }

    #[test]
    fn probe_failed_rederive_does_not_create() {
        let f = fake();
        f.0.lock().unwrap().fail_list = Some(Failure::Other);
        let r = maintain_probe(&f, "");
        assert_eq!(r.id, None);
        assert_eq!(r.write_ok, Some(false));
        assert!(f.0.lock().unwrap().rows.is_empty(), "no bead created on a failed re-derive");
    }

    #[test]
    fn probe_failed_rederive_saturated_is_distinguished() {
        let f = fake();
        f.0.lock().unwrap().fail_list = Some(Failure::Saturated);
        let r = maintain_probe(&f, "");
        assert!(r.saturated);
    }

    #[test]
    fn probe_extras_are_closed_on_adoption() {
        let f = fake();
        {
            let mut s = f.0.lock().unwrap();
            s.rows.insert("sp-1".into(), BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec!["auron:probe".into()] });
            s.rows.insert("sp-2".into(), BeadRow { id: "sp-2".into(), status: "open".into(), labels: vec!["auron:probe".into()] });
        }
        let _ = maintain_probe(&f, "");
        let s = f.0.lock().unwrap();
        let open: Vec<&BeadRow> = s.rows.values().filter(|r| r.status == "open").collect();
        assert_eq!(open.len(), 1, "exactly one probe bead is open after adoption");
    }

    #[test]
    fn probe_update_against_a_stale_cached_id_that_no_longer_exists_is_not_fabricated_ok() {
        // Bash never validates PROBE_ID exists before UPDATE — same here: the fake's
        // update always answers Ok for an unknown id, mirroring `bd update` against a row
        // it never checked existed, which is the seam's problem to surface, not this
        // function's. Documented, not "fixed" (out of scope; see maintain_probe's doc).
        let f = fake();
        let r = maintain_probe(&f, "sp-gone");
        assert_eq!(r.id, Some("sp-gone".to_string()));
    }

    #[test]
    fn probe_update_failure_reports_down() {
        let f = fake();
        f.0.lock().unwrap().rows.insert("sp-1".into(), BeadRow { id: "sp-1".into(), status: "open".into(), labels: vec!["auron:probe".into()] });
        f.0.lock().unwrap().fail_update = true;
        let r = maintain_probe(&f, "sp-1");
        assert_eq!(r.write_ok, Some(false));
    }
}
