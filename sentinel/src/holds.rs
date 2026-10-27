//! CHECK 2d: an ask hold must not outlive the condition that raised it.
//!
//! A hold raised for a machine condition carries that condition as its reason, in the shape
//! its producer writes (`park_unmapped` in lib.sh, `rapid_recur_check` in dwarf), and
//! [`condition`] reads it back as a predicate the sentinel re-evaluates each pass. A hold
//! whose ask is gone — closed, answered, or relabelled off — is withdrawn too. And when held
//! beads are a large share of READY the builder partition reads "nothing ready", so that is
//! announced rather than left to be noticed (law-total-blockage-announces-itself).

use crate::host::Io;
use crate::model::LcRow;
use crate::pass::Sentinel;
use crate::seams;
use crate::store::Snapshot;

pub const WORK_BEAD_LABEL: &str = "work-bead:";
const RAPID_RECUR: &str = "rapid-recur:";
pub const STARVED_PCT: usize = 25;
pub const STARVED_MIN_HELD: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    Unmapped(String),
    RapidRecur,
}

pub fn condition(reason: &str) -> Option<Condition> {
    if reason.starts_with(RAPID_RECUR) {
        return Some(Condition::RapidRecur);
    }
    let (name, rest) = reason.strip_prefix("repo:")?.split_once(" has no ")?;
    (!name.is_empty() && !name.contains(char::is_whitespace) && rest.ends_with(" entry")).then(|| Condition::Unmapped(name.to_string()))
}

fn has_ask_hold(r: &LcRow) -> bool {
    r.holds.iter().any(|h| h == "ask")
}

/// Whether any ask is still open for `id`: the bead itself carries the ask label, an ask bead
/// names it as its work bead, or it depends on an open ask bead.
fn ask_open(snap: &Snapshot, id: &str, ask_label: &str) -> bool {
    let tag = format!("{WORK_BEAD_LABEL}{id}");
    let open = |status: &str| !spira_config::nonwork::is_closed(spira_config::nonwork::Kind::Ask, status);
    let Some(b) = snap.get(id) else { return true };
    if b.has(ask_label) {
        return true;
    }
    if snap.list.iter().any(|a| open(&a.status) && a.has(&tag)) {
        return true;
    }
    b.dependencies.iter().filter_map(|d| d.target()).any(|t| {
        snap.get(t).is_some_and(|a| open(&a.status) && a.has(ask_label))
    })
}

/// `(bead, why)` for every ask hold to withdraw. `repo_resolves` answers the unmapped-repo
/// predicate; `grace` is how long a hold must stand before its ask's absence, or a fault
/// that cannot be re-measured, counts against it — a hold placed a moment before its ask is
/// filed is not an orphan.
pub fn stale_ask_holds(
    rows: &[LcRow],
    snap: &Snapshot,
    ask_label: &str,
    now: i64,
    grace: i64,
    repo_resolves: &dyn Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for r in rows.iter().filter(|r| has_ask_hold(r)) {
        let aged = r.updated_at.is_some_and(|t| now - t >= grace);
        let why = match r.reason.as_deref().and_then(condition) {
            Some(Condition::Unmapped(name)) if repo_resolves(&name) => {
                Some(format!("repo:{name} now resolves in the map"))
            }
            Some(Condition::RapidRecur) if aged => Some(format!("rapid-recur hold is {grace}s old — retried")),
            _ if aged && !ask_open(snap, &r.bead_id, ask_label) => Some("no open ask remains".to_string()),
            _ => None,
        };
        if let Some(w) = why {
            out.push((r.bead_id.clone(), w));
        }
    }
    out
}

/// `(held, ready)` over the READY rows, and whether the held share passes the threshold.
pub fn starvation(rows: &[LcRow]) -> (usize, usize, bool) {
    let ready: Vec<&LcRow> = rows.iter().filter(|r| r.state == "READY").collect();
    let held = ready.iter().filter(|r| !r.holds.is_empty()).count();
    (held, ready.len(), held >= STARVED_MIN_HELD && held * 100 > ready.len() * STARVED_PCT)
}

/// `(dependent, prerequisite, age_secs)` for each open bead depending on a prerequisite whose
/// stack-conflict marker is older than `report_age` yet still names the prerequisite's tip.
pub fn stale_conflicts(rows: &[LcRow], snap: &Snapshot, run: &std::path::Path, report_age: std::time::Duration) -> Vec<(String, String, u64)> {
    let mut out = Vec::new();
    for p in rows.iter().filter(|r| matches!(r.state.as_str(), "CERTIFIED" | "IN_DELIVERY")) {
        let Some((tip, age)) = spira_config::stack_conflict::recorded(run, &p.bead_id) else { continue };
        if age < report_age || p.tip.as_deref() != Some(tip.as_str()) {
            continue;
        }
        for b in snap.list.iter().filter(|b| b.dependencies.iter().any(|d| d.target() == Some(p.bead_id.as_str()))) {
            if rows.iter().any(|r| r.bead_id == b.id && matches!(r.state.as_str(), "READY" | "REWORK")) {
                out.push((b.id.clone(), p.bead_id.clone(), age.as_secs()));
            }
        }
    }
    out
}

impl<'a> Sentinel<'a> {
    /// lib.sh `spira_event`, through its seam.
    fn event(&self, kind: &str, target: &str, title: &str, detail: &str) {
        let mut b = Vec::new();
        for f in [kind, target, title, detail] {
            b.extend_from_slice(f.as_bytes());
            b.push(0);
        }
        self.seam("event", seams::EVENT, Some(b), Io::Null, Io::Null, false);
    }

    pub fn check2d(&self, snap: &Snapshot, rows: &[LcRow]) {
        let ctx = &self.ctx;
        let resolves = |name: &str| {
            ctx.repo(name)
                .and_then(|r| r.root.as_deref())
                .is_some_and(|p| std::path::Path::new(p).join(".git").exists())
        };
        let stale = stale_ask_holds(rows, snap, &self.cfg.ask, self.h.now(), self.cfg.reclaim_grace, &resolves);
        let mut withdrawn = 0;
        for (id, why) in &stale {
            if self.lc_apply(id, "\"AskWithdrawn\"") {
                withdrawn += 1;
                self.log(&format!("CHECK2d {id}: ask hold withdrawn — {why}"));
                let labelled = snap.get(id).is_some_and(|b| b.has(&self.cfg.ask));
                if labelled {
                    self.bd().quiet(self.h, &["label", "remove", id, &self.cfg.ask], None);
                }
            }
        }
        if withdrawn > 0 {
            self.progress(&format!("withdrew {withdrawn} ask hold(s) whose cause no longer holds"));
        }
        for (bead, prereq, age) in stale_conflicts(rows, snap, &self.cfg.run, spira_config::stack_conflict::REPORT_AGE) {
            let title = format!("{bead} is excluded from the ready set by a stack-conflict marker on {prereq}, {age}s old");
            self.log(&format!("CHECK2d STACK-CONFLICT: {title}"));
            self.event("queue.starved", &bead, &title, "the marker lapses on its own; the next claim attempt re-records it if the conflict stands");
        }
        let (held, ready, starved) = starvation(rows);
        if starved {
            let title = format!("{held} of {ready} READY beads are held — the builder queue is starved");
            self.log(&format!("CHECK2d STARVED: {title}"));
            self.event("queue.starved", "-", &title, "withdraw holds whose cause is gone: spira-lc list --hold ask");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, state: &str, holds: &[&str], reason: Option<&str>, updated_at: i64) -> LcRow {
        LcRow {
            bead_id: id.into(),
            state: state.into(),
            holds: holds.iter().map(|h| h.to_string()).collect(),
            reason: reason.map(str::to_string),
            updated_at: Some(updated_at),
            ..Default::default()
        }
    }

    fn snap(json: &str) -> Snapshot {
        Snapshot::from_json(json, None)
    }

    const NOW: i64 = 1_000_000;
    const GRACE: i64 = 1000;

    #[test]
    fn a_stale_marker_on_a_live_prerequisite_names_its_dependents() {
        let d = testkit::TempDir::new("holds-sc");
        let s = snap(r#"[{"id":"sp-p","status":"open"},{"id":"sp-d","status":"open","dependencies":[{"depends_on_id":"sp-p","type":"blocks"}]}]"#); // literal-ok: test fixture
        let mut p = row("sp-p", "IN_DELIVERY", &[], None, NOW);
        p.tip = Some("t1".into());
        let rows = vec![p, row("sp-d", "REWORK", &[], None, NOW)];
        let hour = std::time::Duration::from_secs(3600);
        assert!(stale_conflicts(&rows, &s, d.path(), hour).is_empty(), "no marker");
        spira_config::stack_conflict::record(d.path(), "sp-p", "t1");
        assert!(stale_conflicts(&rows, &s, d.path(), hour).is_empty(), "a fresh marker is not stale");
        let got = stale_conflicts(&rows, &s, d.path(), std::time::Duration::ZERO);
        assert_eq!(got.iter().map(|g| (g.0.as_str(), g.1.as_str())).collect::<Vec<_>>(), vec![("sp-d", "sp-p")]);
    }

    #[test]
    fn conditions_parse_the_producers_wording_and_nothing_else() {
        assert_eq!(condition("repo:spira has no mapped entry"), Some(Condition::Unmapped("spira".into())));
        assert_eq!(condition("rapid-recur: 3 consecutive sub-10s dwarf calls"), Some(Condition::RapidRecur));
        assert_eq!(condition("should we ship?"), None);
        assert_eq!(condition("repo: has no mapped entry"), None);
        assert_eq!(condition("repo:a b has no mapped entry"), None);
    }

    #[test]
    fn an_unmapped_repo_hold_is_withdrawn_once_the_map_resolves_and_not_before() {
        let s = snap(r#"[{"id":"sp-a","status":"open","labels":["needs-operator"]}]"#); // literal-ok: test fixture
        let rows = vec![row("sp-a", "READY", &["ask"], Some("repo:spira has no mapped entry"), NOW)];
        let unresolved = stale_ask_holds(&rows, &s, "needs-operator", NOW, GRACE, &|_| false); // literal-ok: test fixture
        assert!(unresolved.is_empty(), "the condition still holds: {unresolved:?}");
        let resolved = stale_ask_holds(&rows, &s, "needs-operator", NOW, GRACE, &|n| n == "spira"); // literal-ok: test fixture
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "sp-a");
        let other = stale_ask_holds(&rows, &s, "needs-operator", NOW, GRACE, &|n| n == "other"); // literal-ok: test fixture
        assert!(other.is_empty(), "a different repo resolving says nothing about this one");
    }

    #[test]
    fn a_rapid_recur_hold_is_retried_only_after_the_grace() {
        let s = snap(r#"[{"id":"sp-a","status":"open","labels":["needs-operator"]}]"#); // literal-ok: test fixture
        let why = "rapid-recur: 3 consecutive sub-10s dwarf calls";
        let fresh = vec![row("sp-a", "READY", &["ask"], Some(why), NOW - 10)];
        assert!(stale_ask_holds(&fresh, &s, "needs-operator", NOW, GRACE, &|_| false).is_empty()); // literal-ok: test fixture
        let old = vec![row("sp-a", "READY", &["ask"], Some(why), NOW - GRACE)];
        assert_eq!(stale_ask_holds(&old, &s, "needs-operator", NOW, GRACE, &|_| false).len(), 1); // literal-ok: test fixture
    }

    #[test]
    fn a_closed_ask_leaves_no_hold_and_an_open_one_keeps_it() {
        let ask = "needs-operator"; // literal-ok: test fixture
        let rows = vec![row("sp-w", "READY", &["ask"], Some("should we ship?"), NOW - GRACE)];
        let open = snap(
            r#"[{"id":"sp-w","status":"open","labels":[]},
                {"id":"sp-d","status":"open","labels":["needs-operator","work-bead:sp-w"]}]"#, // literal-ok: test fixture
        );
        assert!(stale_ask_holds(&rows, &open, ask, NOW, GRACE, &|_| true).is_empty(), "its ask bead is open");
        let closed = snap(
            r#"[{"id":"sp-w","status":"open","labels":[]},
                {"id":"sp-d","status":"closed","labels":["needs-operator","work-bead:sp-w"]}]"#, // literal-ok: test fixture
        );
        let out = stale_ask_holds(&rows, &closed, ask, NOW, GRACE, &|_| true);
        assert_eq!(out.len(), 1, "the ask closed and the hold stayed");
        let relabelled = snap(r#"[{"id":"sp-w","status":"open","labels":[]}]"#);
        assert_eq!(stale_ask_holds(&rows, &relabelled, ask, NOW, GRACE, &|_| true).len(), 1);
        let labelled = snap(r#"[{"id":"sp-w","status":"open","labels":["needs-operator"]}]"#); // literal-ok: test fixture
        assert!(stale_ask_holds(&rows, &labelled, ask, NOW, GRACE, &|_| true).is_empty());
        let young = vec![row("sp-w", "READY", &["ask"], Some("q"), NOW - 5)];
        assert!(stale_ask_holds(&young, &closed, ask, NOW, GRACE, &|_| true).is_empty(), "ask may not be filed yet");
    }

    #[test]
    fn rows_without_an_ask_hold_are_never_touched() {
        let s = snap(r#"[{"id":"sp-a","status":"open","labels":[]}]"#);
        let rows = vec![row("sp-a", "READY", &["poison"], Some("repo:spira has no mapped entry"), 0)];
        assert!(stale_ask_holds(&rows, &s, "needs-operator", NOW, GRACE, &|_| true).is_empty()); // literal-ok: test fixture
    }

    #[test]
    fn starvation_fires_above_a_quarter_of_ready_and_not_at_it() {
        let mk = |ready: usize, held: usize| -> Vec<LcRow> {
            (0..ready)
                .map(|i| row(&format!("sp-{i}"), "READY", if i < held { &["ask"] } else { &[] }, None, 0))
                .collect()
        };
        assert_eq!(starvation(&mk(473, 167)), (167, 473, true));
        assert!(!starvation(&mk(12, 3)).2, "exactly 25% is not past the threshold");
        assert!(starvation(&mk(11, 3)).2);
        assert!(!starvation(&mk(4, 2)).2, "two held beads are not a starved queue");
        assert!(!starvation(&mk(0, 0)).2);
        let mut rows = mk(4, 4);
        rows.push(row("sp-w", "WORKING", &["ask"], None, 0));
        assert_eq!(starvation(&rows).1, 4, "only READY rows count");
    }
}
