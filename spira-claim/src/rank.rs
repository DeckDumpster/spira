//! Claim selection: the stacked-dependents claimability rule (machine mode) and the
//! epic-first rank (sp-ns46j). Pure. DESIGN.md §2 `select`, §3.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use lifecycle::bead::{BeadState, HoldKind};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// A `bd ready`/`bd list --json` row — only the fields read.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ReadyRow {
    pub id: String,
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

/// An epic is started when any child is in progress or closed, or open with the submitted
/// label (epic_parent_lookup's rule).
pub fn epic_started(children: &[ReadyRow], submitted_label: &str) -> bool {
    children.iter().any(|c| match c.status.as_deref() {
        Some("in_progress") | Some("closed") => true,
        Some("open") => c.labels.iter().any(|l| l == submitted_label),
        _ => false,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ranked {
    pub epic_priority: i64,
    /// 0 = the epic has started (sorts first).
    pub epic_started: u8,
    pub bead_priority: i64,
    /// 0 = resumable (sorts first).
    pub resumable: u8,
    pub age: String,
    pub id: String,
    pub epic_id: String,
}

impl Ranked {
    fn key(&self) -> (i64, u8, i64, u8, &str, &str, &str) {
        (self.epic_priority, self.epic_started, self.bead_priority, self.resumable, &self.age, &self.id, &self.epic_id)
    }
    pub fn tsv(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.epic_priority, self.epic_started, self.bead_priority, self.resumable, self.age, self.id, self.epic_id
        )
    }
}

pub fn rank_one(r: &ReadyRow, lookup: &EpicLookup, resumable: &BTreeSet<String>) -> Ranked {
    let pid = r.parent.clone().unwrap_or_default();
    let started: BTreeSet<&str> = lookup.started.iter().map(String::as_str).collect();
    let epic_priority = if pid.is_empty() { r.prio() } else { *lookup.prio.get(&pid).unwrap_or(&r.prio()) };
    Ranked {
        epic_priority,
        epic_started: if started.contains(pid.as_str()) { 0 } else { 1 },
        bead_priority: r.prio(),
        resumable: if resumable.contains(&r.id) { 0 } else { 1 },
        age: r.created_at.clone().or_else(|| r.updated_at.clone()).unwrap_or_default(),
        id: r.id.clone(),
        epic_id: if pid.is_empty() { r.id.clone() } else { pid },
    }
}

/// Best first. Epic/event rows are dropped defensively (READY_ARGS excludes them already).
pub fn rank(rows: &[ReadyRow], lookup: &EpicLookup, resumable: &BTreeSet<String>) -> Vec<Ranked> {
    let mut v: Vec<Ranked> = rows.iter().filter(|r| !is_container(r)).map(|r| rank_one(r, lookup, resumable)).collect();
    v.sort_by(|a, b| a.key().cmp(&b.key()));
    v
}

fn is_container(r: &ReadyRow) -> bool {
    matches!(r.issue_type.as_deref(), Some("epic") | Some("event"))
}

/// aeon.sh's band lines for the best (epic priority, epic started, bead priority) tier:
/// `id|branch|repo|eprio|estarted|bprio`, in input order.
pub fn top_tier(rows: &[ReadyRow], lookup: &EpicLookup) -> Vec<String> {
    let none = BTreeSet::new();
    let ranked: Vec<(&ReadyRow, Ranked)> =
        rows.iter().filter(|r| !is_container(r)).map(|r| (r, rank_one(r, lookup, &none))).collect();
    let Some(top) = ranked.iter().map(|(_, k)| (k.epic_priority, k.epic_started, k.bead_priority)).min() else {
        return Vec::new();
    };
    ranked
        .iter()
        .filter(|(_, k)| (k.epic_priority, k.epic_started, k.bead_priority) == top)
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
            holds.insert(HoldKind::from_str(s).unwrap_or(HoldKind::Operator));
        }
        let stack_depth = match r.get("stack_depth") {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        };
        out.insert(id.to_string(), LifecycleRow { bead_id: id.to_string(), state, holds, stack_depth });
    }
    Ok(out)
}

/// A blocker's bd record (bd list --id … --status all).
pub fn index_rows(rows: Vec<ReadyRow>) -> HashMap<String, ReadyRow> {
    rows.into_iter().map(|r| (r.id.clone(), r)).collect()
}

/// The `blocks` targets of a candidate.
pub fn blockers(r: &ReadyRow) -> Vec<String> {
    r.dependencies
        .iter()
        .filter(|d| d.dep_type.as_deref() == Some("blocks"))
        .filter(|d| d.issue_id.as_deref().map_or(true, |i| i == r.id))
        .filter_map(|d| d.depends_on_id.clone())
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
    Blocked(String),
    TooDeep { depth: u32, max: u32 },
}

pub fn claimable(
    cand: &ReadyRow,
    lc: &HashMap<String, LifecycleRow>,
    bd: &HashMap<String, ReadyRow>,
    stack_max_depth: u32,
) -> Verdict {
    let Some(own) = lc.get(&cand.id) else { return Verdict::NoOwnRow };
    if let Some(h) = own.holds.iter().find(|h| **h != HoldKind::Wait) {
        return Verdict::Held(*h);
    }
    if !matches!(own.state, BeadState::Ready | BeadState::Rework) {
        return Verdict::OwnState(own.state);
    }
    let own_repo = cand.label_value("repo:");
    let mut stacked_max: Option<u32> = None;
    for bid in blockers(cand) {
        let rec = bd.get(&bid);
        let bd_closed = rec.and_then(|r| r.status.as_deref()) == Some("closed");
        let ok = match lc.get(&bid) {
            None => bd_closed,
            Some(row) => {
                let stackable = stack_max_depth > 0
                    && rec.is_some_and(|r| is_work(r) && r.label_value("repo:") == own_repo && own_repo.is_some());
                match row.state {
                    BeadState::Landed | BeadState::Done => true,
                    BeadState::Certified | BeadState::InDelivery if stackable => {
                        stacked_max = Some(stacked_max.unwrap_or(0).max(row.stack_depth));
                        true
                    }
                    s if s.is_terminal() => bd_closed,
                    _ => false,
                }
            }
        };
        if !ok {
            return Verdict::Blocked(bid);
        }
    }
    let depth = stacked_max.map_or(0, |d| d + 1);
    if depth > stack_max_depth {
        return Verdict::TooDeep { depth, max: stack_max_depth };
    }
    Verdict::Claimable { depth }
}

/// The legacy poison record: the bd label CHECK 4 puts on a bead when `lifecycle_enforce`
/// is off (DESIGN.md §6a; unpoison's §8.7 clears the same label).
pub const POISON_LABEL: &str = "spira-poison"; // literal-ok: the label is the contract

/// `lifecycle_enforce` off: a bead labelled [`POISON_LABEL`] is not claimable.
pub fn poisoned_by_label(r: &ReadyRow) -> bool {
    r.labels.iter().any(|l| l == POISON_LABEL)
}

/// `--blockers machine` with `lifecycle_enforce` off (DESIGN.md §6a): the same question as
/// [`claimable`], answered from legacy records only — no lifecycle row exists to read. The
/// poison is the label; every `blocks` target must be bd-`closed` (the "no lifecycle row"
/// row of the §2 table, which is also what `bd ready` applies); nothing stacks, so depth
/// is 0.
pub fn claimable_legacy(cand: &ReadyRow, bd: &HashMap<String, ReadyRow>) -> Verdict {
    if poisoned_by_label(cand) {
        return Verdict::Held(HoldKind::Poison);
    }
    for bid in blockers(cand) {
        if bd.get(&bid).and_then(|r| r.status.as_deref()) != Some("closed") {
            return Verdict::Blocked(bid);
        }
    }
    Verdict::Claimable { depth: 0 }
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
        // a P0 bead in a P2 epic loses to a P3 bead in a P1 epic.
        let rows = vec![row("a", 0, Some("E2"), "2026-01-01"), row("b", 3, Some("E1"), "2026-01-02")];
        let lk = EpicLookup { prio: [("E2".into(), 2), ("E1".into(), 1)].into(), started: vec![] };
        assert_eq!(ids(&rank(&rows, &lk, &BTreeSet::new())), ["b", "a"]);
    }

    #[test]
    fn started_epic_first_at_equal_priority() {
        let rows = vec![row("a", 1, Some("E1"), "2026-01-01"), row("b", 1, Some("E2"), "2026-01-02")];
        let lk = EpicLookup { prio: [("E1".into(), 1), ("E2".into(), 1)].into(), started: vec!["E2".into()] };
        assert_eq!(ids(&rank(&rows, &lk, &BTreeSet::new())), ["b", "a"]);
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
        assert_eq!(ids(&rank(&rows, &lk, &res)), ["hi", "res", "old", "new"]);
    }

    #[test]
    fn unaffiliated_bead_is_its_own_epic_at_its_own_priority() {
        let rows = vec![row("solo", 1, None, "2026-01-01"), row("kid", 2, Some("E"), "2026-01-01")];
        let lk = EpicLookup { prio: [("E".into(), 0)].into(), started: vec![] };
        let r = rank(&rows, &lk, &BTreeSet::new());
        assert_eq!(ids(&r), ["kid", "solo"]);
        assert_eq!(r[1].epic_id, "solo");
        assert_eq!(r[1].epic_started, 1);
    }

    #[test]
    fn epic_missing_from_lookup_uses_bead_priority() {
        let rows = vec![row("a", 2, Some("Egone"), "2026-01-01"), row("b", 1, None, "2026-01-01")];
        assert_eq!(ids(&rank(&rows, &EpicLookup::default(), &BTreeSet::new())), ["b", "a"]);
    }

    #[test]
    fn missing_priority_is_99_and_age_falls_back_to_updated() {
        let mut a = row("a", 0, None, "");
        a.priority = None;
        a.created_at = None;
        a.updated_at = Some("2026-05-05".into());
        let r = rank_one(&a, &EpicLookup::default(), &BTreeSet::new());
        assert_eq!((r.epic_priority, r.bead_priority, r.age.as_str()), (99, 99, "2026-05-05"));
    }

    #[test]
    fn ties_break_on_id_deterministically() {
        let rows = vec![row("b", 1, None, "t"), row("a", 1, None, "t")];
        assert_eq!(ids(&rank(&rows, &EpicLookup::default(), &BTreeSet::new())), ["a", "b"]);
    }

    #[test]
    fn epics_are_never_ranked() {
        let mut e = row("E", 0, None, "t");
        e.issue_type = Some("epic".into());
        assert!(rank(&[e.clone()], &EpicLookup::default(), &BTreeSet::new()).is_empty());
        assert!(top_tier(&[e], &EpicLookup::default()).is_empty());
    }

    #[test]
    fn tsv_matches_epic_rank_rows_columns() {
        let r = rank_one(&row("sp-a", 1, Some("sp-E"), "2026-01-01T00:00:00Z"), &EpicLookup { prio: [("sp-E".into(), 0)].into(), started: vec!["sp-E".into()] }, &BTreeSet::new());
        assert_eq!(r.tsv(), "0\t0\t1\t1\t2026-01-01T00:00:00Z\tsp-a\tsp-E");
    }

    #[test]
    fn top_tier_lines() {
        let rows = vec![row("a", 1, Some("E"), "t"), row("b", 1, Some("E"), "t"), row("c", 2, Some("E"), "t")];
        let lk = EpicLookup { prio: [("E".into(), 0)].into(), started: vec!["E".into()] };
        assert_eq!(top_tier(&rows, &lk), ["a|spira/a|spira|0|0|1", "b|spira/b|spira|0|0|1"]);
    }

    #[test]
    fn epic_started_rule() {
        let mut c = row("c", 1, None, "t");
        assert!(!epic_started(&[c.clone()], "spira-submitted"));
        c.labels.push("spira-submitted".into());
        assert!(epic_started(&[c.clone()], "spira-submitted"));
        c.labels.clear();
        c.status = Some("in_progress".into());
        assert!(epic_started(&[c], "spira-submitted"));
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
        let r = rank(&rows, &EpicLookup::default(), &BTreeSet::new());
        assert_eq!(r.len(), 3000);
        assert_eq!(r[0].bead_priority, 0);
        assert_eq!(parents(&rows).len(), 7);
    }

    // ---- machine mode ----

    fn lcrow(id: &str, st: BeadState, holds: &[HoldKind], depth: u32) -> (String, LifecycleRow) {
        (id.into(), LifecycleRow { bead_id: id.into(), state: st, holds: holds.iter().copied().collect(), stack_depth: depth })
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
    fn stacked_blocker_certified_is_claimable() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[HoldKind::Wait], 0), lcrow("A", BeadState::Certified, &[], 0)].into();
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 4), Verdict::Claimable { depth: 1 });
    }

    #[test]
    fn stacked_blocker_only_submitted_waits() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0), lcrow("A", BeadState::Submitted, &[], 0)].into();
        let bd: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &bd, 4), Verdict::Blocked("A".into()));
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

    #[test]
    fn blocker_with_no_lifecycle_row_uses_bd_status() {
        let lc: HashMap<_, _> = [lcrow("B", BeadState::Ready, &[], 0)].into();
        let open: HashMap<_, _> = [bdrec("A", "open", "task", "spira")].into();
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &open, 4), Verdict::Blocked("A".into()));
        // unknown to bd too: blocked (fail closed)
        assert_eq!(claimable(&blocked_on("B", "A"), &lc, &HashMap::new(), 4), Verdict::Blocked("A".into()));
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
