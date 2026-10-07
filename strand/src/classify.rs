//! The classifier: one partition's view of the store in, stranded rows out. Pure — no I/O,
//! no clock (the caller passes `now`) — so every rule in DESIGN.md §4 is a unit test.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::model::{Bead, Disposition, Row, Status};

/// The whole store, indexed. Closed beads included: a blocker's status is a lookup, never
/// an inference from its absence (R1).
pub struct Store {
    pub beads: Vec<Bead>,
    by_id: HashMap<String, usize>,
    children: HashMap<String, Vec<usize>>,
}

impl Store {
    pub fn new(beads: Vec<Bead>) -> Store {
        let mut by_id = HashMap::new();
        let mut children: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, b) in beads.iter().enumerate() {
            by_id.insert(b.id.clone(), i);
            if let Some(p) = b.parent.as_deref().filter(|p| !p.is_empty()) {
                children.entry(p.to_string()).or_default().push(i);
            }
        }
        Store { beads, by_id, children }
    }
    pub fn get(&self, id: &str) -> Option<&Bead> {
        self.by_id.get(id).map(|&i| &self.beads[i])
    }
    pub fn children(&self, id: &str) -> Vec<&Bead> {
        self.children
            .get(id)
            .map(|v| v.iter().map(|&i| &self.beads[i]).collect())
            .unwrap_or_default()
    }
}

/// The label vocabulary the rules read. Each is one configured key (DESIGN.md §2.6).
#[derive(Debug, Clone)]
pub struct Vocab {
    pub ask: String,
    pub submitted: String,
    pub queue_wait: String,
    pub open_children: String,
    pub poison: String,
}

impl Default for Vocab {
    fn default() -> Self {
        Vocab {
            // literal-ok: conf.sh's shipped default for SPIRA_ASK_LABEL; production passes the configured one
            ask: "needs-operator".into(),
            submitted: "spira-submitted".into(),
            queue_wait: "spira-queue-waiting".into(),
            open_children: "spira-open-children".into(),
            poison: "spira-poison".into(),
        }
    }
}

/// The admission throttle as `throttle-state` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Throttle {
    Open,
    Shut(String),
    Unreadable(String),
}

/// Everything about the world outside the store that the starved rule reads.
#[derive(Debug, Clone)]
pub struct Facts {
    pub now: i64,
    /// Live aeons of the personas that work this partition.
    pub live: u32,
    /// Live aeons across the fleet, and the configured cap (0 = unconfigured).
    pub total_live: u32,
    pub max_aeons: u32,
    /// Some(detail) while the account is out of capacity.
    pub capacity_paused: Option<String>,
    pub pool_paused: bool,
    /// Some("draining"|"halted") while the operator has stopped the world on purpose.
    pub world: Option<&'static str>,
    pub pass_truncated: bool,
    pub throttle: Throttle,
}

impl Default for Facts {
    fn default() -> Self {
        Facts {
            now: 0,
            live: 0,
            total_live: 0,
            max_aeons: 0,
            capacity_paused: None,
            pool_paused: false,
            world: None,
            pass_truncated: false,
            throttle: Throttle::Open,
        }
    }
}

/// One partition, ready to classify.
pub struct Partition<'a> {
    pub store: &'a Store,
    /// The partition's labels: a bead is in it when it carries all of them.
    pub labels: Vec<String>,
    /// Claimable beads under the partition's own predicate (`bd ready`).
    pub ready: HashSet<String>,
    /// Beads whose lifecycle row is WORKING: a builder holds them.
    pub working: HashSet<String>,
    pub vocab: &'a Vocab,
    pub facts: &'a Facts,
}

impl<'a> Partition<'a> {
    fn member(&self, b: &Bead) -> bool {
        self.labels.iter().all(|l| b.labels.iter().any(|x| x == l))
    }
    /// moving(b): the pipeline is carrying it (DESIGN.md §4).
    fn moving(&self, b: &Bead) -> bool {
        self.working.contains(&b.id)
            || b.has(&self.vocab.submitted)
            || b.has(&self.vocab.queue_wait)
            || self.ready.contains(&b.id)
    }
    fn parked(&self, b: &Bead) -> bool {
        b.has(&self.vocab.ask) || b.status == Status::Deferred
    }
    fn poisoned(&self, b: &Bead) -> bool {
        b.has(&self.vocab.poison)
    }
    fn delegated(&self, b: &Bead) -> bool {
        b.has(&self.vocab.open_children)
    }

    /// progressing(id): something is carrying this bead toward closed, or it is waiting on
    /// something that is — or on the operator, or behind an epic by design. Absent from the
    /// store reads false: unknown is never read as closed.
    fn progressing(&self, id: &str, memo: &mut HashMap<String, bool>, visiting: &mut HashSet<String>) -> bool {
        if let Some(&v) = memo.get(id) {
            return v;
        }
        let Some(b) = self.store.get(id) else {
            return false;
        };
        if b.is_closed() || self.moving(b) || b.is_epic() || self.parked(b) || self.poisoned(b) {
            memo.insert(id.to_string(), true);
            return true;
        }
        if !visiting.insert(id.to_string()) {
            return false; // a ring is never progress
        }
        let targets: Vec<String> = b.blocks_targets().map(str::to_string).collect();
        let mut any = false;
        for t in targets {
            if self.store.get(&t).is_some_and(|x| x.is_closed()) {
                continue;
            }
            if self.progressing(&t, memo, visiting) {
                any = true;
                break;
            }
        }
        visiting.remove(id);
        memo.insert(id.to_string(), any);
        any
    }

    pub fn classify(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let members: Vec<&Bead> =
            self.store.beads.iter().filter(|b| !b.is_closed() && self.member(b)).collect();
        self.deferred(&members, &mut rows);
        self.starved(&mut rows);
        let mut memo = HashMap::new();
        for e in members.iter().filter(|b| b.is_epic()) {
            self.epic(e, &mut memo, &mut rows);
        }
        self.cycles(&members, &mut rows);
        rows
    }

    // -- deferred without an escalation (law-filed-bead-queued-xor-escalated). Exempt when a
    // blocks target anywhere in the store is non-closed (R9).
    fn deferred(&self, members: &[&Bead], rows: &mut Vec<Row>) {
        for b in members {
            if b.status != Status::Deferred || b.has(&self.vocab.ask) {
                continue;
            }
            let live_blocker =
                b.blocks_targets().any(|t| self.store.get(t).is_some_and(|x| !x.is_closed()));
            if live_blocker {
                continue;
            }
            // A future defer_until auto-wakes the bead: a timed hold, not a forgotten one.
            if let Some(until) = b.defer_until.as_deref().filter(|u| {
                crate::timefmt::parse_rfc3339(u).is_some_and(|t| t > self.facts.now)
            }) {
                rows.push(Row::new(
                    "held",
                    &b.id,
                    Disposition::Info,
                    format!("held-until {until}: {}", b.title.as_deref().unwrap_or("")),
                    "none — bd wakes it to open when defer_until passes".into(),
                ));
                continue;
            }
            rows.push(Row::new(
                "deferred-unescalated",
                &b.id,
                Disposition::Escalate,
                format!(
                    "deferred but not labelled {}: {}",
                    self.vocab.ask,
                    b.title.as_deref().unwrap_or("")
                ),
                format!(
                    "bd update {} --status open, or label it {} with the decision",
                    b.id, self.vocab.ask
                ),
            ));
        }
    }

    // -- starved: claimable work and nothing alive serving the partition — unless the
    // harness is correctly refusing to summon, which is named as such (info).
    fn starved(&self, rows: &mut Vec<Row>) {
        let f = self.facts;
        if self.ready.is_empty() || f.live != 0 {
            return;
        }
        let mut sorted: Vec<&String> = self.ready.iter().collect();
        sorted.sort();
        let head = sorted.iter().take(6).map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
        let n = self.ready.len();
        let row = if let Some(state) = f.world {
            Row::new(
                "world-drained",
                "-",
                Disposition::Info,
                format!("world is {state} by the operator — {n} bead(s) ready and deliberately not being summoned: {head}"),
                "none — world.sh start resumes summoning".into(),
            )
        } else if let Some(detail) = &f.capacity_paused {
            Row::new(
                "capacity-paused",
                "-",
                Disposition::Info,
                format!("no aeon summoned: {detail} — {n} bead(s) will be claimed when capacity reopens: {head}"),
                "none — the sentinel will summon when the account is open again".into(),
            )
        } else if f.pool_paused {
            Row::new(
                "pool-paused",
                "-",
                Disposition::Info,
                format!("{n} bead(s) ready but the task pool is set to zero — no aeon can be summoned: {head}"),
                "none — raise the task pool (aeons.sh pool <n>) when ready to resume".into(),
            )
        } else if f.pass_truncated {
            Row::new(
                "pass-truncated",
                "-",
                Disposition::Info,
                format!("{n} bead(s) ready but the last sentinel pass was truncated before evaluating this partition: {head}"),
                "the pass was slow — check bd lock contention; this partition will be evaluated next pass".into(),
            )
        } else if let Throttle::Shut(d) = &f.throttle {
            Row::new(
                "throttled",
                "-",
                Disposition::Info,
                format!("throttled: {d} — {n} bead(s) ready but admission is deliberately withheld: {head}"),
                "none — lifts automatically once certified depth drops below the release threshold".into(),
            )
        } else if let Throttle::Unreadable(d) = &f.throttle {
            Row::new(
                "throttle-unreadable",
                "-",
                Disposition::Escalate,
                format!("{n} bead(s) ready and no live aeon, and the queue throttle state could not be read ({d}) — this detector will not guess whether admission is open or shut"),
                "check permissions on the queue-throttled stamp file ($SPIRA_RUN/queue-throttled)".into(),
            )
        } else if f.max_aeons > 0 && f.total_live >= f.max_aeons {
            Row::new(
                "fleet-saturated",
                "-",
                Disposition::Info,
                format!(
                    "{n} bead(s) ready but all {} aeon slot(s) are held by other partitions ({} live across fleet): {head}",
                    f.max_aeons, f.total_live
                ),
                "none — will be claimed when a slot opens".into(),
            )
        } else {
            let slot = if f.max_aeons > 0 {
                format!(
                    "; 0 of {} aeon slot(s) are serving this partition ({} live across fleet)",
                    f.max_aeons, f.total_live
                )
            } else {
                String::new()
            };
            Row::new(
                "starved",
                "-",
                Disposition::Escalate,
                format!("{n} bead(s) ready and no live aeon{slot}: {head}"),
                "check spira-sentinel.timer and the tail of sentinel.log".into(),
            )
        };
        rows.push(row);
    }

    // -- per-epic analysis (R2–R6, R8).
    fn epic(&self, e: &Bead, memo: &mut HashMap<String, bool>, rows: &mut Vec<Row>) {
        let kids = self.store.children(&e.id);
        if kids.is_empty() {
            rows.push(Row::new(
                "empty",
                &e.id,
                Disposition::Escalate,
                format!("open epic with no children: {}", e.title.as_deref().unwrap_or("")),
                "decompose it, or close it".into(),
            ));
            return;
        }
        let live: Vec<&Bead> = kids.into_iter().filter(|c| !c.is_closed()).collect();
        if live.is_empty() {
            return; // complete; pilgrimage.sh owns that
        }
        if live.iter().any(|c| self.moving(c)) {
            return; // moving (R2)
        }
        // Open children outside this partition are another partition's to watch (R8).
        let mine: Vec<&Bead> = live.into_iter().filter(|c| self.member(c)).collect();
        if mine.is_empty() {
            return;
        }
        let parked: Vec<&Bead> = mine.iter().copied().filter(|c| self.parked(c)).collect();
        let poisoned: Vec<&Bead> = mine.iter().copied().filter(|c| self.poisoned(c)).collect();
        let rest: Vec<&Bead> = mine
            .iter()
            .copied()
            .filter(|c| !self.parked(c) && !self.poisoned(c) && !self.delegated(c))
            .collect();
        if rest.is_empty() {
            // Not a strand: waiting on the operator is a queue he already has, a poisoned bead
            // was escalated by the sentinel's poison check, and a delegated bead's work is its
            // own children.
            if parked.is_empty() && poisoned.is_empty() {
                return;
            }
            let (kind, what, who) = if !parked.is_empty() {
                ("waiting", "awaiting the operator", &parked)
            } else {
                ("poisoned", "poisoned", &poisoned)
            };
            rows.push(Row::new(
                kind,
                &e.id,
                Disposition::Info,
                format!(
                    "{} open child(ren), all {}: {}",
                    mine.len(),
                    what,
                    who.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(" ")
                ),
                "none — already queued".into(),
            ));
            return;
        }

        let mut internal: Vec<(String, String)> = Vec::new();
        let mut foreign: Vec<(String, String)> = Vec::new();
        let mut unknown: Vec<(String, String)> = Vec::new();
        let mut foreign_moving = 0usize;
        let mut sequenced: Vec<(String, String)> = Vec::new();
        for c in &rest {
            for t in c.blocks_targets() {
                let pair = (c.id.clone(), t.to_string());
                match self.store.get(t) {
                    None => unknown.push(pair),
                    Some(b) if b.is_closed() => {} // R1
                    Some(b) if b.is_epic() => sequenced.push(pair), // R4
                    Some(b) if self.member(b) => internal.push(pair),
                    Some(_) => {
                        let mut visiting = HashSet::new();
                        if self.progressing(t, memo, &mut visiting) {
                            foreign_moving += 1;
                        } else {
                            foreign.push(pair); // R3
                        }
                    }
                }
            }
        }
        let ids = rest.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(" ");
        let pairs = |v: &[(String, String)]| {
            v.iter().map(|(a, b)| format!("{a}->{b}")).collect::<Vec<_>>()
        };

        if internal.is_empty() && foreign.is_empty() && unknown.is_empty() && sequenced.is_empty() && foreign_moving == 0 {
            // R6: every blocker closed, nothing claimable — is_blocked is a cached column and
            // goes stale after an import or a pull.
            rows.push(Row::new(
                "stale-blocked",
                &e.id,
                Disposition::Act,
                format!(
                    "{} open child(ren) with every blocker closed, none claimable: {}",
                    rest.len(),
                    ids
                ),
                "bd recompute-blocked".into(),
            ));
            return;
        }
        if !foreign.is_empty() || !unknown.is_empty() {
            let mut all = pairs(&foreign);
            all.extend(pairs(&unknown));
            let mut detail = format!(
                "blocked by bead(s) outside the partition: {}",
                all.iter().take(5).cloned().collect::<Vec<_>>().join(" ")
            );
            if !unknown.is_empty() {
                let missing: BTreeSet<&str> = unknown.iter().map(|(_, t)| t.as_str()).collect();
                detail.push_str(&format!(
                    "; not in the store: {}",
                    missing.into_iter().collect::<Vec<_>>().join(" ")
                ));
            }
            rows.push(Row::new(
                "blocked-external",
                &e.id,
                Disposition::Escalate,
                detail,
                "close or import the blocker, or drop the edge with bd dep remove".into(),
            ));
            return;
        }
        let blockers: BTreeSet<String> = internal.iter().map(|(_, t)| t.clone()).collect();
        if !blockers.is_empty() && foreign_moving == 0 {
            let any_progress = blockers.iter().any(|t| {
                let mut visiting = HashSet::new();
                self.progressing(t, memo, &mut visiting)
            });
            if !any_progress {
                rows.push(Row::new(
                    "stuck",
                    &e.id,
                    Disposition::Escalate,
                    format!(
                        "{} open child(ren) blocked by work that is itself not moving: {}",
                        rest.len(),
                        blockers.iter().take(5).cloned().collect::<Vec<_>>().join(" ")
                    ),
                    "unblock the head of the chain, or re-sequence the plan".into(),
                ));
                return;
            }
        }
        if !sequenced.is_empty() {
            let epics: BTreeSet<&str> = sequenced.iter().map(|(_, t)| t.as_str()).collect();
            rows.push(Row::new(
                "sequenced",
                &e.id,
                Disposition::Info,
                format!(
                    "{} open child(ren) ordered behind epic(s) {}: {}",
                    rest.len(),
                    epics.into_iter().collect::<Vec<_>>().join(" "),
                    pairs(&sequenced).into_iter().take(5).collect::<Vec<_>>().join(" ")
                ),
                "none — intentional sequencing; starts when the epic(s) it is ordered behind complete".into(),
            ));
        }
    }

    // -- cycles: Tarjan over the partition's open sub-graph.
    fn cycles(&self, members: &[&Bead], rows: &mut Vec<Row>) {
        let ids: Vec<&str> = members.iter().map(|b| b.id.as_str()).collect();
        let idx: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
        let graph: Vec<Vec<usize>> = members
            .iter()
            .map(|b| b.blocks_targets().filter_map(|t| idx.get(t).copied()).collect())
            .collect();
        for comp in tarjan(&graph) {
            if comp.len() < 2 {
                continue;
            }
            let mut names: Vec<&str> = comp.iter().map(|&i| ids[i]).collect();
            names.sort();
            rows.push(Row::new(
                "cycle",
                names[0],
                Disposition::Escalate,
                format!("dependency cycle, permanently unclaimable: {}", names.join(" -> ")),
                "bd dep remove one edge of the ring".into(),
            ));
        }
    }
}

/// Iterative Tarjan: strongly connected components, in completion order.
fn tarjan(graph: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let n = graph.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on = vec![false; n];
    let mut stack = Vec::new();
    let mut comps = Vec::new();
    let mut counter = 0usize;
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        index[root] = counter;
        low[root] = counter;
        counter += 1;
        stack.push(root);
        on[root] = true;
        while let Some(&mut (v, ref mut next)) = work.last_mut() {
            if *next < graph[v].len() {
                let w = graph[v][*next];
                *next += 1;
                if index[w] == usize::MAX {
                    index[w] = counter;
                    low[w] = counter;
                    counter += 1;
                    stack.push(w);
                    on[w] = true;
                    work.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                work.pop();
                if let Some(&(u, _)) = work.last() {
                    low[u] = low[u].min(low[v]);
                }
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    comps.push(comp);
                }
            }
        }
    }
    comps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_beads;
    use serde_json::json;

    const PLAN: &[&str] = &["spira", "plan"];
    const ASK: &str = "operator-ask";

    fn bead(id: &str, status: &str, labels: &[&str], parent: Option<&str>, blocks: &[&str]) -> serde_json::Value {
        let mut deps: Vec<serde_json::Value> = blocks
            .iter()
            .map(|t| json!({"issue_id": id, "depends_on_id": t, "type": "blocks"}))
            .collect();
        if let Some(p) = parent {
            deps.push(json!({"issue_id": id, "depends_on_id": p, "type": "parent-child"}));
        }
        json!({"id": id, "title": format!("title of {id}"), "status": status, "issue_type": "task",
               "labels": labels, "parent": parent, "dependencies": deps})
    }
    fn epic(id: &str, labels: &[&str]) -> serde_json::Value {
        json!({"id": id, "title": format!("epic {id}"), "status": "open", "issue_type": "epic",
               "labels": labels, "dependencies": []})
    }
    fn plan_with(extra: &[&str]) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = PLAN.to_vec();
        for e in extra {
            v.push(Box::leak(e.to_string().into_boxed_str()));
        }
        v
    }
    fn store(v: Vec<serde_json::Value>) -> Store {
        Store::new(parse_beads(&serde_json::Value::Array(v).to_string()).unwrap())
    }
    fn run_with(s: &Store, ready: &[&str], facts: &Facts) -> Vec<Row> {
        run_working(s, ready, &[], facts)
    }
    fn run_working(s: &Store, ready: &[&str], working: &[&str], facts: &Facts) -> Vec<Row> {
        let vocab = Vocab { ask: ASK.into(), ..Vocab::default() };
        let p = Partition {
            store: s,
            labels: PLAN.iter().map(|s| s.to_string()).collect(),
            ready: ready.iter().map(|s| s.to_string()).collect(),
            working: working.iter().map(|s| s.to_string()).collect(),
            vocab: &vocab,
            facts,
        };
        p.classify()
    }
    fn run(s: &Store, ready: &[&str]) -> Vec<Row> {
        // A live aeon, so starved never fires unless a test asks for it.
        let f = Facts { live: 1, ..Facts::default() };
        run_with(s, ready, &f)
    }
    fn kinds(rows: &[Row]) -> Vec<(String, String)> {
        rows.iter().map(|r| (r.kind.clone(), r.id.clone())).collect()
    }

    // ---- the bead's own four, plus empty ----

    #[test]
    fn closed_blocker_is_not_reported() {
        // A child blocked only by a closed bead — a bead outside the partition's open list,
        // exactly the case the old detector read as "outside the partition".
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", PLAN, Some("E"), &["x"]),
            bead("x", "closed", &["spira"], None, &[]),
        ]);
        assert!(run(&s, &["c"]).is_empty(), "claimable child, closed blocker: nothing");
        // Not claimable (is_blocked stale): the mechanical fix, never blocked-external.
        assert_eq!(kinds(&run(&s, &[])), vec![("stale-blocked".into(), "E".into())]);
    }

    #[test]
    fn submitted_dependent_is_not_reported() {
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", &plan_with(&["spira-submitted"]), Some("E"), &["x"]),
            bead("x", "open", &["spira", "incident"], None, &[]),
        ]);
        assert!(run(&s, &[]).is_empty());
    }

    #[test]
    fn working_dependent_is_not_reported() {
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", PLAN, Some("E"), &["x"]),
            bead("x", "open", &["spira", "incident"], None, &[]),
        ]);
        assert!(run_working(&s, &[], &["c"], &Facts { live: 1, ..Facts::default() }).is_empty());
        assert!(!run(&s, &[]).is_empty(), "bd status in_progress alone no longer means moving");
    }

    #[test]
    fn open_external_blocker_on_unstarted_dependent_is_reported() {
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", PLAN, Some("E"), &["x"]),
            bead("x", "open", &["spira", "incident"], None, &[]),
        ]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("blocked-external".into(), "E".into())]);
        assert_eq!(rows[0].disp, Disposition::Escalate);
        assert_eq!(rows[0].detail, "blocked by bead(s) outside the partition: c->x");
    }

    #[test]
    fn empty_epic_is_reported_as_empty() {
        let s = store(vec![epic("E", PLAN)]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("empty".into(), "E".into())]);
        assert_eq!(rows[0].detail, "open epic with no children: epic E");
        assert_eq!(rows[0].action, "decompose it, or close it");
    }

    #[test]
    fn all_closed_epic_is_complete_not_empty() {
        let s = store(vec![epic("E", PLAN), bead("c", "closed", PLAN, Some("E"), &[])]);
        assert!(run(&s, &[]).is_empty());
    }

    // ---- tonight's false asks, from the real edges (bd show, 2026-09-29) ----

    #[test]
    fn fixture_sp_vvt04_yyltf_blocked_only_by_closed_beads() {
        // sp-vvt04: "sp-f3af9->sp-lno75 sp-f0qhr->sp-xethq/sp-a0zvh/sp-i2m7y sp-s9675->sp-lno75";
        // every blocker closed, the two dependents submitted.
        let sub = plan_with(&["spira-submitted"]);
        let s = store(vec![
            epic("sp-yyltf", PLAN),
            bead("sp-f3af9", "open", &sub, Some("sp-yyltf"), &["sp-lno75"]),
            bead("sp-f0qhr", "open", &sub, Some("sp-yyltf"), &["sp-xethq", "sp-a0zvh", "sp-i2m7y"]),
            bead("sp-s9675", "open", PLAN, Some("sp-yyltf"), &["sp-lno75", "sp-f3af9", "sp-f0qhr"]),
            bead("sp-falao", "open", PLAN, Some("sp-yyltf"), &["sp-f0qhr"]),
            bead("sp-lno75", "closed", &sub, Some("sp-yyltf"), &["sp-a0zvh"]),
            bead("sp-a0zvh", "closed", &sub, Some("sp-yyltf"), &[]),
            bead("sp-xethq", "closed", PLAN, None, &[]),
            bead("sp-i2m7y", "closed", PLAN, None, &[]),
        ]);
        assert!(run(&s, &[]).is_empty());
    }

    #[test]
    fn fixture_sp_cxlmq_closed_blockers_without_the_epic_level_skip() {
        // sp-cxlmq's edges with the dependents NOT submitted: the blockers are still closed,
        // so the verdict is never blocked-external.
        let s = store(vec![
            epic("sp-yyltf", PLAN),
            bead("sp-f0qhr", "open", PLAN, Some("sp-yyltf"), &["sp-xethq", "sp-i2m7y"]),
            bead("sp-s9675", "open", PLAN, Some("sp-yyltf"), &["sp-1z10t"]),
            bead("sp-xethq", "closed", PLAN, None, &[]),
            bead("sp-i2m7y", "closed", PLAN, None, &[]),
            bead("sp-1z10t", "closed", PLAN, None, &[]),
        ]);
        let rows = run(&s, &[]);
        assert!(rows.iter().all(|r| r.kind != "blocked-external"), "{rows:?}");
    }

    #[test]
    fn fixture_sp_bpe4n_msk4h_closed_blocker_and_worked_dependent() {
        // sp-bpe4n: "sp-o3o6z->sp-7tw9h sp-o4wu7->sp-7tw9h"; sp-7tw9h landed, sp-o3o6z worked.
        let s = store(vec![
            epic("sp-msk4h", PLAN),
            bead("sp-7tw9h", "closed", &plan_with(&["spira-submitted"]), Some("sp-msk4h"), &[]),
            bead("sp-o3o6z", "open", PLAN, Some("sp-msk4h"), &["sp-7tw9h"]),
            bead("sp-o4wu7", "open", PLAN, Some("sp-msk4h"), &["sp-7tw9h", "sp-o3o6z"]),
        ]);
        assert!(run_working(&s, &[], &["sp-o3o6z"], &Facts { live: 1, ..Facts::default() }).is_empty());
    }

    #[test]
    fn fixture_sp_7jail_msk4h_submitted_head_is_not_stuck() {
        // sp-7jail: "blocked by work that is itself not moving: sp-7tw9h sp-o3o6z" while
        // sp-7tw9h was submitted and in round 119.
        let s = store(vec![
            epic("sp-msk4h", PLAN),
            bead("sp-7tw9h", "open", &plan_with(&["spira-submitted"]), Some("sp-msk4h"), &[]),
            bead("sp-o3o6z", "open", PLAN, Some("sp-msk4h"), &["sp-7tw9h"]),
            bead("sp-o4wu7", "open", PLAN, Some("sp-msk4h"), &["sp-7tw9h", "sp-o3o6z"]),
        ]);
        assert!(run(&s, &[]).is_empty());
    }

    #[test]
    fn submitted_blocker_outside_the_epic_is_progress_not_stuck() {
        // The stuck rule without the epic-level skip: the head of the chain is another
        // epic's submitted bead in the same partition.
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", PLAN, Some("E"), &["h"]),
            bead("h", "open", &plan_with(&["spira-submitted"]), None, &[]),
        ]);
        assert!(run(&s, &[]).is_empty());
    }

    #[test]
    fn fixture_sp_srdjc_yyltf_submitted_entry_beads() {
        // sp-srdjc: stuck on sp-1z10t sp-a0zvh sp-f0qhr sp-f3af9 sp-falao while the entry
        // beads sp-a0zvh and sp-1z10t were submitted.
        let sub = plan_with(&["spira-submitted"]);
        let s = store(vec![
            epic("sp-yyltf", PLAN),
            bead("sp-a0zvh", "open", &sub, Some("sp-yyltf"), &[]),
            bead("sp-1z10t", "open", &sub, Some("sp-yyltf"), &[]),
            bead("sp-f0qhr", "open", PLAN, Some("sp-yyltf"), &["sp-a0zvh"]),
            bead("sp-f3af9", "open", PLAN, Some("sp-yyltf"), &["sp-a0zvh"]),
            bead("sp-falao", "open", PLAN, Some("sp-yyltf"), &["sp-f0qhr"]),
            bead("sp-s9675", "open", PLAN, Some("sp-yyltf"), &["sp-1z10t", "sp-falao"]),
        ]);
        assert!(run(&s, &[]).is_empty());
    }

    #[test]
    fn fixture_sp_mf03u_zs04v_ordered_behind_an_epic_is_sequenced() {
        // sp-mf03u: "sp-zs04v.5->sp-pswer sp-zs04v.5->sp-zs04v.4 sp-zs04v.6->sp-zs04v.4
        // sp-zs04v.6->sp-pswer". sp-zs04v.4 closed; sp-pswer an open epic outside the plan
        // partition that the operator ordered sp-zs04v behind.
        let s = store(vec![
            epic("sp-zs04v", PLAN),
            epic("sp-pswer", &["repo:spira", "spira"]),
            bead("sp-zs04v.4", "closed", &plan_with(&["spira-submitted"]), Some("sp-zs04v"), &[]),
            bead("sp-zs04v.5", "open", PLAN, Some("sp-zs04v"), &["sp-pswer", "sp-zs04v.4"]),
            bead("sp-zs04v.6", "open", PLAN, Some("sp-zs04v"), &["sp-zs04v.4", "sp-pswer"]),
        ]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("sequenced".into(), "sp-zs04v".into())]);
        assert_eq!(rows[0].disp, Disposition::Info);
        assert!(rows[0].detail.contains("ordered behind epic(s) sp-pswer"), "{}", rows[0].detail);
    }

    #[test]
    fn a_bead_waiting_on_a_sequenced_bead_is_not_stuck() {
        // sp-zs04v.6 behind sp-zs04v.5 behind the epic sp-pswer: the chain head is
        // intentional sequencing, so the chain is waiting, not stuck.
        let s = store(vec![
            epic("sp-zs04v", PLAN),
            epic("sp-pswer", &["spira"]),
            bead("sp-zs04v.5", "open", PLAN, None, &["sp-pswer"]),
            bead("sp-zs04v.6", "open", PLAN, Some("sp-zs04v"), &["sp-zs04v.5"]),
        ]);
        let rows = run(&s, &[]);
        assert!(rows.iter().all(|r| r.disp == Disposition::Info), "{rows:?}");
    }

    // ---- what must still be reported ----

    #[test]
    fn a_blocker_missing_from_the_store_still_counts() {
        let s = store(vec![epic("E", PLAN), bead("c", "open", PLAN, Some("E"), &["ghostid"])]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("blocked-external".into(), "E".into())]);
        assert!(rows[0].detail.ends_with("; not in the store: ghostid"), "{}", rows[0].detail);
    }

    #[test]
    fn foreign_blocker_that_is_itself_moving_is_not_external() {
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", PLAN, Some("E"), &["x"]),
            bead("x", "open", &["spira", "incident"], None, &[]),
        ]);
        assert!(run_working(&s, &[], &["x"], &Facts { live: 1, ..Facts::default() }).is_empty());
    }

    #[test]
    fn dead_internal_chain_is_stuck() {
        // h is open, unclaimable (say, a dead assignee) and blocked by nothing: not moving.
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", PLAN, Some("E"), &["h"]),
            bead("h", "open", PLAN, None, &[]),
        ]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("stuck".into(), "E".into())]);
        assert_eq!(rows[0].detail, "1 open child(ren) blocked by work that is itself not moving: h");
        // The same chain with a claimable head is not stuck.
        assert!(run(&s, &["h"]).is_empty());
    }

    #[test]
    fn parked_and_poisoned_children_are_info() {
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", &plan_with(&[ASK]), Some("E"), &[]),
        ]);
        assert_eq!(kinds(&run(&s, &[])), vec![("waiting".into(), "E".into())]);
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", &plan_with(&["spira-poison"]), Some("E"), &[]),
        ]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("poisoned".into(), "E".into())]);
        assert_eq!(rows[0].disp, Disposition::Info);
    }

    #[test]
    fn open_children_outside_the_partition_are_not_analysed_here() {
        let s = store(vec![
            epic("E", PLAN),
            bead("c", "open", &["spira", "incident"], Some("E"), &["x"]),
            bead("x", "open", &["spira", "other"], None, &[]),
        ]);
        assert!(run(&s, &[]).is_empty());
    }

    #[test]
    fn deferred_exemption_sees_foreign_blockers() {
        let s = store(vec![
            bead("d", "deferred", PLAN, None, &["x"]),
            bead("x", "open", &["spira", "incident"], None, &[]),
        ]);
        assert!(run(&s, &[]).is_empty());
        let s = store(vec![bead("d", "deferred", PLAN, None, &["x"]), bead("x", "closed", &[], None, &[])]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("deferred-unescalated".into(), "d".into())]);
        assert_eq!(rows[0].detail, "deferred but not labelled operator-ask: title of d");
    }

    #[test]
    fn timed_hold_is_held_not_stranded() {
        let now = crate::timefmt::parse_rfc3339("2026-10-02T09:00:00Z").unwrap();
        let f = Facts { live: 1, now, ..Facts::default() };
        let held = |until: Option<&str>| {
            let mut v = bead("d", "deferred", PLAN, None, &[]);
            if let Some(u) = until {
                v["defer_until"] = json!(u);
            }
            run_with(&store(vec![v]), &[], &f)
        };
        let rows = held(Some("2026-10-02T09:48:33Z"));
        assert_eq!(kinds(&rows), vec![("held".into(), "d".into())]);
        assert_eq!(rows[0].disp, Disposition::Info);
        assert!(rows[0].detail.starts_with("held-until 2026-10-02T09:48:33Z"), "{}", rows[0].detail);
        for stranded in [held(None), held(Some("2026-10-02T08:00:00Z")), held(Some("garbage"))] {
            assert_eq!(kinds(&stranded), vec![("deferred-unescalated".into(), "d".into())]);
        }
    }

    #[test]
    fn starved_and_its_correct_refusals() {
        let s = store(vec![bead("r", "open", PLAN, None, &[])]);
        let mut f = Facts::default();
        let rows = run_with(&s, &["r"], &f);
        assert_eq!(kinds(&rows), vec![("starved".into(), "-".into())]);
        assert_eq!(rows[0].detail, "1 bead(s) ready and no live aeon: r");
        f.world = Some("draining");
        let r = run_with(&s, &["r"], &f);
        assert_eq!((r[0].kind.as_str(), r[0].disp), ("world-drained", Disposition::Info));
        f.world = Some("halted");
        assert_eq!(run_with(&s, &["r"], &f)[0].kind, "world-drained");
        f.world = None;
        f.capacity_paused = Some("out".into());
        assert_eq!(run_with(&s, &["r"], &f)[0].kind, "capacity-paused");
        f.capacity_paused = None;
        f.pool_paused = true;
        assert_eq!(run_with(&s, &["r"], &f)[0].kind, "pool-paused");
        f.pool_paused = false;
        f.pass_truncated = true;
        assert_eq!(run_with(&s, &["r"], &f)[0].kind, "pass-truncated");
        f.pass_truncated = false;
        f.throttle = Throttle::Shut("depth 9 >= release-at 8".into());
        assert_eq!(run_with(&s, &["r"], &f)[0].kind, "throttled");
        f.throttle = Throttle::Unreadable("x".into());
        let r = run_with(&s, &["r"], &f);
        assert_eq!((r[0].kind.as_str(), r[0].disp), ("throttle-unreadable", Disposition::Escalate));
        f.throttle = Throttle::Open;
        f.max_aeons = 2;
        f.total_live = 2;
        assert_eq!(run_with(&s, &["r"], &f)[0].kind, "fleet-saturated");
        f.total_live = 1;
        let r = run_with(&s, &["r"], &f);
        assert_eq!(r[0].detail, "1 bead(s) ready and no live aeon; 0 of 2 aeon slot(s) are serving this partition (1 live across fleet): r");
        f.live = 1;
        assert!(run_with(&s, &["r"], &f).is_empty());
    }

    #[test]
    fn cycle_is_reported_once() {
        let s = store(vec![
            bead("a", "open", PLAN, None, &["b"]),
            bead("b", "open", PLAN, None, &["c"]),
            bead("c", "open", PLAN, None, &["a"]),
            bead("d", "open", PLAN, None, &["a"]),
        ]);
        let rows = run(&s, &[]);
        assert_eq!(kinds(&rows), vec![("cycle".into(), "a".into())]);
        assert_eq!(rows[0].detail, "dependency cycle, permanently unclaimable: a -> b -> c");
    }

    #[test]
    fn partition_membership_is_all_labels() {
        let s = store(vec![epic("E", &["spira"])]); // not in spira,plan
        assert!(run(&s, &[]).is_empty());
    }
}
