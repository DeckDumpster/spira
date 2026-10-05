//! Family H — queue waiters (DESIGN.md §4, CHECK 3b). Ported from lib.sh
//! `mark_queue_waiters`/`close_landed_queue_waiters` (wave 4.28, sp-fbqsv). PERMANENT: the
//! wave 4 lifecycle-flip plan keeps this family even once lc.sh's own calls are gone
//! (stacked dependents still read the queue-wait label) — see
//! `wiki/projects/spira/designs/stacked-dependents-2026-09-28.md`.
//!
//! `mark_queue_waiters` applies/removes `SPIRA_QUEUE_WAIT_LABEL` on a ready bead whose
//! closed blocker is CERTIFIED/BATCHED (in the queue pipeline, not yet LANDED) — bd
//! considers a closed dep resolved, but queue mode's CLOSED is not LANDED. The decision is
//! one pass over the union of the labeled set and the ready set (never release-then-apply,
//! which could clear for one blocker and reapply for another in the same run).
//! `close_landed_queue_waiters` closes a labeled bead whose lifecycle row already reads LANDED
//! — it never got a branch to land, so the normal close-on-land path never visited it.
//!
//! Both are dual-written (bd label AND the spira-lc `wait` hold, sp-ki12s precedent):
//! `fayth_ready` still reads the label, not the hold, until CHECK 3b's reader cuts over.

use std::collections::HashSet;

use crate::host::{Io, Spec};
use crate::model::{parse_beads, Bead, LcRow};
use crate::pass::Sentinel;
use crate::store;

/// Active queue blockers: spira-lc rows in CERTIFIED or IN_DELIVERY — the queue pipeline's
/// states. A tipless design/diagnosis bead reaches a terminal state, never these.
pub fn active_blockers_lc(rows: &[LcRow]) -> HashSet<String> {
    rows.iter()
        .filter(|r| matches!(r.state.as_str(), "CERTIFIED" | "IN_DELIVERY"))
        .map(|r| r.bead_id.clone())
        .collect()
}

/// One decision per ready bead (add when it blocks on an active blocker and is not yet
/// labeled, remove when it no longer does but is), plus an unconditional remove for every
/// labeled id absent from the ready set (lib.sh: "ready dep data is authoritative; a
/// labeled-only bead gets no dep check — removing the label is safe since the bead is not
/// claimable"). `true` = add, `false` = remove.
pub fn decide(active: &HashSet<String>, labeled: &HashSet<String>, ready: &[Bead]) -> Vec<(bool, String)> {
    let mut by_id: std::collections::HashMap<&str, &Bead> = std::collections::HashMap::new();
    for b in ready {
        by_id.insert(b.id.as_str(), b);
    }
    let mut out = Vec::new();
    for (id, b) in &by_id {
        let currently = labeled.contains(*id);
        let want = !active.is_empty()
            && b.dependencies.iter().any(|d| {
                d.r#type.as_deref() == Some("blocks")
                    && d.target().map(|t| active.contains(t)).unwrap_or(false)
            });
        if want && !currently {
            out.push((true, id.to_string()));
        } else if !want && currently {
            out.push((false, id.to_string()));
        }
    }
    for id in labeled {
        if !by_id.contains_key(id.as_str()) {
            out.push((false, id.clone()));
        }
    }
    out
}

impl<'a> Sentinel<'a> {
    /// `spira-lc hold <id> <kind> <reason> sentinel` — fire-and-forget, dual-written
    /// alongside the bd label (sp-ki12s precedent): never gated on `self.lc`, unlike the
    /// CAS-based holds in lifecycle.rs, because `spira-lc hold` answers "cannot tell" on its
    /// own, the same `|| true` shape lib.sh used.
    pub fn lc_hold(&self, id: &str, kind: &str, reason: &str) {
        self.h.run(
            Spec::args_owned(
                self.cfg.lc_bin.clone(),
                vec![
                    "hold".into(),
                    id.into(),
                    kind.into(),
                    reason.into(),
                    "sentinel".into(),
                ],
            )
            .out(Io::Null)
            .err(Io::Null),
        );
    }

    /// `spira-lc unhold <id> <kind> sentinel`.
    pub fn lc_unhold(&self, id: &str, kind: &str) {
        self.h.run(
            Spec::args_owned(
                self.cfg.lc_bin.clone(),
                vec!["unhold".into(), id.into(), kind.into(), "sentinel".into()],
            )
            .out(Io::Null)
            .err(Io::Null),
        );
    }

    /// lib.sh `mark_queue_waiters`. `broad_ready` is the pass's own unscoped ready
    /// snapshot (`ready_raw_args`) when running inside a full pass; `None` for a standalone
    /// `sentinel --mark-queue-waiters` invocation, which falls back to
    /// `$SPIRA_READY_SNAPSHOT` (re-narrowed to `SPIRA_SCOPE_LABEL`, exactly as the snapshot
    /// path always did) and then a live `bd ready` call.
    pub fn mark_queue_waiters(&self, broad_ready: Option<&[Bead]>) {
        let label = self.cfg.queue_wait.clone();
        if label.is_empty() {
            return;
        }
        let Some(rows) = self.lc_rows() else {
            return;
        };
        let active = active_blockers_lc(&rows);

        let labeled: HashSet<String> = {
            let args = vec![
                "list".into(),
                "--status".into(),
                "open".into(),
                "--label".into(),
                label.clone(),
                "--limit".into(),
                "0".into(),
            ];
            self.bd()
                .json(self.h, &args)
                .ok()
                .and_then(|s| parse_beads(&s).ok())
                .map(|v| v.into_iter().map(|b| b.id).collect())
                .unwrap_or_default()
        };

        // qblockers empty: skip the ready query entirely (lib.sh does the same), which
        // leaves `ready` empty and therefore removes every currently-labeled bead below —
        // nothing is an active blocker, so nothing should be held.
        let ready: Vec<Bead> = if active.is_empty() {
            Vec::new()
        } else {
            self.queue_wait_ready(broad_ready)
        };

        for (add, id) in decide(&active, &labeled, &ready) {
            if add {
                self.bd().quiet(self.h, &["label", "add", &id, &label], None);
                self.lc_hold(&id, "wait", "blocker certified or batched, not yet landed");
                self.log(&format!("mark_queue_waiters: {id} — queue-wait applied"));
            } else {
                self.bd().quiet(self.h, &["label", "remove", &id, &label], None);
                self.lc_unhold(&id, "wait");
                self.log(&format!("mark_queue_waiters: {id} — blocker landed, cleared"));
            }
        }
    }

    fn queue_wait_ready(&self, broad_ready: Option<&[Bead]>) -> Vec<Bead> {
        let scope_narrow = |mut v: Vec<Bead>| -> Vec<Bead> {
            if !self.cfg.scope.is_empty() {
                v.retain(|b| b.has(&self.cfg.scope));
            }
            v
        };
        if let Some(r) = broad_ready {
            return scope_narrow(r.to_vec());
        }
        match std::env::var("SPIRA_READY_SNAPSHOT") {
            Ok(p) if !p.is_empty() => match std::fs::read_to_string(&p) {
                Ok(text) => scope_narrow(parse_beads(&text).unwrap_or_default()),
                Err(_) => self.live_ready(),
            },
            _ => self.live_ready(),
        }
    }

    fn live_ready(&self) -> Vec<Bead> {
        self.bd()
            .json(self.h, &store::ready_args(&self.cfg))
            .ok()
            .and_then(|s| parse_beads(&s).ok())
            .unwrap_or_default()
    }

    /// lib.sh `close_landed_queue_waiters`.
    pub fn close_landed_queue_waiters(&self) {
        let label = self.cfg.queue_wait.clone();
        if label.is_empty() {
            return;
        }
        let args = vec![
            "list".into(),
            "--status".into(),
            "open".into(),
            "--label".into(),
            label.clone(),
            "--limit".into(),
            "0".into(),
        ];
        let ids: Vec<String> = match self.bd().json(self.h, &args) {
            Ok(s) => parse_beads(&s)
                .map(|v| v.into_iter().map(|b| b.id).collect())
                .unwrap_or_default(),
            Err(_) => return,
        };
        if ids.is_empty() {
            return;
        }
        let Some(rows) = self.lc_rows() else {
            return;
        };
        for id in ids {
            if rows.iter().any(|r| r.bead_id == id && r.state == "LANDED") {
                self.bd().quiet(self.h, &["label", "remove", &id, &label], None);
                self.lc_unhold(&id, "wait");
                self.bd().quiet(
                    self.h,
                    &[
                        "close",
                        &id,
                        "--reason",
                        "Content already on main (LANDED); no branch remained to land.",
                    ],
                    None,
                );
                self.log(&format!("close_landed_queue_waiters: {id} — closed (LANDED, no branch)"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    fn bead(id: &str, deps: &[(&str, &str)]) -> Bead {
        let d = deps
            .iter()
            .map(|(t, k)| format!(r#"{{"depends_on_id":"{t}","type":"{k}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        parse_beads(&format!(r#"{{"id":"{id}","dependencies":[{d}]}}"#))
            .unwrap()
            .remove(0)
    }

    fn lc_row(id: &str, state: &str) -> LcRow {
        LcRow { bead_id: id.into(), state: state.into(), ..Default::default() }
    }

    #[test]
    fn active_blockers_lc_takes_only_the_queue_pipeline_states() {
        let rows = vec![
            lc_row("cert", "CERTIFIED"),
            lc_row("deliv", "IN_DELIVERY"),
            lc_row("landed", "LANDED"),
            lc_row("sub", "SUBMITTED"),
            lc_row("sup", "SUPERSEDED"),
        ];
        let got = active_blockers_lc(&rows);
        assert_eq!(got, ["cert".to_string(), "deliv".to_string()].into());
        assert!(active_blockers_lc(&[]).is_empty());
    }

    #[test]
    fn decide_adds_once_and_never_flip_flops_in_one_pass() {
        let active: HashSet<String> = ["blocker".to_string()].into();
        let labeled: HashSet<String> = HashSet::new();
        let ready = vec![bead("dependent", &[("blocker", "blocks")])];
        assert_eq!(decide(&active, &labeled, &ready), vec![(true, "dependent".to_string())]);
    }

    #[test]
    fn decide_removes_when_blocker_no_longer_active_even_with_a_second_tipless_blocker() {
        // The two-blocker case (assertions 12-14): a real blocker lands (drops out of
        // `active`) while a tipless design blocker stays CERTIFIED forever. The label must
        // clear in this one pass, not clear-then-reapply for the tipless blocker.
        let active: HashSet<String> = HashSet::new(); // the real blocker landed; the design one was never active
        let labeled: HashSet<String> = ["dependent".to_string()].into();
        let ready = vec![bead("dependent", &[("blocker-real", "blocks"), ("blocker-design", "blocks")])];
        assert_eq!(decide(&active, &labeled, &ready), vec![(false, "dependent".to_string())]);
    }

    #[test]
    fn decide_removes_a_labeled_bead_absent_from_the_ready_set() {
        let active: HashSet<String> = ["blocker".to_string()].into();
        let labeled: HashSet<String> = ["gone".to_string()].into();
        assert_eq!(decide(&active, &labeled, &[]), vec![(false, "gone".to_string())]);
    }

    #[test]
    fn decide_is_idempotent_once_applied() {
        let active: HashSet<String> = ["blocker".to_string()].into();
        let labeled: HashSet<String> = ["dependent".to_string()].into();
        let ready = vec![bead("dependent", &[("blocker", "blocks")])];
        assert!(decide(&active, &labeled, &ready).is_empty());
    }

    #[test]
    fn decide_ignores_a_parent_child_dependency_on_an_active_blocker() {
        let active: HashSet<String> = ["blocker".to_string()].into();
        let labeled: HashSet<String> = HashSet::new();
        let ready = vec![bead("dependent", &[("blocker", "parent-child")])];
        assert!(decide(&active, &labeled, &ready).is_empty());
    }
}
