//! Everything the sentinel reads from or writes to `spira_lifecycle` (DESIGN.md §1, "On
//! lifecycle_enforce"): CHECK 2's wait hold and stale-lease reap, CHECK 2c's consistency
//! sweep, and CHECK 4's poison hold. sp-i2m7y moved these onto spira-lc unconditionally;
//! there is no bd-label mode to fall back to, and nothing here reads lifecycle_enforce.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::host::{Io, Spec};
use crate::model::{parse_lc_rows, LcRow};
use crate::pass::Sentinel;
use crate::store::Snapshot;

/// The hold kind's serde tag and implied cause (spira-lc callers.rs `hold_cause`; lc.sh's
/// `_lc_hold_kind_tag` / `_lc_hold_kind_cause` before sp-arpjt).
pub fn hold_tag(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "poison" => ("Poison", "attempts-exhausted"),
        "ask" => ("Ask", "operator-question"),
        "wait" => ("Wait", "unlanded-blocker"),
        "operator" => ("Operator", "manual-hold"),
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

/// CHECK 2 (protect-waiting half): which in-progress beads to wait-hold or release.
/// `held` is the wait-held set; each bead's dependency facts come from the snapshot join.
pub fn wait_decisions(
    snap: &crate::store::Snapshot,
    held: &HashSet<String>,
    ask: &str,
) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    for b in snap.list.iter().filter(|b| b.status == "in_progress") {
        let has_skip = held.contains(&b.id);
        if !(has_skip || b.dependency_count > 0) {
            continue;
        }
        let deps: Vec<(String, Vec<String>, bool)> = b
            .dependencies
            .iter()
            .filter_map(|d| {
                let t = d.target()?;
                let (status, labels) = match snap.get(t) {
                    Some(x) => (x.status.clone(), x.labels.clone()),
                    None => (
                        d.status.clone().unwrap_or_default(),
                        d.labels.clone().unwrap_or_default(),
                    ),
                };
                if status == "closed" {
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

/// CHECK 2 (reaper half): WORKING rows whose lease expired more than `grace` ago, not
/// wait-held → (id, seconds since expiry).
pub fn stale_leases(rows: &[LcRow], now: i64, grace: i64) -> Vec<(String, i64)> {
    rows.iter()
        .filter(|r| r.state == "WORKING" && !r.holds.iter().any(|h| h == "wait"))
        .filter_map(|r| {
            let lu = r.lease_until?;
            (now - lu > grace).then(|| (r.bead_id.clone(), now - lu))
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

/// Whether an `LcRow.state` tag is one of the machine's terminal states (`lifecycle::bead::
/// BeadState::is_terminal`) — this crate depends on the tag strings `spira-lc list` emits,
/// not the `lifecycle` crate itself.
fn lc_terminal(state: &str) -> bool {
    matches!(state, "LANDED" | "SUPERSEDED" | "DROPPED" | "DONE")
}

/// CHECK5-LC (ON path, design sp-pswer.2 — "design ON-path replacement for CHECK5 / groomer
/// STATE sweeps"): the landed-but-open shape lib.sh's `detect_landed_but_open` proves today
/// by grepping every open bead's repo base for a commit subject naming it. A LANDED row is
/// the same fact spira_lifecycle already carries — reached only via `content_on_base` or
/// `delivered`, both proof-carrying transitions — so this is a lookup, not a git walk.
pub fn landed_but_open(snap: &Snapshot, rows: &[LcRow], work_types: &[String]) -> Vec<String> {
    let lc: HashMap<&str, &str> = rows
        .iter()
        .map(|r| (r.bead_id.as_str(), r.state.as_str()))
        .collect();
    snap.list
        .iter()
        .filter(|b| matches!(b.status.as_str(), "open" | "in_progress"))
        .filter(|b| work_types.iter().any(|t| t == b.typ()))
        .filter(|b| lc.get(b.id.as_str()) == Some(&"LANDED"))
        .map(|b| {
            format!(
                "STATE-LC {} landed-but-open — spira-lc row is LANDED; close it",
                b.id
            )
        })
        .collect()
}

/// Open work beads with no `spira_lifecycle` row: they can never be claimed.
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

/// CHECK5-LC: the closed-unlanded shape `detect_closed_unlanded_states` proves today from the
/// base's commit graph plus a hand-maintained exclusion list (supersedes, spira-dropped,
/// delivers:, content-landed) — every entry of which is one of the machine's own terminal
/// states, so `is_terminal` alone replaces the whole list. Returns (id, lc_state) pairs; the
/// ids are also `false_blockers`'s input.
pub fn closed_unlanded(
    snap: &Snapshot,
    rows: &[LcRow],
    work_types: &[String],
) -> Vec<(String, String)> {
    let lc: HashMap<&str, &str> = rows
        .iter()
        .map(|r| (r.bead_id.as_str(), r.state.as_str()))
        .collect();
    snap.list
        .iter()
        .filter(|b| b.status == "closed")
        .filter(|b| work_types.iter().any(|t| t == b.typ()))
        .filter_map(|b| {
            let st = *lc.get(b.id.as_str())?;
            (!lc_terminal(st)).then(|| (b.id.clone(), st.to_string()))
        })
        .collect()
}

/// CHECK5-LC: the false-blockers shape `detect_false_blockers` proves today by re-reading
/// `detect_closed_unlanded_states`'s own ids as the blocker set — the same relation, against
/// `closed_unlanded`'s ids instead.
pub fn false_blockers(snap: &Snapshot, closed_unlanded_ids: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for b in &snap.list {
        if !matches!(b.status.as_str(), "open" | "in_progress") {
            continue;
        }
        for d in &b.dependencies {
            if d.r#type.as_deref() != Some("blocks") {
                continue;
            }
            let Some(t) = d.target() else { continue };
            if closed_unlanded_ids.contains(t) {
                out.push(format!(
                    "STATE-LC {} blocked-by-unlanded {t} — depends on {t}, which is closed but its spira-lc row is not a terminal state",
                    b.id
                ));
            }
        }
    }
    out
}

impl<'a> Sentinel<'a> {
    /// ON mode's one lifecycle read. The machine is authoritative, so an absent binary or a
    /// failed read is LOUD: one `LIFECYCLE UNREACHABLE` line naming why, no lifecycle
    /// decision this pass, and the unit exits 1 once the rest of the pass has run.
    /// Never called in OFF mode.
    pub fn lc_rows(&self) -> Option<Vec<LcRow>> {
        if let Some(memo) = self.lc_memo.borrow().as_ref() {
            return memo.clone();
        }
        let rows = self.lc_rows_read();
        *self.lc_memo.borrow_mut() = Some(rows.clone());
        rows
    }

    fn lc_rows_read(&self) -> Option<Vec<LcRow>> {
        debug_assert_eq!(
            self.lc,
            crate::cfg::Lifecycle::On,
            "OFF must never read spira-lc"
        );
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
                "LIFECYCLE UNREACHABLE — lifecycle_enforce=1 but {why}; CHECK 2/2c/4 make no lifecycle decision this pass and the unit exits 1"
            ));
        }
    }

    /// ON mode: apply one lifecycle event; a machine that cannot answer (rc 2, or no binary)
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
        for (add, id) in wait_decisions(snap, &held, &ask) {
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
        for (id, ago) in stale_leases(&rows, now, self.cfg.reclaim_grace) {
            if !self.lc_apply(&id, HOLDER_DEAD) {
                continue;
            }
            self.write_event_row(&id, "reclaimed", "stale-lease");
            let note = format!("Reclaimed by CHECK 2: in_progress with a lease that expired {}m ago and was never released.", ago / 60);
            self.bd()
                .quiet(self.h, &["note", &id, "--stdin"], Some(&note));
            n += 1;
        }
        if n > 0 {
            self.progress(&format!("reclaimed {n} stale lease(s)"));
        }
    }

    /// lib.sh `_bump_write_event`: one events row, best-effort.
    pub fn write_event_row(&self, id: &str, etype: &str, cause: &str) {
        let actor = self
            .ctx
            .get("BEADS_ACTOR")
            .filter(|s| !s.is_empty())
            .unwrap_or("harness")
            .to_string();
        let uuid = uuid4();
        let q = format!(
            "INSERT INTO events (id, issue_id, event_type, actor, new_value, created_at) VALUES ('{uuid}', '{}', '{}', '{}', '{}', UTC_TIMESTAMP())",
            sql_str(id),
            sql_str(etype),
            sql_str(&actor),
            sql_str(cause)
        );
        let args = vec!["-C".to_string(), self.cfg.db.clone(), "sql".into(), q];
        let _ = self.h.run(
            Spec::args_owned(self.cfg.bd.clone(), args)
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

    /// CHECK5-LC — detect, never repair, exactly as CHECK 2c: the three shapes CHECK 5 and
    /// the groomer's STATE sweeps prove from git log / bd status, read instead from the row
    /// `spira-lc list` already gave this pass. Advisory only, run alongside the legacy checks
    /// so the two can be compared before either is retired (design sp-pswer.2).
    pub fn check5_lc(&self, snap: &Snapshot, rows: &[LcRow]) {
        let work_types = self.cfg.work_types.clone();
        let mut lines = landed_but_open(snap, rows, &work_types);
        let unlanded = closed_unlanded(snap, rows, &work_types);
        let unlanded_ids: HashSet<String> = unlanded.iter().map(|(id, _)| id.clone()).collect();
        lines.extend(unlanded.iter().map(|(id, st)| {
            format!("STATE-LC {id} closed-unlanded — spira-lc row is {st}, not a terminal state")
        }));
        lines.extend(false_blockers(snap, &unlanded_ids));
        if lines.is_empty() {
            return;
        }
        self.h.print(&lines.join("\n"));
        self.log(&format!(
            "CHECK5-LC: {} state drift line(s) from spira-lc, alongside CHECK 5's own",
            lines.len()
        ));
        self.act(&format!("surfaced {} CHECK5-LC line(s)", lines.len()));
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

fn sql_str(s: &str) -> String {
    s.replace('\'', "''")
}

/// A random v4 UUID from /dev/urandom (python's uuid.uuid4()).
pub fn uuid4() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = b.iter().fold(String::new(), |mut s, x| {
        use std::fmt::Write;
        let _ = write!(s, "{x:02x}");
        s
    });
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
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
    }

    #[test]
    fn protect_only_when_every_open_dep_is_an_ask_blocker() {
        let held: HashSet<String> = ["w3".to_string()].into();
        let d = wait_decisions(&snap(), &held, "ask");
        assert_eq!(d, vec![(true, "w1".to_string()), (false, "w3".to_string())]);
        // w2 has a non-ask open dep; w4 has none; w5's ask dep is not a blocks edge.
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
        assert_eq!(stale_leases(&rows, 1000, 500), vec![("a".to_string(), 900)]);
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

    #[test]
    fn uuid_is_v4_shaped() {
        let u = uuid4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
    }

    // ── CHECK5-LC (design sp-pswer.2) ──────────────────────────────────────────────────

    fn lc5_snap() -> Snapshot {
        Snapshot::from_json(
            r#"[
            {"id":"a","status":"open","issue_type":"task"},
            {"id":"b","status":"in_progress","issue_type":"task"},
            {"id":"c","status":"closed","issue_type":"task"},
            {"id":"d","status":"closed","issue_type":"task"},
            {"id":"e","status":"open","issue_type":"task","dependencies":[{"depends_on_id":"c","type":"blocks"}]},
            {"id":"f","status":"open","issue_type":"task","dependencies":[{"depends_on_id":"d","type":"blocks"}]},
            {"id":"g","status":"open","issue_type":"epic"}
        ]"#,
            None,
        )
    }

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

    #[test]
    fn landed_but_open_flags_only_a_landed_row_on_an_open_bead() {
        let rows = vec![lc5_row("a", "LANDED"), lc5_row("b", "WORKING")];
        assert_eq!(
            landed_but_open(&lc5_snap(), &rows, &wt()),
            vec!["STATE-LC a landed-but-open — spira-lc row is LANDED; close it".to_string()]
        );
    }

    #[test]
    fn landed_but_open_is_silent_when_bd_and_lc_agree() {
        // SEEN RED: swapping b's row to LANDED (agreeing with a's already-planted offender)
        // would add a second line — proving this fixture's silence on b is not a tautology.
        let rows = vec![lc5_row("a", "LANDED"), lc5_row("b", "WORKING")];
        assert!(!landed_but_open(&lc5_snap(), &rows, &wt())
            .iter()
            .any(|l| l.contains(" b ")));
        let rows_seen_red = vec![lc5_row("a", "LANDED"), lc5_row("b", "LANDED")];
        assert_eq!(landed_but_open(&lc5_snap(), &rows_seen_red, &wt()).len(), 2);
    }

    #[test]
    fn rowless_flags_open_work_beads_with_no_row() {
        let rows = vec![lc5_row("a", "READY")];
        assert_eq!(rowless(&lc5_snap(), &rows, &wt()), vec!["b", "e", "f"]);
        let all = vec![lc5_row("a", "READY"), lc5_row("b", "READY"), lc5_row("e", "READY"), lc5_row("f", "READY")];
        assert!(rowless(&lc5_snap(), &all, &wt()).is_empty());
    }

    #[test]
    fn landed_but_open_ignores_a_non_work_type() {
        let rows = vec![lc5_row("g", "LANDED")];
        assert!(landed_but_open(&lc5_snap(), &rows, &wt()).is_empty());
    }

    #[test]
    fn closed_unlanded_flags_a_non_terminal_row_on_a_closed_bead() {
        let rows = vec![lc5_row("c", "WORKING"), lc5_row("d", "LANDED")];
        assert_eq!(
            closed_unlanded(&lc5_snap(), &rows, &wt()),
            vec![("c".to_string(), "WORKING".to_string())]
        );
    }

    #[test]
    fn closed_unlanded_accepts_every_terminal_state() {
        for st in ["LANDED", "SUPERSEDED", "DROPPED", "DONE"] {
            let rows = vec![lc5_row("c", st)];
            assert!(
                closed_unlanded(&lc5_snap(), &rows, &wt()).is_empty(),
                "{st} should be accepted as a legitimate close"
            );
        }
    }

    #[test]
    fn false_blockers_flags_only_the_dependent_of_a_closed_unlanded_blocker() {
        let ids: HashSet<String> = ["c".to_string()].into();
        assert_eq!(
            false_blockers(&lc5_snap(), &ids),
            vec!["STATE-LC e blocked-by-unlanded c — depends on c, which is closed but its spira-lc row is not a terminal state".to_string()]
        );
    }

    #[test]
    fn false_blockers_is_silent_once_the_blocker_is_no_longer_in_the_unlanded_set() {
        // SEEN RED: e's dependency on c is real; only clearing c from the set (as landing c
        // would) silences it — proving the filter is doing the work, not vacuously passing.
        assert!(!false_blockers(&lc5_snap(), &HashSet::new())
            .iter()
            .any(|l| l.contains(" e ")));
        let ids: HashSet<String> = ["c".to_string()].into();
        assert!(false_blockers(&lc5_snap(), &ids)
            .iter()
            .any(|l| l.contains(" e ")));
    }
}
