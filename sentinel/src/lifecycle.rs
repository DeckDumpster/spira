//! Everything the sentinel reads from or writes to `spira_lifecycle` (DESIGN.md §1, "The
//! lifecycle machine"): CHECK 2's wait hold and stale-lease reap, CHECK 2c's consistency
//! sweep, and CHECK 4's poison hold. sp-i2m7y moved these onto spira-lc unconditionally;
//! there is no bd-label mode to fall back to, and no off mode (sp-v62vn).

use std::collections::HashSet;

use serde_json::Value;

use crate::model::{parse_lc_rows, LcRow};
use crate::pass::Sentinel;
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

pub use sentinel::lifecycle_events::HOLDER_DEAD;

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
        if !(has_skip || !b.dependencies.is_empty()) {
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
            && !b.dependencies.is_empty()
            && !deps.is_empty()
            && ask_open > 0
            && non_ask_open == 0
        {
            out.push((true, b.id.clone()));
        }
    }
    out
}

pub use sentinel::checks::{inconsistent, stale_leases};

fn names(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).collect()
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

    /// Rows for named beads whatever their age: the live read omits old terminal rows.
    pub fn lc_rows_by_id(&self, ids: &[String]) -> Vec<LcRow> {
        if ids.is_empty() {
            return Vec::new();
        }
        let o = self.h.lc(&self.cfg.lc_bin, &["list".into(), "--ids".into(), ids.join(",")]);
        if !o.ok() {
            return Vec::new();
        }
        parse_lc_rows(&o.stdout).unwrap_or_default()
    }

    /// Adds the terminal states the live read leaves out, for the beads a red tracker waits on.
    pub fn complete_snapshot(&self, snap: &mut crate::store::Snapshot) {
        let ids = snap.unread_blockers(&self.cfg.red_tracker);
        snap.add_lc_states(&self.lc_rows_by_id(&ids));
    }

    fn lc_rows_read(&self) -> Option<Vec<LcRow>> {
        let o = self.h.lc(&self.cfg.lc_bin, &["list".into(), "--live".into()]);
        if !o.ok() {
            let why = o
                .stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no message");
            self.lc_unreachable(&format!("`spira-lc list --live` failed (rc={}: {why})", o.rc));
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
        let o = self.h.lc(&self.cfg.lc_bin, &["show".into(), id.into()]);
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
        let bin = &self.cfg.lc_bin;
        let o = self.h.lc(bin, &["show".into(), id.into()]);
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
        let e = self.h.lc(
            bin,
            &[
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
        let mut stale = Vec::new();
        for (id, ago) in stale_leases(&rows, now, self.cfg.reclaim_grace, |id| self.holder_alive(id), |h| spira_config::session::session_gone(h, &spira_config::admission::RealProcs)) {
            if let Some(now_state) = self.lc_live_state(&id).filter(|st| !lc_state::is_working(st)) {
                self.log(&format!("CHECK2 {id}: skipped HolderDead — the bead moved to {now_state} since the sweep read it"));
                continue;
            }
            if !self.stop_holder(&id) {
                self.log(&format!("CHECK2 {id}: NOT reclaimed — the lease lapsed but a live process still works in its worktree and its unit did not stop"));
                continue;
            }
            stale.push((id, ago));
        }
        let returned: HashSet<String> = if self.cfg.control_store {
            self.expire_claims(&stale.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>())
        } else {
            stale.iter().filter(|(id, _)| self.lc_apply(id, HOLDER_DEAD)).map(|(id, _)| id.clone()).collect()
        };
        let mut n = 0;
        for (id, ago) in stale.iter().filter(|(id, _)| returned.contains(id)) {
            self.write_event_row(id, "reclaimed", "stale-lease");
            let note = if *ago > 0 {
                format!("Reclaimed by CHECK 2: in_progress with a lease that expired {}m ago and was never released.", ago / 60)
            } else {
                "Reclaimed by CHECK 2: the holder's session is gone and never released the claim.".to_string()
            };
            self.bd()
                .quiet(self.h, &["note", id, "--stdin"], Some(&note));
            n += 1;
        }
        if n > 0 {
            self.progress(&format!("reclaimed {n} stale lease(s)"));
        }
        if self.cfg.control_store {
            self.sweep_open(&rows);
        }
    }

    /// The control store's `expire-claims` for the holders just stopped: each named record with
    /// no live claim lock goes back to the state it was claimed from. The records returned.
    fn expire_claims(&self, stopped: &[&str]) -> HashSet<String> {
        if stopped.is_empty() {
            return HashSet::new();
        }
        let args = ["expire-claims", "--stopped", &stopped.join(","), "--actor", "sentinel"].map(String::from).to_vec();
        let o = self.h.lc(&self.cfg.lc_bin, &args);
        if !o.ok() {
            self.lc_unreachable(&format!("spira-lc expire-claims failed (rc={})", o.rc));
            return HashSet::new();
        }
        serde_json::from_str::<Value>(o.stdout.trim()).ok().map(|v| names(&v["returned"])).unwrap_or_default().into_iter().collect()
    }

    /// The control store's `sweep-open`: an OPEN record older than the grace is promoted when the
    /// planning plane still has its bead open, and escalated once when it does not. A planning
    /// read that cannot answer sweeps nothing: a bead it fails to list is not a bead that is gone.
    fn sweep_open(&self, rows: &[LcRow]) {
        let bin = &self.cfg.lc_bin;
        let open: Vec<&str> = rows.iter().filter(|r| r.state == "OPEN").map(|r| r.bead_id.as_str()).collect();
        if open.is_empty() {
            return;
        }
        let mut standing = HashSet::new();
        for chunk in open.chunks(100) {
            let o = self.h.lc(bin, &["plan", "items", "--ids", &chunk.join(","), "--open"].map(String::from));
            let items = o.ok().then(|| serde_json::from_str::<Value>(o.stdout.trim()).ok()).flatten().and_then(|v| v.get("items").cloned());
            let Some(items) = items.filter(Value::is_array) else {
                self.log("CHECK2 sweep-open: the planning plane did not answer — no OPEN record swept this pass");
                return;
            };
            standing.extend(items.as_array().into_iter().flatten().filter_map(|i| i["id"].as_str().map(str::to_string)));
        }
        let (present, gone): (Vec<&str>, Vec<&str>) = open.iter().partition(|id| standing.contains(**id));
        let call = |escalated: &[String]| {
            let mut args = vec!["sweep-open".to_string(), "--grace-secs".into(), self.cfg.open_promote_grace.to_string()];
            for (flag, ids) in [("--open", present.join(",")), ("--gone", gone.join(",")), ("--escalated", escalated.join(","))] {
                if !ids.is_empty() {
                    args.extend([flag.to_string(), ids]);
                }
            }
            args.extend(["--actor".to_string(), "sentinel".to_string()]);
            let o = self.h.lc(bin, &args);
            o.ok().then(|| serde_json::from_str::<Value>(o.stdout.trim()).ok()).flatten()
        };
        let Some(out) = call(&[]) else {
            self.lc_unreachable("spira-lc sweep-open failed");
            return;
        };
        for id in names(&out["promoted"]) {
            self.progress(&format!("promoted {id}: OPEN past the grace with its bead open"));
        }
        let mut mailed = Vec::new();
        for id in names(&out["escalate"]) {
            let subject = format!("{id} is OPEN with no open bead behind it");
            let default = format!("drop the record: `spira-lc drop-row {id}`");
            let body = format!("## Question\n{subject}\n\n## Default\n{default}\n\nA crashed filing left the record OPEN, and the planning plane has no open bead for it, so nothing will promote it.\n");
            if self.mail("Sentinel <sentinel@spira>", &subject, &default, &body, &id, false) {
                self.act(&format!("escalated {id}: OPEN with no open bead"));
                mailed.push(id);
            }
        }
        if !mailed.is_empty() && call(&mailed).is_none() {
            self.lc_unreachable("spira-lc sweep-open could not mark the escalations");
        }
    }

    /// Stop whatever still works in the bead's worktree before the bead goes back to READY.
    fn stop_holder(&self, id: &str) -> bool {
        self.h.svc.stop_holder(id)
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
        let _ = self.h.lc(&self.cfg.lc_bin, &args);
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
    fn the_wait_decision_reads_the_door_rows_edges_not_a_count_the_row_does_not_carry() {
        let doored = Snapshot::from_json(
            r#"[
            {"id":"w1","status":"in_progress","labels":[],"parent":null,"dependencies":[{"issue_id":"w1","depends_on_id":"q1","type":"blocks"}]},
            {"id":"w4","status":"in_progress","labels":[],"parent":null,"dependencies":[]},
            {"id":"q1","status":"open","labels":["ask"],"parent":null,"dependencies":[]}
        ]"#,
            None,
        )
        .with_lc(Some(&working()));
        let d = wait_decisions(&doored, &working(), &HashSet::new(), "ask");
        assert_eq!(d, vec![(true, "w1".to_string())], "w1 waits on an open ask; w4 has no edge");
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
        let rows = vec![row("orphan", "dwarf-austri@7.99"), row("running", "dwarf-austri@8.120")];
        let gone = |h: &str| h == "dwarf-austri@7.99";
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
            LcRow {
                bead_id: "d".into(),
                state: "SUBMITTED".into(),
                holder: Some("x".into()),
                ..Default::default()
            },
            LcRow {
                bead_id: "e".into(),
                state: "CERTIFIED".into(),
                holder: Some("x".into()),
                ..Default::default()
            },
            LcRow {
                bead_id: "f".into(),
                state: "IN_DELIVERY".into(),
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

}
