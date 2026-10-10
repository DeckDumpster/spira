//! Everything the sentinel reads from or writes to `spira_lifecycle` (DESIGN.md §1, "The
//! lifecycle machine"): CHECK 2's wait hold and stale-lease reap, CHECK 2c's consistency
//! sweep, and CHECK 4's poison hold. sp-i2m7y moved these onto spira-lc unconditionally;
//! there is no bd-label mode to fall back to, and no off mode (sp-v62vn).

use std::collections::HashSet;

use serde_json::Value;

use crate::host::{Io, Spec};
use crate::model::{parse_lc_rows, LcRow};
use crate::pass::Sentinel;
use crate::store::Snapshot;
use spira_config::lc_state;
use spira_config::nonwork::{self, Kind};

/// The hold kind's serde tag and implied cause (spira-lc callers.rs `hold_cause`; lc.sh's
/// `_lc_hold_kind_tag` / `_lc_hold_kind_cause` before sp-arpjt).
pub fn hold_tag(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "poison" => ("Poison", "attempts-exhausted"),
        "ask" => ("Ask", "operator-question"),
        "wait" => ("Wait", "unlanded-blocker"),
        "manual" => ("Manual", "manual-hold"),
        _ => return None,
    })
}

pub fn hold_event(kind: &str, detail: &str) -> Option<String> {
    let (tag, cause) = hold_tag(kind)?;
    Some(format!(
        "{{\"Hold\":{{\"kind\":\"{tag}\",\"cause\":\"{cause}\",\"detail\":{}}}}}",
        Value::String(detail.to_string())
    ))
}

pub fn unhold_event(kind: &str) -> Option<String> {
    let (tag, _) = hold_tag(kind)?;
    Some(format!("{{\"Unhold\":{{\"kind\":\"{tag}\"}}}}"))
}

pub const HOLDER_DEAD: &str = "\"HolderDead\"";

/// CHECK 2 (protect-waiting half): which WORKING beads to wait-hold or release. `held` is
/// the wait-held set; each bead's dependency facts come from the snapshot join.
///
/// Every state here is the lifecycle machine's (design §3.4, sp-mve9i): the candidates are
/// the WORKING rows, and a work dependency is resolved once its row is past the builder
/// (what bd `closed` meant). An ask is not a work bead — its bd status is its only state —
/// so an ask dependency is resolved when bd closes it (`spira_config::nonwork`). A work
/// dependency with no lifecycle row counts as unresolved: it blocks the wait hold, which
/// leaves the bead to the reaper exactly as an open non-ask dependency always did.
pub fn wait_decisions(
    snap: &crate::store::Snapshot,
    rows: &[LcRow],
    held: &HashSet<String>,
    ask: &str,
) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    for r in rows.iter().filter(|r| lc_state::is_working(&r.state)) {
        let Some(b) = snap.get(&r.bead_id) else { continue };
        let has_skip = held.contains(&b.id);
        if !(has_skip || b.dependency_count > 0) {
            continue;
        }
        let deps: Vec<(String, Vec<String>, bool)> = b
            .dependencies
            .iter()
            .filter_map(|d| {
                let t = d.target()?;
                let (bd_status, labels) = match snap.get(t) {
                    Some(x) => (x.status.clone(), x.labels.clone()),
                    None => (
                        d.status.clone().unwrap_or_default(),
                        d.labels.clone().unwrap_or_default(),
                    ),
                };
                let is_ask = !ask.is_empty() && labels.iter().any(|x| x == ask);
                let resolved = if is_ask {
                    nonwork::is_closed(Kind::Ask, &bd_status)
                } else {
                    snap.lc_past_builder(t)
                };
                if resolved {
                    return None;
                }
                Some((t.to_string(), labels, d.r#type.as_deref() == Some("blocks")))
            })
            .collect();
        let ask_open = deps
            .iter()
            .filter(|(_, l, blocks)| *blocks && l.iter().any(|x| x == ask))
            .count();
        let non_ask_open = deps
            .iter()
            .filter(|(_, l, _)| !l.iter().any(|x| x == ask))
            .count();
        if has_skip && ask_open == 0 {
            out.push((false, b.id.clone()));
        } else if !has_skip
            && b.dependency_count > 0
            && !deps.is_empty()
            && ask_open > 0
            && non_ask_open == 0
        {
            out.push((true, b.id.clone()));
        }
    }
    out
}

/// CHECK 2 (reaper half): WORKING rows, not wait-held, whose lease expired more than `grace`
/// ago, or has expired at all with no live holder → (id, seconds since expiry).
/// A holder whose session is provably gone (`gone`) is reaped at once, lease or no lease.
pub fn stale_leases(rows: &[LcRow], now: i64, grace: i64, alive: impl Fn(&str) -> bool, gone: impl Fn(&str) -> bool) -> Vec<(String, i64)> {
    rows.iter()
        .filter(|r| r.state == "WORKING" && !r.holds.iter().any(|h| h == "wait"))
        .filter_map(|r| {
            let past = now - r.lease_until.unwrap_or(now);
            let session_gone = r.holder.as_deref().is_some_and(&gone);
            (session_gone || (r.lease_until.is_some() && (past > grace || (past > 0 && !alive(&r.bead_id))))).then(|| (r.bead_id.clone(), past))
        })
        .collect()
}

/// CHECK 2c: rows whose holder and state disagree.
pub fn inconsistent(rows: &[LcRow]) -> Vec<String> {
    rows.iter()
        .filter_map(|r| {
            if r.state == "WORKING" && r.holder.is_none() {
                Some(format!(
                    "INCONSISTENT\t{}\tWORKING with no holder",
                    r.bead_id
                ))
            } else if r.state != "WORKING" && r.holder.is_some() {
                Some(format!(
                    "INCONSISTENT\t{}\t{} with a holder still set",
                    r.bead_id, r.state
                ))
            } else {
                None
            }
        })
        .collect()
}

/// Open work beads with no `spira_lifecycle` row: they can never be claimed.
///
/// NAMED EXCEPTION to lifecycle-guard's bd-status-read rule (lifecycle-guard/DESIGN.md,
/// "The rowless controls"; the Concierge's ruling on sp-mve9i): this is the positive control
/// for "no bead is rowless". A bead with no lifecycle row has no state but bd's, so the only
/// way to find one is to ask bd which beads it considers live and look for each in the
/// machine — reading bd status here audits the machine's coverage, it decides nothing about
/// a bead the machine holds. The rule names this function; nothing else may do this.
///
/// The CHECK5-LC shapes (landed-but-open, closed-unlanded, blocked-by-unlanded) are gone:
/// each was a disagreement between bd `status` and the lifecycle row, and with bd status
/// inert for work beads (design §3.4, sp-mve9i) there is nothing for the row to disagree with.
pub fn rowless(snap: &Snapshot, rows: &[LcRow], work_types: &[String]) -> Vec<String> {
    let have: HashSet<&str> = rows.iter().map(|r| r.bead_id.as_str()).collect();
    snap.list
        .iter()
        .filter(|b| matches!(b.status.as_str(), "open" | "in_progress"))
        .filter(|b| work_types.iter().any(|t| t == b.typ()))
        .filter(|b| !have.contains(b.id.as_str()))
        .map(|b| b.id.clone())
        .collect()
}

impl<'a> Sentinel<'a> {
    /// The pass's one lifecycle read. The machine is authoritative, so an absent binary or a
    /// failed read is LOUD: one `LIFECYCLE UNREACHABLE` line naming why, no lifecycle
    /// decision this pass, and the unit exits 1 once the rest of the pass has run.
    pub fn lc_rows(&self) -> Option<Vec<LcRow>> {
        if let Some(memo) = self.lc_memo.borrow().as_ref() {
            return memo.clone();
        }
        let rows = self.lc_rows_read();
        *self.lc_memo.borrow_mut() = Some(rows.clone());
        rows
    }

    /// The bead states the pass's snapshot carries (design §3.4, sp-mve9i): a work bead's
    /// state is its lifecycle row, because bd status is inert and there is no other source —
    /// [`Self::lc_rows`] (fail-closed, LOUD).
    pub fn state_rows(&self) -> Option<Vec<LcRow>> {
        self.lc_rows()
    }

    fn lc_rows_read(&self) -> Option<Vec<LcRow>> {
        let bin = self.cfg.lc_bin.clone();
        let o = self.h.run(Spec::args_owned(bin, vec!["list".into()]));
        if !o.ok() {
            let why = o
                .stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no message");
            self.lc_unreachable(&format!("`spira-lc list` failed (rc={}: {why})", o.rc));
            return None;
        }
        match parse_lc_rows(&o.stdout) {
            Ok(r) => Some(r),
            Err(e) => {
                self.lc_unreachable(&e);
                None
            }
        }
    }

    pub fn lc_unreachable(&self, why: &str) {
        if !self.lc_failed.replace(true) {
            self.log(&format!(
                "LIFECYCLE UNREACHABLE — {why}; CHECK 2/2c/4 make no lifecycle decision this pass and the unit exits 1"
            ));
        }
    }

    /// Apply one lifecycle event; a machine that cannot answer (rc 2, or no binary)
    /// is loud, a CAS refusal (rc 3: someone moved the row first) is the normal race.
    pub fn lc_apply(&self, id: &str, kind: &str) -> bool {
        match self.lc_event(id, kind, "sentinel") {
            Ok(()) => true,
            Err(rc) => {
                if rc == 2 || rc == 127 {
                    self.lc_unreachable(&format!(
                        "spira-lc could not apply an event to {id} (rc={rc})"
                    ));
                } else {
                    self.log(&format!(
                        "WARN lifecycle event on {id} refused (rc={rc}) — retried next pass"
                    ));
                }
                false
            }
        }
    }

    /// One bead's lifecycle state, read live (`spira-lc show <id>`): the re-read before a
    /// write that a stale snapshot must not drive. None when the machine has no row or
    /// cannot answer.
    pub fn lc_live_state(&self, id: &str) -> Option<String> {
        let o = self.h.run(
            Spec::args_owned(self.cfg.lc_bin.clone(), vec!["show".into(), id.into()])
                .err(Io::Null)
                .timeout(5),
        );
        if !o.ok() {
            return None;
        }
        let v: Value = serde_json::from_str(o.stdout.trim()).ok()?;
        v.get("bead")?.get("state")?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
    }

    /// `spira-lc show` + `event`: read the row's current state and version, then apply
    /// `kind` under that CAS. Ok(()) applied; Err(rc) as the caller verbs (1 no row, 2 cannot
    /// tell).
    pub fn lc_event(&self, id: &str, kind: &str, actor: &str) -> Result<(), i32> {
        let bin = self.cfg.lc_bin.clone();
        let o = self
            .h
            .run(Spec::args_owned(bin.clone(), vec!["show".into(), id.into()]).err(Io::Null));
        if !o.ok() {
            return Err(o.rc);
        }
        let v: Value = serde_json::from_str(o.stdout.trim()).unwrap_or(Value::Null);
        let field = |k: &str| -> String {
            match v.get("bead").and_then(|b| b.get(k)) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(x) => x.to_string(),
            }
        };
        let (state, version) = (field("state"), field("version"));
        if state.is_empty() || version.is_empty() {
            return Err(1);
        }
        let e = self.h.run(
            Spec::args_owned(
                bin,
                vec![
                    "event".into(),
                    "bead".into(),
                    id.into(),
                    "--expect".into(),
                    state,
                    "--version".into(),
                    version,
                    "--actor".into(),
                    actor.into(),
                    "--kind".into(),
                    kind.into(),
                ],
            )
            .out(Io::Null)
            .err(Io::Null),
        );
        if e.ok() {
            Ok(())
        } else {
            Err(e.rc)
        }
    }

    /// CHECK 2 — dead workers: the wait hold for ask-blocked beads, then the time-based reap.
    pub fn check2(&self, snap: &crate::store::Snapshot, rows: &[LcRow]) {
        let ask = self.cfg.ask.clone();
        let held: HashSet<String> = rows
            .iter()
            .filter(|r| r.holds.iter().any(|h| h == "wait"))
            .map(|r| r.bead_id.clone())
            .collect();
        // The bash reaper re-listed spira-lc after the protect step, so a hold applied (or
        // released) a moment ago already counted. The rows here are the pass's one read;
        // apply the protect step's successful writes to them before reaping.
        let mut now_held = held.clone();
        for (add, id) in wait_decisions(snap, rows, &held, &ask) {
            if add {
                let ev = hold_event(
                    "wait",
                    &format!("waiting on {ask} dep(s), excluded from CHECK 2's reclaim"),
                )
                .unwrap_or_default();
                if self.lc_apply(&id, &ev) {
                    now_held.insert(id.clone());
                }
                self.log(&format!(
                    "CHECK2 {id}: only open dep(s) carry {ask} — wait-held, excluded from reclaim"
                ));
                self.act(&format!(
                    "protected {id} from reclaim: waiting on {ask} dep"
                ));
            } else {
                if self.lc_apply(&id, &unhold_event("wait").unwrap_or_default()) {
                    now_held.remove(&id);
                }
                self.log(&format!("CHECK2 {id}: {ask} dep no longer blocking — wait released, re-enters the reaper"));
                self.act(&format!("unprotected {id}: {ask} dep closed"));
            }
        }
        let rows: Vec<LcRow> = rows
            .iter()
            .map(|r| {
                let mut r = r.clone();
                r.holds.retain(|h| h != "wait");
                if now_held.contains(&r.bead_id) {
                    r.holds.push("wait".into());
                }
                r
            })
            .collect();
        let now = self.h.now();
        let mut n = 0;
        for (id, ago) in stale_leases(&rows, now, self.cfg.reclaim_grace, |id| self.holder_alive(id), |h| spira_config::session::session_gone(h, &spira_config::admission::RealProcs)) {
            if let Some(now_state) = self.lc_live_state(&id).filter(|st| st != "WORKING") {
                self.log(&format!("CHECK2 {id}: skipped HolderDead — the bead moved to {now_state} since the sweep read it"));
                continue;
            }
            if !self.lc_apply(&id, HOLDER_DEAD) {
                continue;
            }
            self.write_event_row(&id, "reclaimed", "stale-lease");
            let note = if ago > 0 {
                format!("Reclaimed by CHECK 2: in_progress with a lease that expired {}m ago and was never released.", ago / 60)
            } else {
                "Reclaimed by CHECK 2: the holder's session is gone and never released the claim.".to_string()
            };
            self.bd()
                .quiet(self.h, &["note", &id, "--stdin"], Some(&note));
            n += 1;
        }
        if n > 0 {
            self.progress(&format!("reclaimed {n} stale lease(s)"));
        }
    }

    /// One attempt-history fact appended to the lifecycle event log, best-effort.
    pub fn write_event_row(&self, id: &str, etype: &str, cause: &str) {
        let actor = self
            .ctx
            .get("BEADS_ACTOR")
            .filter(|s| !s.is_empty())
            .unwrap_or("harness")
            .to_string();
        let args = ["fact", id, "--kind", etype, "--actor", &actor, "--cause", cause].map(String::from).to_vec();
        let _ = self.h.run(
            Spec::args_owned(self.cfg.lc_bin.clone(), args)
                .out(Io::Null)
                .err(Io::Null)
                .timeout(self.cfg.bd_timeout),
        );
    }

    /// CHECK 2c — detect, never repair.
    pub fn check2c(&self, rows: &[LcRow]) {
        let lines = inconsistent(rows);
        if lines.is_empty() {
            return;
        }
        self.h.print(&lines.join("\n"));
        self.log(&format!(
            "CHECK2c: {} spira-lc row(s) with holder/state out of sync — a bug reached spira_lifecycle outside its own CAS",
            lines.len()
        ));
        self.act(&format!(
            "surfaced {} inconsistent spira-lc row(s)",
            lines.len()
        ));
    }
}

impl<'a> Sentinel<'a> {
    /// Surface every rowless open work bead and backfill its row (`create-bead` is
    /// idempotent). A failed backfill is loud and retried next pass.
    pub fn check_rowless(&self, snap: &Snapshot, rows: &[LcRow]) {
        let rowless_ids = rowless(snap, rows, &self.cfg.work_types);
        if rowless_ids.is_empty() {
            return;
        }
        let bin = self.cfg.lc_bin.clone();
        let mut failed = Vec::new();
        for id in &rowless_ids {
            let o = self.h.run(
                Spec::args_owned(bin.clone(), vec!["create-bead".into(), id.clone()])
                    .out(Io::Null)
                    .err(Io::Null),
            );
            if !o.ok() {
                failed.push(id.clone());
            }
        }
        self.h.print(
            &rowless_ids.iter()
                .map(|i| format!("STATE-LC {i} rowless — open bead had no spira-lc row; backfilled"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.log(&format!(
            "CHECK-ROWLESS: {} open bead(s) had no lifecycle row; {} backfill(s) failed{}",
            rowless_ids.len(),
            failed.len(),
            if failed.is_empty() { String::new() } else { format!(": {}", failed.join(" ")) }
        ));
        self.act(&format!("backfilled {} rowless bead(s)", rowless_ids.len() - failed.len()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Snapshot;

    #[test]
    fn hold_events_are_lc_sh_json() {
        assert_eq!(
            hold_event("poison", "poisoned after 3 \"x\"").unwrap(),
            r#"{"Hold":{"kind":"Poison","cause":"attempts-exhausted","detail":"poisoned after 3 \"x\""}}"#
        );
        assert_eq!(
            unhold_event("wait").unwrap(),
            r#"{"Unhold":{"kind":"Wait"}}"#
        );
        assert!(hold_event("bogus", "").is_none());
    }

    fn snap() -> Snapshot {
        Snapshot::from_json(
            r#"[
            {"id":"w1","status":"in_progress","dependency_count":1,"dependencies":[{"depends_on_id":"q1","type":"blocks"}]},
            {"id":"w2","status":"in_progress","dependency_count":2,"dependencies":[{"depends_on_id":"q1","type":"blocks"},{"depends_on_id":"o1","type":"blocks"}]},
            {"id":"w3","status":"in_progress","dependency_count":1,"dependencies":[{"depends_on_id":"c1","type":"blocks"}]},
            {"id":"w4","status":"in_progress","dependency_count":0},
            {"id":"w5","status":"in_progress","dependency_count":1,"dependencies":[{"depends_on_id":"q1","type":"parent-child"}]},
            {"id":"q1","status":"open","labels":["ask"]},
            {"id":"o1","status":"open","labels":[]},
            {"id":"c1","status":"closed","labels":["ask"]}
        ]"#,
            None,
        )
        .with_lc(Some(&working()))
    }

    fn working() -> Vec<LcRow> {
        let mut rows: Vec<LcRow> = ["w1", "w2", "w3", "w4", "w5"].iter().map(|i| lc5_row(i, "WORKING")).collect();
        rows.push(lc5_row("o1", "READY"));
        rows
    }

    #[test]
    fn protect_only_when_every_open_dep_is_an_ask_blocker() {
        let held: HashSet<String> = ["w3".to_string()].into();
        let d = wait_decisions(&snap(), &working(), &held, "ask");
        assert_eq!(d, vec![(true, "w1".to_string()), (false, "w3".to_string())]);
        // w2 has a non-ask open dep; w4 has none; w5's ask dep is not a blocks edge.
    }

    /// sp-mve9i: the candidates are the WORKING lifecycle rows and a work dependency is
    /// resolved by its row, never bd status. bd calls x in_progress and its dependency o2
    /// closed; the machine has x READY (not a candidate) and y WORKING on o2, which is still
    /// WORKING — so y's open dep is a non-ask and y is not wait-held. z's dependency o3 is
    /// open in bd but SUBMITTED in the machine: only its ask dep is left, so z is held.
    #[test]
    fn wait_decisions_read_the_lifecycle_row_not_bd_status() {
        let s = Snapshot::from_json(
            r#"[
            {"id":"x","status":"in_progress","dependency_count":1,"dependencies":[{"depends_on_id":"q","type":"blocks"}]},
            {"id":"y","status":"open","dependency_count":2,"dependencies":[{"depends_on_id":"q","type":"blocks"},{"depends_on_id":"o2","type":"blocks"}]},
            {"id":"z","status":"open","dependency_count":2,"dependencies":[{"depends_on_id":"q","type":"blocks"},{"depends_on_id":"o3","type":"blocks"}]},
            {"id":"q","status":"open","labels":["ask"]},
            {"id":"o2","status":"closed","labels":[]},
            {"id":"o3","status":"open","labels":[]}
        ]"#,
            None,
        );
        let rows = vec![lc5_row("x", "READY"), lc5_row("y", "WORKING"), lc5_row("z", "WORKING"), lc5_row("o2", "WORKING"), lc5_row("o3", "SUBMITTED")];
        let s = s.with_lc(Some(&rows));
        assert_eq!(wait_decisions(&s, &rows, &HashSet::new(), "ask"), vec![(true, "z".to_string())]);
    }

    #[test]
    fn stale_leases_skip_wait_and_respect_grace() {
        let rows = vec![
            LcRow {
                bead_id: "a".into(),
                state: "WORKING".into(),
                lease_until: Some(100),
                ..Default::default()
            },
            LcRow {
                bead_id: "b".into(),
                state: "WORKING".into(),
                lease_until: Some(100),
                holds: vec!["wait".into()],
                ..Default::default()
            },
            LcRow {
                bead_id: "c".into(),
                state: "WORKING".into(),
                lease_until: Some(900),
                ..Default::default()
            },
            LcRow {
                bead_id: "d".into(),
                state: "READY".into(),
                lease_until: Some(100),
                ..Default::default()
            },
            LcRow {
                bead_id: "e".into(),
                state: "WORKING".into(),
                lease_until: None,
                ..Default::default()
            },
        ];
        assert_eq!(stale_leases(&rows, 1000, 500, |_| true, |_| false), vec![("a".to_string(), 900)]);
    }

    #[test]
    fn a_holder_whose_session_is_gone_is_reaped_before_its_lease_expires() {
        let row = |id: &str, holder: &str| LcRow {
            bead_id: id.into(),
            state: "WORKING".into(),
            holder: Some(holder.into()),
            lease_until: Some(9_000),
            ..Default::default()
        };
        let rows = vec![row("orphan", "aeon-mindy@7.99"), row("running", "aeon-mindy@8.120")];
        let gone = |h: &str| h == "aeon-mindy@7.99";
        assert_eq!(stale_leases(&rows, 1000, 500, |_| true, gone), vec![("orphan".to_string(), -8000)]);
    }

    #[test]
    fn stale_leases_reap_at_expiry_only_when_the_holder_is_gone() {
        let row = |id: &str, lu| LcRow {
            bead_id: id.into(),
            state: "WORKING".into(),
            lease_until: Some(lu),
            ..Default::default()
        };
        let rows = vec![row("dead", 900), row("live", 900), row("unexpired", 2000)];
        let alive = |id: &str| id == "live";
        assert_eq!(stale_leases(&rows, 1000, 500, alive, |_| false), vec![("dead".to_string(), 100)]);
        assert_eq!(
            stale_leases(&rows, 1500, 500, alive, |_| false),
            vec![("dead".to_string(), 600), ("live".to_string(), 600)]
        );
    }

    #[test]
    fn inconsistency_lines() {
        let rows = vec![
            LcRow {
                bead_id: "a".into(),
                state: "WORKING".into(),
                holder: None,
                ..Default::default()
            },
            LcRow {
                bead_id: "b".into(),
                state: "READY".into(),
                holder: Some("x".into()),
                ..Default::default()
            },
            LcRow {
                bead_id: "c".into(),
                state: "WORKING".into(),
                holder: Some("x".into()),
                ..Default::default()
            },
        ];
        assert_eq!(
            inconsistent(&rows),
            vec![
                "INCONSISTENT\ta\tWORKING with no holder",
                "INCONSISTENT\tb\tREADY with a holder still set"
            ]
        );
    }

    // ── CHECK-ROWLESS ─────────────────────────────────────────────────────────────────

    fn lc5_row(id: &str, state: &str) -> LcRow {
        LcRow {
            bead_id: id.into(),
            state: state.into(),
            ..Default::default()
        }
    }

    fn wt() -> Vec<String> {
        vec!["task".into(), "bug".into(), "feature".into()]
    }

    /// The rowless control (a named exception, see `rowless`): open and in-progress work
    /// beads bd holds with no lifecycle row; a closed bead or an epic is not one.
    #[test]
    fn rowless_flags_live_work_beads_with_no_row() {
        let s = Snapshot::from_json(
            r#"[{"id":"a","status":"open","issue_type":"task"},{"id":"c","status":"closed","issue_type":"task"},
                {"id":"r","status":"in_progress","issue_type":"task"},{"id":"g","status":"open","issue_type":"epic"}]"#,
            None,
        );
        let rows = vec![lc5_row("a", "READY")];
        assert_eq!(rowless(&s, &rows, &wt()), vec!["r"]);
        assert!(rowless(&s, &[lc5_row("a", "READY"), lc5_row("r", "READY")], &wt()).is_empty());
    }
}
