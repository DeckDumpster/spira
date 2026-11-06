//! The full pass's remaining checks as pure functions over one snapshot, for the scheduler's
//! `sentinel-pass` task: CHECK 2d (stale ask holds, stack conflicts, starvation), CHECK 3
//! (recompute-blocked when starved), CHECK 3c (coordination beads with open children, landed
//! red trackers), the plan counts CHECK 3 and CHECK 8 read, and CHECK 8's judgement predicate.
//! The binary's modules (`holds.rs`, `open_children.rs`, `red_trackers.rs`, `judge.rs`) state
//! the same contracts over the binary's own snapshot; each function here names the one it
//! mirrors. Anything a check needs from outside the snapshot (a file under `$SPIRA_RUN`, the
//! repository map) is an argument, so nothing here does IO.

use std::collections::{BTreeSet, HashMap, HashSet};

use spira_config::lc_state;
use spira_config::nonwork::{self, Kind};

use crate::model::{Bead, LcRow};

/// The beads and the lifecycle rows of one pass, indexed: what every check below reads.
pub struct View<'a> {
    pub beads: &'a [Bead],
    pub rows: &'a [LcRow],
    by_id: HashMap<&'a str, &'a Bead>,
    state: HashMap<&'a str, &'a str>,
    own_work: HashSet<String>,
}

impl<'a> View<'a> {
    pub fn new(beads: &'a [Bead], rows: &'a [LcRow]) -> View<'a> {
        // `store.rs` `set_lc`: a bead with a recorded tip, or one a live row's stack names.
        let own_work = rows
            .iter()
            .flat_map(|r| {
                let own = r.tip.iter().map(|_| r.bead_id.clone());
                let prereqs = (!lc_state::is_terminal(&r.state)).then(|| r.stack.iter().cloned());
                own.chain(prereqs.into_iter().flatten()).collect::<Vec<_>>()
            })
            .collect();
        View { beads, rows, by_id: beads.iter().map(|b| (b.id.as_str(), b)).collect(), state: rows.iter().map(|r| (r.bead_id.as_str(), r.state.as_str())).collect(), own_work }
    }

    pub fn get(&self, id: &str) -> Option<&'a Bead> {
        self.by_id.get(id).copied()
    }

    /// The bead's lifecycle state; `None` when the machine has no row for it.
    pub fn lc_state(&self, id: &str) -> Option<&'a str> {
        self.state.get(id).copied()
    }

    pub fn has_own_work(&self, id: &str) -> bool {
        self.own_work.contains(id)
    }
}

fn has_all(b: &Bead, labels: &[String]) -> bool {
    labels.iter().all(|l| b.has(l))
}

/// `store.rs` `plan_open`: every bead carrying all of `plan` whose row still owes builder work
/// (READY, WORKING, REWORK) and is work (not an epic or an event).
pub fn plan_open(v: &View, plan: &[String]) -> Vec<String> {
    v.beads.iter().filter(|b| v.lc_state(&b.id).is_some_and(|s| matches!(s, "READY" | "WORKING" | "REWORK")) && !matches!(b.typ(), "epic" | "event") && has_all(b, plan)).map(|b| b.id.clone()).collect()
}

/// `store.rs` `plan_inprog`: the plan beads whose row is being worked.
pub fn plan_inprog(v: &View, plan: &[String]) -> usize {
    v.beads.iter().filter(|b| v.lc_state(&b.id).is_some_and(lc_state::is_working) && has_all(b, plan)).count()
}

// ---------------------------------------------------------------------------------------------
// CHECK 2d (`holds.rs`).

pub const WORK_BEAD_LABEL: &str = "work-bead:";
const RAPID_RECUR: &str = "rapid-recur:";
pub const STARVED_PCT: usize = 25;
pub const STARVED_MIN_HELD: usize = 3;

/// The machine condition an ask hold's reason states, in its producer's wording.
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

/// The repository names the unmapped-repo holds among `rows` wait on: what the caller resolves.
pub fn unmapped_repos(rows: &[LcRow]) -> BTreeSet<String> {
    rows.iter().filter(|r| r.holds.iter().any(|h| h == "ask")).filter_map(|r| match r.reason.as_deref().and_then(condition) {
        Some(Condition::Unmapped(n)) => Some(n),
        _ => None,
    }).collect()
}

fn ask_open(v: &View, id: &str, ask_label: &str) -> bool {
    let tag = format!("{WORK_BEAD_LABEL}{id}");
    let open = |status: &str| !nonwork::is_closed(Kind::Ask, status);
    let Some(b) = v.get(id) else { return true };
    if b.has(ask_label) {
        return true;
    }
    if v.beads.iter().any(|a| open(&a.status) && a.has(&tag)) {
        return true;
    }
    b.dependencies.iter().filter_map(|d| d.target()).any(|t| v.get(t).is_some_and(|a| open(&a.status) && a.has(ask_label)))
}

/// `holds.rs` `stale_ask_holds`: `(bead, why)` for every ask hold to withdraw.
pub fn stale_ask_holds(v: &View, ask_label: &str, now: i64, grace: i64, repo_resolves: &dyn Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for r in v.rows.iter().filter(|r| r.holds.iter().any(|h| h == "ask")) {
        let aged = r.updated_at.is_some_and(|t| now - t >= grace);
        let why = match r.reason.as_deref().and_then(condition) {
            Some(Condition::Unmapped(name)) if repo_resolves(&name) => Some(format!("repo:{name} now resolves in the map")),
            Some(Condition::RapidRecur) if aged => Some(format!("rapid-recur hold is {grace}s old — retried")),
            _ if aged && !ask_open(v, &r.bead_id, ask_label) => Some("no open ask remains".to_string()),
            _ => None,
        };
        if let Some(w) = why {
            out.push((r.bead_id.clone(), w));
        }
    }
    out
}

/// `holds.rs` `starvation`: `(held, ready, starved)` over the READY rows.
pub fn starvation(rows: &[LcRow]) -> (usize, usize, bool) {
    let ready: Vec<&LcRow> = rows.iter().filter(|r| r.state == "READY").collect();
    let held = ready.iter().filter(|r| !r.holds.is_empty()).count();
    (held, ready.len(), held >= STARVED_MIN_HELD && held * 100 > ready.len() * STARVED_PCT)
}

/// The prerequisites a stack-conflict marker may stand on: CERTIFIED or IN_DELIVERY rows.
pub fn conflict_prereqs(rows: &[LcRow]) -> Vec<&LcRow> {
    rows.iter().filter(|r| matches!(r.state.as_str(), "CERTIFIED" | "IN_DELIVERY")).collect()
}

/// `holds.rs` `stale_conflicts`: `(dependent, prerequisite, age_secs)` for each READY/REWORK
/// bead depending on a prerequisite whose marker (`marker(id)` = recorded tip and age in
/// seconds) is at least `report_age_secs` old and still names the prerequisite's tip.
pub fn stale_conflicts(v: &View, marker: &dyn Fn(&str) -> Option<(String, u64)>, report_age_secs: u64) -> Vec<(String, String, u64)> {
    let mut out = Vec::new();
    for p in conflict_prereqs(v.rows) {
        let Some((tip, age)) = marker(&p.bead_id) else { continue };
        if age < report_age_secs || p.tip.as_deref() != Some(tip.as_str()) {
            continue;
        }
        for b in v.beads.iter().filter(|b| b.dependencies.iter().any(|d| d.target() == Some(p.bead_id.as_str()))) {
            if v.lc_state(&b.id).is_some_and(|s| matches!(s, "READY" | "REWORK")) {
                out.push((b.id.clone(), p.bead_id.clone(), age));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// CHECK 3 (`pass.rs` `check3`).

/// Whether the stale blocked flags are recomputed: the plan is starved (0 ready, nothing in
/// progress, work open) and the reaper is on. An unknown ready count is not starved.
pub fn should_recompute(skip_reclaim: bool, plan_ready: Option<usize>, plan_inprog: usize, n_open: usize) -> bool {
    !skip_reclaim && plan_ready == Some(0) && plan_inprog == 0 && n_open > 0
}

// ---------------------------------------------------------------------------------------------
// CHECK 3c (`open_children.rs`, `red_trackers.rs`).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Add(String),
    Remove(String),
}

fn child_open(v: &View, b: &Bead) -> bool {
    match v.lc_state(&b.id) {
        Some(state) => !lc_state::is_terminal(state),
        None if matches!(b.typ(), "epic" | "event") => !nonwork::is_closed(Kind::Epic, &b.status),
        None => true,
    }
}

/// `open_children.rs` `open_parents`: every bead with at least one open child, by the row's
/// `parent` field or a `parent-child` dependency.
pub fn open_parents<'a>(v: &View<'a>) -> HashSet<&'a str> {
    let mut out = HashSet::new();
    for b in v.beads.iter().filter(|b| child_open(v, b)) {
        if let Some(p) = b.parent.as_deref().filter(|p| !p.is_empty() && *p != b.id) {
            out.insert(p);
        }
        for d in b.dependencies.iter().filter(|d| d.r#type.as_deref() == Some("parent-child")) {
            if let Some(t) = d.target().filter(|t| !t.is_empty() && *t != b.id) {
                out.insert(t);
            }
        }
    }
    out
}

/// `open_children.rs` `decide`: the label changes over the scoped ready set and the labeled
/// claimable beads, ready candidates first.
pub fn open_children(v: &View, ready: &[String], label: &str) -> Vec<Change> {
    if label.is_empty() {
        return Vec::new();
    }
    let open = open_parents(v);
    let labeled: Vec<&str> = v.beads.iter().filter(|b| v.lc_state(&b.id).is_some_and(lc_state::is_claimable) && b.has(label)).map(|b| b.id.as_str()).collect();
    let labeled_set: HashSet<&str> = labeled.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in ready.iter().map(String::as_str).chain(labeled.iter().copied()) {
        if id.is_empty() || !seen.insert(id) {
            continue;
        }
        let has_open = open.contains(id) && !v.has_own_work(id);
        let currently = labeled_set.contains(id);
        if has_open && !currently {
            out.push(Change::Add(id.to_string()));
        } else if !has_open && currently {
            out.push(Change::Remove(id.to_string()));
        }
    }
    out
}

/// `red_trackers.rs` `decide`: trackers whose every `blocks` dependency is LANDED, with those.
pub fn landed_red_trackers(v: &View, label: &str) -> Vec<(String, Vec<String>)> {
    if label.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for b in v.beads.iter().filter(|b| b.has(label)) {
        if v.lc_state(&b.id).is_none_or(lc_state::is_terminal) {
            continue;
        }
        let fixes: Vec<String> = b.dependencies.iter().filter(|d| d.r#type.as_deref() == Some("blocks")).filter_map(|d| d.target().map(str::to_string)).collect();
        if !fixes.is_empty() && fixes.iter().all(|f| v.lc_state(f) == Some("LANDED")) {
            out.push((b.id.clone(), fixes));
        }
    }
    out
}

/// The close reason `close_landed_red_trackers` writes.
pub fn red_tracker_reason(fixes: &[String]) -> String {
    format!("No code change: every failure this tracker names is fixed by a landed bead ({}).", fixes.join(", "))
}

// ---------------------------------------------------------------------------------------------
// CHECK 8 (`judge.rs`).

/// How many open plan beads judgement is shown.
pub const REFLECT_IDS: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judge {
    Yes,
    Cooldown,
    No,
}

/// lib.sh `check8_should_judge`: judge only a starved plan where nothing moved this pass,
/// at most once per `every` seconds.
pub fn should_judge(plan_ready: Option<usize>, plan_inprog: usize, n_open: usize, progressed: u32, last: i64, now: i64, every: i64) -> Judge {
    let Some(plan_ready) = plan_ready else { return Judge::No };
    if plan_ready > 0 {
        return Judge::No;
    }
    if plan_inprog == 0 && n_open > 0 && progressed == 0 {
        return if now - last < every { Judge::Cooldown } else { Judge::Yes };
    }
    Judge::No
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;

    fn row(id: &str, state: &str) -> LcRow {
        LcRow { bead_id: id.into(), state: state.into(), ..LcRow::default() }
    }

    #[test]
    fn judgement_table_is_the_binarys() {
        assert_eq!(should_judge(Some(1), 0, 5, 0, 0, 10_000, 3600), Judge::No);
        assert_eq!(should_judge(Some(0), 0, 5, 0, 0, 10_000, 3600), Judge::Yes);
        assert_eq!(should_judge(Some(0), 0, 5, 0, 9_000, 10_000, 3600), Judge::Cooldown);
        assert_eq!(should_judge(Some(0), 0, 5, 1, 0, 10_000, 3600), Judge::No);
        assert_eq!(should_judge(None, 0, 5, 0, 0, 10_000, 3600), Judge::No);
    }

    #[test]
    fn recompute_runs_only_on_a_starved_plan() {
        assert!(should_recompute(false, Some(0), 0, 3));
        assert!(!should_recompute(true, Some(0), 0, 3), "the reaper is off");
        assert!(!should_recompute(false, None, 0, 3), "unknown is not starved");
        assert!(!should_recompute(false, Some(0), 1, 3));
        assert!(!should_recompute(false, Some(0), 0, 0));
    }

    #[test]
    fn an_open_child_labels_its_parent_and_a_closed_one_releases_it() {
        let beads = parse_beads(r#"[{"id":"p","status":"open"},{"id":"c","status":"open","parent":"p"}]"#).unwrap();
        let rows = vec![row("p", "READY"), row("c", "WORKING")];
        let v = View::new(&beads, &rows);
        assert_eq!(open_children(&v, &["p".into()], "oc"), vec![Change::Add("p".into())]);
        let labeled = parse_beads(r#"[{"id":"p","status":"open","labels":["oc"]},{"id":"c","status":"closed","parent":"p"}]"#).unwrap();
        let rows = vec![row("p", "READY"), row("c", "LANDED")];
        assert_eq!(open_children(&View::new(&labeled, &rows), &[], "oc"), vec![Change::Remove("p".into())]);
    }

    #[test]
    fn a_red_tracker_closes_once_every_fix_landed() {
        let beads = parse_beads(r#"[{"id":"t","status":"open","labels":["rt"],"dependencies":[{"depends_on_id":"f","type":"blocks"}]},{"id":"f","status":"open"}]"#).unwrap();
        let landed = vec![row("t", "READY"), row("f", "LANDED")];
        assert_eq!(landed_red_trackers(&View::new(&beads, &landed), "rt"), vec![("t".to_string(), vec!["f".to_string()])]);
        let pending = vec![row("t", "READY"), row("f", "SUBMITTED")];
        assert!(landed_red_trackers(&View::new(&beads, &pending), "rt").is_empty());
    }

    #[test]
    fn an_ask_hold_whose_ask_is_gone_is_withdrawn_after_the_grace() {
        let beads = parse_beads(r#"[{"id":"sp-a","status":"open","labels":[]}]"#).unwrap();
        let mut r = row("sp-a", "READY");
        r.holds = vec!["ask".into()];
        r.updated_at = Some(0);
        let rows = vec![r];
        let v = View::new(&beads, &rows);
        assert_eq!(stale_ask_holds(&v, "needs-operator", 10, 100, &|_| false), vec![]);
        assert_eq!(stale_ask_holds(&v, "needs-operator", 1_000, 100, &|_| false), vec![("sp-a".to_string(), "no open ask remains".to_string())]);
    }

    #[test]
    fn a_stale_marker_names_its_ready_dependents() {
        let beads = parse_beads(r#"[{"id":"p","status":"open"},{"id":"d","status":"open","dependencies":[{"depends_on_id":"p","type":"blocks"}]}]"#).unwrap();
        let mut p = row("p", "IN_DELIVERY");
        p.tip = Some("t1".into());
        let rows = vec![p, row("d", "REWORK")];
        let v = View::new(&beads, &rows);
        assert!(stale_conflicts(&v, &|_| Some(("t1".into(), 10)), 3600).is_empty(), "fresh");
        assert!(stale_conflicts(&v, &|_| Some(("t0".into(), 7200)), 3600).is_empty(), "another tip");
        assert_eq!(stale_conflicts(&v, &|_| Some(("t1".into(), 7200)), 3600), vec![("d".into(), "p".into(), 7200)]);
    }
}
