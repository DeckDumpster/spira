//! Claim selection: the stacked-dependents claimability rule (machine mode) and the
//! epic-first rank (sp-ns46j). Pure. DESIGN.md §2 `select`, §3.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use lifecycle::bead::{BeadState, HoldKind, Stack};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A `bd ready`/`bd list --json` row — only the fields read.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ReadyRow {
    pub id: String,
    /// Content only: carried so a ready set handed to a display (the cockpit's NEXT rows)
    /// names each bead without a second read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub priority: Option<i64>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default, deserialize_with = "null_vec")]
    pub labels: Vec<String>,
    #[serde(default)]
    pub issue_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub defer_until: Option<String>,
    #[serde(default, deserialize_with = "null_vec")]
    pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Dependency {
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default, alias = "id")]
    pub depends_on_id: Option<String>,
    #[serde(default, rename = "type", alias = "dependency_type")]
    pub dep_type: Option<String>,
}

fn null_vec<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

fn lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

impl ReadyRow {
    pub fn label_value(&self, prefix: &str) -> Option<&str> {
        self.labels.iter().find_map(|l| l.strip_prefix(prefix))
    }
    fn prio(&self) -> i64 {
        self.priority.unwrap_or(99)
    }
}

/// Parse a JSON array (or single object, or null/empty) of rows. Empty text is zero rows —
/// `json.loads(sys.stdin.read() or "[]")`, as epic_rank_rows reads it.
pub fn parse_ready(text: &str) -> Result<Vec<ReadyRow>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(Vec::new());
    }
    let v: Value = serde_json::from_str(t).map_err(|e| format!("ready set is not JSON: {e}"))?;
    let arr = match v {
        Value::Null => return Ok(Vec::new()),
        Value::Array(a) => a,
        other => vec![other],
    };
    arr.into_iter()
        .map(|r| serde_json::from_value::<ReadyRow>(r.clone()).map_err(|e| format!("bad ready row ({e}): {r}")))
        .collect()
}

/// epic_parent_lookup's output: `{"prio": {epic: priority}, "started": [epic, ...]}`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct EpicLookup {
    #[serde(default)]
    pub prio: BTreeMap<String, i64>,
    #[serde(default)]
    pub started: Vec<String>,
}

pub fn parse_lookup(text: &str) -> Result<EpicLookup, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(EpicLookup::default());
    }
    serde_json::from_str(t).map_err(|e| format!("epic lookup is not JSON: {e}"))
}

/// The distinct parent epics a ready set references, sorted.
pub fn parents(rows: &[ReadyRow]) -> Vec<String> {
    rows.iter().filter_map(|r| r.parent.clone()).filter(|p| !p.is_empty()).collect::<BTreeSet<_>>().into_iter().collect()
}

/// An epic is started when any child's lifecycle row has left the queue: an aeon holds it
/// (WORKING) or the builder has handed it on (SUBMITTED onwards, or over) — what bd's
/// "in progress, closed, or open with the submitted label" meant (epic_parent_lookup's
/// rule). A child read from its lifecycle row, never bd status (design §3.4, sp-mve9i); a
/// child with no row has not started anything.
pub fn epic_started(children: &[ReadyRow], lc: &HashMap<String, LifecycleRow>) -> bool {
    children.iter().any(|c| lc.get(&c.id).is_some_and(|r| !matches!(r.state, BeadState::Ready | BeadState::Rework)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ranked {
    pub epic_priority: i64,
    /// 0 = the epic has started (sorts first).
    pub epic_started: u8,
    pub bead_priority: i64,
    /// 0 = an unheld REWORK bead with a tip: ranks ahead of all fresh READY work, and
    /// ignores epic priority and started-ness among its own kind.
    pub rework: u8,
    /// 0 = resumable (sorts first).
    pub resumable: u8,
    pub age: String,
    pub id: String,
    pub epic_id: String,
}

impl Ranked {
    fn tier(&self) -> (u8, i64, u8, i64) {
        if self.rework == 0 {
            (0, 0, 0, self.bead_priority)
        } else {
            (1, self.epic_priority, self.epic_started, self.bead_priority)
        }
    }
    fn key(&self) -> ((u8, i64, u8, i64), u8, &str, &str, &str) {
        (self.tier(), self.resumable, &self.age, &self.id, &self.epic_id)
    }
    pub fn tsv(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.epic_priority, self.epic_started, self.bead_priority, self.resumable, self.age, self.id, self.epic_id
        )
    }
}

pub fn rank_one(r: &ReadyRow, lookup: &EpicLookup, resumable: &BTreeSet<String>, rework: &BTreeSet<String>) -> Ranked {
    let pid = r.parent.clone().unwrap_or_default();
    let started: BTreeSet<&str> = lookup.started.iter().map(String::as_str).collect();
    let epic_priority = if pid.is_empty() { r.prio() } else { (*lookup.prio.get(&pid).unwrap_or(&r.prio())).min(r.prio()) };
    Ranked {
        epic_priority,
        epic_started: if started.contains(pid.as_str()) { 0 } else { 1 },
        bead_priority: r.prio(),
        rework: if rework.contains(&r.id) { 0 } else { 1 },
        resumable: if resumable.contains(&r.id) { 0 } else { 1 },
        age: r.created_at.clone().or_else(|| r.updated_at.clone()).unwrap_or_default(),
        id: r.id.clone(),
        epic_id: if pid.is_empty() { r.id.clone() } else { pid },
    }
}

/// Best first. Epic/event rows are dropped defensively (READY_ARGS excludes them already).
pub fn rank(rows: &[ReadyRow], lookup: &EpicLookup, resumable: &BTreeSet<String>, rework: &BTreeSet<String>) -> Vec<Ranked> {
    let mut v: Vec<Ranked> = rows.iter().filter(|r| !is_container(r)).map(|r| rank_one(r, lookup, resumable, rework)).collect();
    v.sort_by(|a, b| a.key().cmp(&b.key()));
    v
}

fn is_container(r: &ReadyRow) -> bool {
    matches!(r.issue_type.as_deref(), Some("epic") | Some("event"))
}

/// aeon.sh's band lines for the best tier — REWORK beads by bead priority, else the best
/// (epic priority, epic started, bead priority): `id|branch|repo|eprio|estarted|bprio`,
/// in input order.
pub fn top_tier(rows: &[ReadyRow], lookup: &EpicLookup, rework: &BTreeSet<String>) -> Vec<String> {
    let none = BTreeSet::new();
    let ranked: Vec<(&ReadyRow, Ranked)> =
        rows.iter().filter(|r| !is_container(r)).map(|r| (r, rank_one(r, lookup, &none, rework))).collect();
    let Some(best) = ranked.iter().map(|(_, k)| k.tier()).min() else {
        return Vec::new();
    };
    let top = (best.1, best.2, best.3);
    ranked
        .iter()
        .filter(|(_, k)| k.tier() == best)
        .map(|(r, _)| {
            format!(
                "{}|{}|{}|{}|{}|{}",
                r.id,
                r.label_value("branch:").unwrap_or(""),
                r.label_value("repo:").unwrap_or(""),
                top.0,
                top.1,
                top.2
            )
        })
        .collect()
}

/// Ids from a resumable file: comma- or whitespace-separated.
pub fn parse_id_set(text: &str) -> BTreeSet<String> {
    text.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

// ---- machine mode: the stacked-dependents claim rule --------------------------------------

/// A `spira-lc list` row. Dolt returns columns string-valued, so each field accepts either
/// the string or its native JSON type.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleRow {
    pub bead_id: String,
    pub state: BeadState,
    pub holds: BTreeSet<HoldKind>,
    pub stack_depth: u32,
    /// The row's own certified tip — `None` for a row never submitted, or one whose tip
    /// column came back empty. The stack proposal a dependent builds names this, not the
    /// bd branch head, so a prerequisite that never certifies is never stackable regardless
    /// of state.
    pub tip: Option<String>,
    /// An unexpired `wait` snooze's end, resolved against the clock at parse time.
    pub snoozed_until: Option<i64>,
}

pub fn parse_lifecycle(text: &str) -> Result<HashMap<String, LifecycleRow>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty lifecycle snapshot".into());
    }
    let v: Value = serde_json::from_str(t).map_err(|e| format!("lifecycle snapshot is not JSON: {e}"))?;
    let arr = match v {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        other => vec![other],
    };
    let mut out = HashMap::new();
    for r in arr {
        let id = r.get("bead_id").and_then(Value::as_str).ok_or_else(|| format!("lifecycle row without bead_id: {r}"))?;
        let st = r.get("state").and_then(Value::as_str).unwrap_or("");
        let state = BeadState::from_str(st).ok_or_else(|| format!("lifecycle row {id}: bad state {st:?}"))?;
        let holds_v = match r.get("holds") {
            Some(Value::String(s)) if s.trim().is_empty() => Value::Null,
            Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| format!("lifecycle row {id}: holds: {e}"))?,
            Some(v) => v.clone(),
            None => Value::Null,
        };
        let mut holds = BTreeSet::new();
        for h in holds_v.as_array().into_iter().flatten() {
            let s = h.as_str().unwrap_or("");
            // An unknown hold kind refuses the bead rather than being ignored.
            holds.insert(HoldKind::from_str(s).unwrap_or(HoldKind::Manual));
        }
        let stack_depth = match r.get("stack_depth") {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        };
        let tip = match r.get("tip") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        let snoozed_until = match r.get("reason") {
            Some(Value::String(reason)) if holds.contains(&HoldKind::Wait) => {
                spira_config::lc_state::snooze_until(reason).filter(|t| *t > crate::ready::now_epoch())
            }
            _ => None,
        };
        out.insert(id.to_string(), LifecycleRow { bead_id: id.to_string(), state, holds, stack_depth, tip, snoozed_until });
    }
    Ok(out)
}

/// A blocker's bd record (bd list --id … --status all).
pub fn index_rows(rows: Vec<ReadyRow>) -> HashMap<String, ReadyRow> {
    rows.into_iter().map(|r| (r.id.clone(), r)).collect()
}

/// The `blocks` targets of a candidate. A `blocks` edge onto the candidate's own parent is read as
/// parent-child: a container closes only when its children do, so it could never clear.
pub fn blockers(r: &ReadyRow) -> Vec<String> {
    r.dependencies
        .iter()
        .filter(|d| d.dep_type.as_deref() == Some("blocks"))
        .filter(|d| d.issue_id.as_deref().map_or(true, |i| i == r.id))
        .filter_map(|d| d.depends_on_id.clone())
        .filter(|b| r.parent.as_deref() != Some(b.as_str()))
        .collect()
}

pub fn all_blockers(rows: &[ReadyRow]) -> Vec<String> {
    rows.iter().flat_map(blockers).collect::<BTreeSet<_>>().into_iter().collect()
}

/// A work bead is anything that is not a container/decision/event.
fn is_work(r: &ReadyRow) -> bool {
    !matches!(r.issue_type.as_deref(), Some("epic") | Some("decision") | Some("event") | Some("molecule") | Some("gate"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Claimable { depth: u32 },
    NoOwnRow,
    OwnState(BeadState),
    Held(HoldKind),
    Snoozed(i64),
    Blocked(String),
    TooDeep { depth: u32, max: u32 },
}

/// An incident waits for its fix to land: a certified-only fix is not a reason to summon Ops,
/// so an incident-labelled candidate never stacks.
pub fn stack_cap(cand: &ReadyRow, incident_label: &str, stack_max_depth: u32) -> u32 {
    if !incident_label.is_empty() && cand.labels.iter().any(|l| l == incident_label) {
        0
    } else {
        stack_max_depth
    }
}

pub fn claimable(
    cand: &ReadyRow,
    lc: &HashMap<String, LifecycleRow>,
    bd: &HashMap<String, ReadyRow>,
    stack_max_depth: u32,
) -> Verdict {
    match stack_plan(cand, lc, bd, stack_max_depth) {
        Ok((_, depth)) => Verdict::Claimable { depth },
        Err(v) => v,
    }
}

/// The same claimability rule as [`claimable`], but also returning the stack proposal a
/// claim of `cand` would carry: `{prereq_bead_id: certified_tip}` for every blocker this
/// rule stacked on rather than waited for. `Err(Verdict::TooDeep { depth, .. })` still
/// carries the depth the proposal would have reached, so a caller can hand it to the
/// machine's own `Claim` event and let `DepthExceeded` be the refusal of record rather than
/// duplicating the ceiling check here.
pub fn stack_plan(
    cand: &ReadyRow,
    lc: &HashMap<String, LifecycleRow>,
    bd: &HashMap<String, ReadyRow>,
    stack_max_depth: u32,
) -> Result<(Stack, u32), Verdict> {
    let Some(own) = lc.get(&cand.id) else { return Err(Verdict::NoOwnRow) };
    if let Some(h) = own.holds.iter().find(|h| **h != HoldKind::Wait) {
        return Err(Verdict::Held(*h));
    }
    if let Some(t) = own.snoozed_until {
        return Err(Verdict::Snoozed(t));
    }
    if !matches!(own.state, BeadState::Ready | BeadState::Rework) {
        return Err(Verdict::OwnState(own.state));
    }
    let own_repo = cand.label_value("repo:");
    let mut stack = Stack::new();
    let mut stacked_max: Option<u32> = None;
    for bid in blockers(cand) {
        let rec = bd.get(&bid);
        let ok = match lc.get(&bid) {
            None => rec.is_some_and(rowless_blocker_done),
            Some(row) => {
                let stackable = stack_max_depth > 0
                    && rec.is_some_and(|r| is_work(r) && r.label_value("repo:") == own_repo && own_repo.is_some());
                match row.state {
                    BeadState::Landed | BeadState::Done => true,
                    BeadState::Certified | BeadState::InDelivery if stackable => {
                        stacked_max = Some(stacked_max.unwrap_or(0).max(row.stack_depth));
                        if let Some(tip) = &row.tip {
                            stack.insert(bid.clone(), tip.clone());
                        }
                        true
                    }
                    // SUPERSEDED / DROPPED: nothing more will happen to the blocker, so
                    // waiting on it would wait forever (the machine's word, not bd's).
                    s => s.is_terminal(),
                }
            }
        };
        if !ok {
            return Err(Verdict::Blocked(bid));
        }
    }
    let depth = stacked_max.map_or(0, |d| d + 1);
    if depth > stack_max_depth {
        return Err(Verdict::TooDeep { depth, max: stack_max_depth });
    }
    Ok((stack, depth))
}

/// A blocker with no lifecycle row (DESIGN.md §2, sp-mve9i). A container, decision, event,
/// molecule or gate bead is not a work bead: the machine never holds it, and bd's status is
/// its whole lifecycle, read through `spira_config::nonwork` naming the kind. A work bead
/// with no row is not live work — every work bead gets its row at creation, and CHECK-ROWLESS
/// backfills a READY row for a live one that missed it, at which point it blocks by its row —
/// so it holds nothing back (the reading the sentinel's backlog and the claim witness give a
/// rowless bead).
fn rowless_blocker_done(r: &ReadyRow) -> bool {
    use spira_config::nonwork::{self, Kind};
    let kind = match r.issue_type.as_deref() {
        Some("gate") => Kind::Hold,
        Some("epic") | Some("decision") | Some("event") | Some("molecule") => Kind::Epic,
        _ => return true,
    };
    nonwork::is_closed(kind, r.status.as_deref().unwrap_or(""))
}

/// The hard ceiling the spira-config schema enforces (stacked-dependents §1).
pub const STACK_CEILING: u32 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, prio: i64, parent: Option<&str>, created: &str) -> ReadyRow {
        ReadyRow {
            id: id.into(),
            priority: Some(prio),
            parent: parent.map(str::to_string),
            created_at: Some(created.into()),
            issue_type: Some("task".into()),
            status: Some("open".into()),
            labels: vec!["repo:spira".into(), format!("branch:spira/{id}")],
            ..Default::default()
        }
    }

    fn ids(r: &[Ranked]) -> Vec<&str> {
        r.iter().map(|k| k.id.as_str()).collect()
    }

    #[test]
    fn epic_priority_ranks_first() {
        let rows = vec![row("a", 2, Some("E2"), "2026-01-01"), row("b", 3, Some("E1"), "2026-01-02")];
        let lk = EpicLookup { prio: [("E2".into(), 2), ("E1".into(), 1)].into(), started: vec![] };
        assert_eq!(ids(&rank(&rows, &lk, &BTreeSet::new(), &BTreeSet::new())), ["b", "a"]);
    }

    #[test]
    fn a_p0_child_of_a_p1_epic_outranks_a_standalone_p1() {
        let rows = vec![row("solo", 1, None, "2026-01-01"), row("kid", 0, Some("E"), "2026-02-01")];
        let lk = EpicLookup { prio: [("E".into(), 1)].into(), started: vec![] };
        let r = rank(&rows, &lk, &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(ids(&r), ["kid", "solo"]);
        assert_eq!(r[0].epic_priority, 0);
    }

    #[test]
    fn equal_effective_priority_keeps_epic_first_order() {
        let rows = vec![row("a", 1, Some("E1"), "2026-01-01"), row("b", 0, Some("E2"), "2026-01-02")];
        let lk = EpicLookup { prio: [("E1".into(), 1), ("E2".into(), 1)].into(), started: vec!["E1".into()] };
        assert_eq!(ids(&rank(&rows, &lk, &BTreeSet::new(), &BTreeSet::new())), ["b", "a"]);
        let lk = EpicLookup { prio: [("E1".into(), 0), ("E2".into(), 1)].into(), started: vec!["E1".into()] };
        let rows = vec![row("b", 0, Some("E2"), "2026-01-02"), row("a", 1, Some("E1"), "2026-01-01")];
        assert_eq!(ids(&rank(&rows, &lk, &BTreeSet::new(), &BTreeSet::new())), ["a", "b"]);
    }

    #[test]
    fn started_epic_first_at_equal_priority() {
        let rows = vec![row("a", 1, Some("E1"), "2026-01-01"), row("b", 1, Some("E2"), "2026-01-02")];
        let lk = EpicLookup { prio: [("E1".into(), 1), ("E2".into(), 1)].into(), started: vec!["E2".into()] };
        assert_eq!(ids(&rank(&rows, &lk, &BTreeSet::new(), &BTreeSet::new())), ["b", "a"]);
    }

    #[test]
    fn bead_priority_within_epic_then_resumable_then_oldest() {
        let rows = vec![
            row("old", 1, Some("E"), "2026-01-01"),
            row("new", 1, Some("E"), "2026-02-01"),
            row("res", 1, Some("E"), "2026-03-01"),
            row("hi", 0, Some("E"), "2026-04-01"),
        ];
        let lk = EpicLookup { prio: [("E".into(), 0)].into(), started: vec![] };
        let res: BTreeSet<String> = ["res".to_string()].into();
        assert_eq!(ids(&rank(&rows, &lk, &res, &BTreeSet::new())), ["hi", "res", "old", "new"]);
    }

    #[test]
    fn unaffiliated_bead_is_its_own_epic_at_its_own_priority() {
        let rows = vec![row("solo", 1, None, "2026-01-01"), row("kid", 2, Some("E"), "2026-01-01")];
        let lk = EpicLookup { prio: [("E".into(), 0)].into(), started: vec![] };
        let r = rank(&rows, &lk, &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(ids(&r), ["kid", "solo"]);
        assert_eq!(r[1].epic_id, "solo");
        assert_eq!(r[1].epic_started, 1);
    }

    #[test]
    fn epic_missing_from_lookup_uses_bead_priority() {
        let rows = vec![row("a", 2, Some("Egone"), "2026-01-01"), row("b", 1, None, "2026-01-01")];
        assert_eq!(ids(&rank(&rows, &EpicLookup::default(), &BTreeSet::new(), &BTreeSet::new())), ["b", "a"]);
    }

    #[test]
    fn missing_priority_is_99_and_age_falls_back_to_updated() {
        let mut a = row("a", 0, None, "");
        a.priority = None;
        a.created_at = None;
        a.updated_at = Some("2026-05-05".into());
        let r = rank_one(&a, &EpicLookup::default(), &BTreeSet::new(), &BTreeSet::new());
        assert_eq!((r.epic_priority, r.bead_priority, r.age.as_str()), (99, 99, "2026-05-05"));
    }

    #[test]
    fn ties_break_on_id_deterministically() {
        let rows = vec![row("b", 1, None, "t"), row("a", 1, None, "t")];
        assert_eq!(ids(&rank(&rows, &EpicLookup::default(), &BTreeSet::new(), &BTreeSet::new())), ["a", "b"]);
    }

    #[test]
    fn epics_are_never_ranked() {
        let mut e = row("E", 0, None, "t");
        e.issue_type = Some("epic".into());
        assert!(rank(&[e.clone()], &EpicLookup::default(), &BTreeSet::new(), &BTreeSet::new()).is_empty());
        assert!(top_tier(&[e], &EpicLookup::default(), &BTreeSet::new()).is_empty());
    }

    #[test]
    fn tsv_matches_epic_rank_rows_columns() {
        let r = rank_one(&row("sp-a", 1, Some("sp-E"), "2026-01-01T00:00:00Z"), &EpicLookup { prio: [("sp-E".into(), 0)].into(), started: vec!["sp-E".into()] }, &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(r.tsv(), "0\t0\t1\t1\t2026-01-01T00:00:00Z\tsp-a\tsp-E");
    }

    #[test]
    fn rework_outranks_fresh_ready_whatever_its_priority_and_orders_by_priority_then_age() {
        let rows = vec![
            row("p1ready", 1, None, "2026-01-01"),
            row("p0ready", 0, None, "2026-01-01"),
            row("p2rw", 2, None, "2026-03-01"),
            row("p2rw-old", 2, None, "2026-02-01"),
            row("p1rw", 1, Some("E"), "2026-04-01"),
        ];
        let lk = EpicLookup { prio: [("E".into(), 3)].into(), started: vec![] };
        let rw: BTreeSet<String> = ["p2rw", "p2rw-old", "p1rw"].map(String::from).into();
        let r = rank(&rows, &lk, &BTreeSet::new(), &rw);
        assert_eq!(ids(&r), ["p1rw", "p2rw-old", "p2rw", "p0ready", "p1ready"]);
        assert_eq!(top_tier(&rows, &lk, &rw), ["p1rw|spira/p1rw|spira|0|0|1"]);
    }

    #[test]
    fn top_tier_lines() {
        let rows = vec![row("a", 1, Some("E"), "t"), row("b", 1, Some("E"), "t"), row("c", 2, Some("E"), "t")];
        let lk = EpicLookup { prio: [("E".into(), 0)].into(), started: vec!["E".into()] };
        assert_eq!(top_tier(&rows, &lk, &BTreeSet::new()), ["a|spira/a|spira|0|0|1", "b|spira/b|spira|0|0|1"]);
    }

    /// sp-mve9i: a child's progress is its lifecycle row, whatever bd's status says.
    #[test]
    fn epic_started_rule() {
        let mut c = row("c", 1, None, "t");
        c.status = Some("in_progress".into());
        let at = |st: BeadState| HashMap::from([("c".to_string(), LifecycleRow { bead_id: "c".into(), state: st, holds: BTreeSet::new(), stack_depth: 0, tip: None, snoozed_until: None })]);
        assert!(!epic_started(&[c.clone()], &HashMap::new()), "no row: nothing started, though bd says in_progress");
        assert!(!epic_started(&[c.clone()], &at(BeadState::Ready)));
        assert!(!epic_started(&[c.clone()], &at(BeadState::Rework)));
        assert!(epic_started(&[c.clone()], &at(BeadState::Working)));
        assert!(epic_started(&[c.clone()], &at(BeadState::Submitted)));
        assert!(epic_started(&[c], &at(BeadState::Landed)));
    }

    #[test]
    fn parse_ready_contract() {
        assert!(parse_ready("").unwrap().is_empty());
        assert!(parse_ready("null").unwrap().is_empty());
        assert!(parse_ready("not json").is_err());
        assert!(parse_ready(r#"[{"title":"no id"}]"#).is_err());
        let r = parse_ready(r#"{"id":"a","priority":"2","labels":null,"dependencies":null}"#).unwrap();
        assert_eq!(r[0].priority, Some(2));
    }

    #[test]
    fn large_ready_set_over_128k() {
        // sp-o4trx: 142 beads was already past MAX_ARG_STRLEN. The whole path is in-memory
        // and on stdin; 3000 fat rows must rank.
        let mut s = String::from("[");
        for i in 0..3000 {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                r#"{{"id":"sp-{i:05}","priority":{},"parent":"sp-E{}","created_at":"2026-01-01T00:00:{:02}Z","labels":["repo:spira","plan"],"description":"{}"}}"#,
                i % 4,
                i % 7,
                i % 60,
                "x".repeat(200)
            ));
        }
        s.push(']');
        assert!(s.len() > 128 * 1024);
        let rows = parse_ready(&s).unwrap();
        let r = rank(&rows, &EpicLookup::default(), &BTreeSet::new(), &BTreeSet::new());
        assert_eq!(r.len(), 3000);
        assert_eq!(r[0].bead_priority, 0);
        assert_eq!(parents(&rows).len(), 7);
    }

    // ---- machine mode ----

    fn lcrow(id: &str, st: BeadState, holds: &[HoldKind], depth: u32) -> (String, LifecycleRow) {
        (id.into(), LifecycleRow { bead_id: id.into(), state: st, holds: holds.iter().copied().collect(), stack_depth: depth, tip: Some(format!("tip-{id}")), snoozed_until: None })
    }

    fn blocked_on(id: &str, blocker: &str) -> ReadyRow {
        let mut r = row(id, 1, None, "t");
        r.dependencies.push(Dependency { issue_id: Some(id.into()), depends_on_id: Some(blocker.into()), dep_type: Some("blocks".into()) });
        r
    }

    fn bdrec(id: &str, status: &str, ty: &str, repo: &str) -> (String, ReadyRow) {
        let mut r = row(id, 1, None, "t");
        r.status = Some(status.into());
        r.issue_type = Some(ty.into());
        r.labels = vec![format!("repo:{repo}")];
        (id.into(), r)
    }

    #[test]
    fn an_incident_never_stacks_on_a_certified_fix_but_claims_once_it_lands() {
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        let mut inc = blocked_on("B", "A");
        inc.labels.push("incident".into());
        let certified: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[HoldKind::Wait], 0), lcrow("A", BeadState::Certified, &[], 0)].into();
        let cap = stack_cap(&inc, "incident", 4);
        assert_eq!(claimable(&inc, &certified, &bd, cap), Verdict::Blocked("A".into()));
        assert_eq!(claimable(&blocked_on("B", "A"), &certified, &bd, stack_cap(&blocked_on("B", "A"), "incident", 4)), Verdict::Claimable { depth: 1 }, "a non-incident still stacks");
        let landed: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[HoldKind::Wait], 0), lcrow("A", BeadState::Landed, &[], 0)].into();
        assert_eq!(claimable(&inc, &landed, &bd, cap), Verdict::Claimable { depth: 0 });
    }

    #[test]
    fn stacked_blocker_certified_is_claimable() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[HoldKind::Wait], 0), lcrow("A", BeadState::Certified, &[], 0)].into();
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 4), Verdict::Claimable { depth: 1 });
    }

    #[test]
    fn stack_plan_names_the_stacked_prerequisites_tip() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Certified, &[], 0)].into();
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        let (stack, depth) = stack_plan(&blocked_on("B", "A"), &lc, &bd, 4).unwrap();
        assert_eq!(depth, 1);
        assert_eq!(stack.get("A").map(String::as_str), Some("tip-A"));
    }

    #[test]
    fn stack_plan_omits_a_landed_blocker_it_does_not_stack_on() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Landed, &[], 0)].into();
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        let (stack, depth) = stack_plan(&blocked_on("B", "A"), &lc, &bd, 4).unwrap();
        assert_eq!(depth, 0);
        assert!(stack.is_empty(), "a landed prerequisite is satisfied, not stacked on");
    }

    #[test]
    fn stack_plan_too_deep_still_reports_the_attempted_stack() {
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::InDelivery, &[], 4)].into();
        let err = stack_plan(&blocked_on("B", "A"), &lc, &bd, 4).unwrap_err();
        assert_eq!(err, Verdict::TooDeep { depth: 5, max: 4 });
    }

    #[test]
    fn stacked_blocker_only_submitted_waits() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Submitted, &[], 0)].into();
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 4), Verdict::Blocked("A".into()));
    }

    #[test]
    fn a_blocker_landing_releases_its_dependent_with_no_hand_step_but_a_manual_hold_never_lifts() {
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        let dep = blocked_on("B", "A");
        for blocker in [BeadState::Working, BeadState::Submitted] {
            let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", blocker, &[], 0)].into();
            assert_eq!(claimable(&dep, &lc, &bd, 0), Verdict::Blocked("A".into()), "{blocker:?}");
        }
        let landed: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Landed, &[], 0)].into();
        assert_eq!(claimable(&dep, &landed, &bd, 0), Verdict::Claimable { depth: 0 });
        let parked: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[HoldKind::Manual], 0), lcrow("A", BeadState::Landed, &[], 0)].into();
        assert_eq!(claimable(&dep, &parked, &bd, 0), Verdict::Held(HoldKind::Manual));
    }

    #[test]
    fn blocker_epic_waits_for_close() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0)].into();
        let open: HashMap<_, _> = [bdrec("E", "open", "epic", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "E"), &lc, &open, 4), Verdict::Blocked("E".into()));
        let closed: HashMap<_, _> = [bdrec("E", "closed", "epic", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "E"), &lc, &closed, 4), Verdict::Claimable { depth: 0 });
        // Even an epic with a lifecycle row certified does not stack.
        let lc2: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("E", BeadState::Certified, &[], 0)].into();
        assert_eq!(claimable(&blocked_on("B", "E"), &lc2, &open, 4), Verdict::Blocked("E".into()));
    }

    #[test]
    fn a_blocks_edge_onto_the_candidates_own_open_epic_does_not_block() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0)].into();
        let open: HashMap<_, _> = [bdrec("E", "open", "epic", "spira")].into();
        let mut child = blocked_on("B", "E");
        child.parent = Some("E".into());
        assert_eq!(claimable(&child, &lc, &open, 4), Verdict::Claimable { depth: 0 });
    }

    #[test]
    fn cross_repo_blocker_waits_for_landed() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Certified, &[], 0)].into();
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "other")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 4), Verdict::Blocked("A".into()));
        let lc2: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Landed, &[], 0)].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc2, &bd, 4), Verdict::Claimable { depth: 0 });
    }

    #[test]
    fn depth_capped_stack() {
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Certified, &[], 3)].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 4), Verdict::Claimable { depth: 4 });
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::InDelivery, &[], 4)].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 4), Verdict::TooDeep { depth: 5, max: 4 });
    }

    #[test]
    fn stack_max_depth_zero_is_todays_rule() {
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Certified, &[], 0)].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 0), Verdict::Blocked("A".into()));
    }

    fn snooze_snapshot(until: i64, hold: &str) -> HashMap<String, LifecycleRow> {
        let reason = spira_config::lc_state::snooze_reason(until);
        parse_lifecycle(&format!(r#"[{{"bead_id":"B","state":"READY","holds":["{hold}"],"reason":"{reason}"}}]"#)).unwrap()
    }

    #[test]
    fn an_unexpired_wait_snooze_is_not_claimable_and_an_expired_one_is() {
        let bd = HashMap::new();
        let b = row("B", 1, None, "t");
        let future = crate::ready::now_epoch() + 3600;
        assert_eq!(claimable(&b, &snooze_snapshot(future, "wait"), &bd, 4), Verdict::Snoozed(future));
        assert_eq!(claimable(&b, &snooze_snapshot(crate::ready::now_epoch() - 1, "wait"), &bd, 4), Verdict::Claimable { depth: 0 });
        assert_eq!(claimable(&b, &snooze_snapshot(future, "manual"), &bd, 4), Verdict::Held(HoldKind::Manual), "only a wait hold snoozes");
        assert!(crate::ready::lifecycle_ready_ids(&snooze_snapshot(future, "wait")).is_empty());
    }

    #[test]
    fn own_row_rules() {
        let bd = HashMap::new();
        let b = row("B", 1, None, "t");
        assert_eq!(claimable(&b, &HashMap::new(), &bd, 4), Verdict::NoOwnRow);
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[HoldKind::Poison], 0)].into();
        assert_eq!(claimable(&b, &lc, &bd, 4), Verdict::Held(HoldKind::Poison));
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Working, &[], 0)].into();
        assert_eq!(claimable(&b, &lc, &bd, 4), Verdict::OwnState(BeadState::Working));
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Rework, &[HoldKind::Wait], 0)].into();
        assert_eq!(claimable(&b, &lc, &bd, 4), Verdict::Claimable { depth: 0 });
    }

    /// sp-mve9i: a rowless work blocker is not live work — bd's status is not read for it;
    /// a rowless gate is a non-work bead whose bd status is its state.
    #[test]
    fn blocker_with_no_lifecycle_row() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0)].into();
        let open: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &open, 4), Verdict::Claimable { depth: 0 }, "bd's open is not read for a work bead");
        // unknown to bd too: blocked (fail closed)
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &HashMap::new(), 4), Verdict::Blocked("A".into()));
        let gate: HashMap<_, _> = [bdrec("A", "open", "gate", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &gate, 4), Verdict::Blocked("A".into()));
        let gate: HashMap<_, _> = [bdrec("A", "closed", "gate", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &gate, 4), Verdict::Claimable { depth: 0 });
    }

    /// A SUPERSEDED or DROPPED blocker is over: the machine's terminal word releases it,
    /// whatever bd's status still says.
    #[test]
    fn terminal_blocker_is_done_by_the_machine() {
        let open: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        for st in [BeadState::Superseded, BeadState::Dropped] {
            let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", st, &[], 0)].into();
            assert_eq!(claimable(&blocked_on("B", "A"), &lc, &open, 4), Verdict::Claimable { depth: 0 }, "{st:?}");
        }
    }

    #[test]
    fn parent_child_and_relates_edges_do_not_block() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0)].into();
        let mut b = row("B", 1, Some("E"), "t");
        b.dependencies.push(Dependency { issue_id: Some("B".into()), depends_on_id: Some("E".into()), dep_type: Some("parent-child".into()) });
        b.dependencies.push(Dependency { issue_id: Some("B".into()), depends_on_id: Some("X".into()), dep_type: Some("relates-to".into()) });
        assert_eq!(claimable(&b, &lc, &HashMap::new(), 4), Verdict::Claimable { depth: 0 });
    }

    #[test]
    fn parse_lifecycle_string_valued_rows() {
        let m = parse_lifecycle(r#"[{"bead_id":"a","state":"CERTIFIED","holds":"[\"wait\"]","stack_depth":"2"},{"bead_id":"b","state":"READY","holds":"","version":"3"}]"#).unwrap();
        assert_eq!(m["a"].state, BeadState::Certified);
        assert!(m["a"].holds.contains(&HoldKind::Wait));
        assert_eq!(m["a"].stack_depth, 2);
        assert!(m["b"].holds.is_empty());
        assert!(parse_lifecycle("").is_err());
        assert!(parse_lifecycle(r#"[{"bead_id":"a","state":"NOPE"}]"#).is_err());
    }
}
