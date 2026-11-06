//! CHECK 7's call planner, pure: the lane-then-pool loop of `_ck7_call_body` and the refusal
//! ladder of `call_rune` (`call.rs`) over one [`CallWorld`] read up front, so the scheduler's
//! `sentinel-call` task decides with no IO. The binary's `Sentinel::ck7_call_body` asks the
//! same questions of the live host one at a time; this asks them of the snapshot, counting
//! each call it plans as live so a later rune in the same pass sees the slot taken.

use std::collections::BTreeMap;

pub fn parse_cache(text: &str) -> Vec<(String, usize)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.next()?.parse().ok()?))
        })
        .collect()
}

pub fn render_cache(counts: &[(String, usize)]) -> String {
    counts.iter().fold(String::new(), |mut s, (f, n)| {
        use std::fmt::Write;
        let _ = writeln!(s, "{f} {n}");
        s
    })
}

/// check7_pool_decision <throttled> <free> <express-ready> -> the task pool CHECK 7 grants
/// this pass (lib.sh:474). An express grant sets the pool to EXACTLY 1, never whatever
/// `free` already held (sp-zcvh1's own fix).
pub fn check7_pool_decision(throttled: bool, free: i64, express_ready: bool) -> i64 {
    if throttled {
        i64::from(express_ready)
    } else {
        free
    }
}

/// lane_rotate <last> <lanes> -> this pass's lane evaluation order (lib.sh:489): `last`
/// (and everything before it) rotates to the end, so the lane that won last pass draws
/// last this pass and the collective cap cannot be monopolised by whichever lane happens
/// to sort first. `last` absent from `lanes` (or empty) leaves the order unchanged.
pub fn lane_rotate(last: &str, lanes: &[String]) -> Vec<String> {
    if last.is_empty() || lanes.is_empty() {
        return lanes.to_vec();
    }
    match lanes.iter().position(|l| l == last) {
        Some(i) => lanes[i + 1..].iter().chain(lanes[..=i].iter()).cloned().collect(),
        None => lanes.to_vec(),
    }
}

/// ck7_pool <max-dwarves> <task-live> -> the task pool's starting size, floored at 0
/// (lib.sh:517). `None` when no pool is configured — every persona's own concurrency cap
/// applies unchanged, exactly as before this pool existed.
pub fn ck7_pool(max_dwarves: Option<i64>, task_live: i64) -> Option<i64> {
    max_dwarves.map(|m| (m - task_live).max(0))
}

/// ck7_throttled <stamp-exists> <override> -> true when the admission throttle stamp holds
/// the task pool at 0 this pass (lib.sh:526). `SPIRA_QUEUE_THROTTLE_OVERRIDE=off` pins the
/// pass unthrottled regardless of the stamp.
pub fn ck7_throttled(stamp_exists: bool, override_: &str) -> bool {
    stamp_exists && override_ != "off"
}

/// ck7_fill_cap <fill> <pool> <max-live-dwarves> -> true ("stop") once the pool (if bounded)
/// is spent or `fill` has reached the per-persona-per-pass cap (lib.sh:536, default 4 —
/// the caller resolves `SPIRA_MAX_LIVE_DWARVES` with that default before calling this).
pub fn ck7_fill_cap(fill: i64, pool: Option<i64>, max_live_dwarves: i64) -> bool {
    if let Some(p) = pool {
        if p <= 0 {
            return true;
        }
    }
    fill >= max_live_dwarves
}

/// The world gate (`world_gate`): halted and draining both refuse every call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum World {
    #[default]
    Running,
    Halted,
    Draining,
}

/// The account's capacity pause (`dwarf::capacity::pause_state`): unknown refuses, as paused does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Capacity {
    #[default]
    Open,
    Paused,
    Unknown,
}

/// One rune of the roster, as `call_rune` reads it: its `RUNE_*` settings, its live dwarves and
/// the machine's ready count (`None`: the count could not be read, never zero, sp-hh599).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rune {
    pub name: String,
    pub lane: bool,
    pub max_concurrent: i64,
    pub elastic: bool,
    pub live: i64,
    pub ready: Option<i64>,
}

/// One row of `spira-claim urgent`: a P0 or REWORK bead. Only `p0` or `express` hold the lanes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Urgent {
    pub id: String,
    pub p0: bool,
    pub express: bool,
}

/// A live lane dwarf and the bead it works, with when it started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaneDwarf {
    pub lane: String,
    pub bead: String,
    pub started_ms: u64,
}

/// Everything CHECK 7 reads, read once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallWorld {
    pub world: World,
    /// The release skew refusal (`skew_gate`), when this process is not the current release.
    pub skew: Option<String>,
    pub capacity: Capacity,
    /// The task runes, then the lane runes, each in roster order (`task_and_lane_runes`).
    pub task: Vec<Rune>,
    pub lanes: Vec<Rune>,
    /// Live dwarves of no rune on the roster: they count toward the fleet ceiling only.
    pub other_live: i64,
    pub max_dwarves: Option<i64>,
    pub max_live_dwarves: Option<i64>,
    pub lanes_max_live: Option<i64>,
    pub throttled: bool,
    pub express_ready: bool,
    /// `None` when the machine could not answer: the lanes yield, never "nothing urgent".
    pub urgent: Option<Vec<Urgent>>,
    pub lane_last: String,
    pub lane_dwarves: Vec<LaneDwarf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub rune: String,
    pub require_express: bool,
}

/// Stop the youngest lane dwarf so a P0 express bead's builder can take its slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preempt {
    pub lane: String,
    pub bead: String,
    pub express: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallPlan {
    pub calls: Vec<Call>,
    pub preempt: Option<Preempt>,
    /// The lane that won this pass, for the next pass's rotation.
    pub lane_last: Option<String>,
    /// Why nobody was called, when a gate closed for the whole pass.
    pub refusal: Option<String>,
    /// The pass log, one line per refusal or call, as the binary printed them.
    pub log: Vec<String>,
}

struct Planner<'a> {
    w: &'a CallWorld,
    live: BTreeMap<String, i64>,
    ready: BTreeMap<String, Option<i64>>,
    plan: CallPlan,
    only_slot_held: Vec<String>,
}

impl Planner<'_> {
    fn rune(&self, f: &str) -> Option<&Rune> {
        self.w.task.iter().chain(&self.w.lanes).find(|r| r.name == f)
    }

    fn live_of(&self, f: &str) -> i64 {
        self.live.get(f).copied().unwrap_or(0)
    }

    fn live_total(&self) -> i64 {
        self.live.values().sum::<i64>() + self.w.other_live
    }

    fn lanes_live(&self) -> i64 {
        self.w.lanes.iter().map(|l| self.live_of(&l.name)).sum()
    }

    fn ready_of(&self, f: &str) -> Option<i64> {
        self.ready.get(f).copied().flatten()
    }

    /// `rune_free`: how many more of this persona may be called now.
    fn free(&self, f: &str, pool: Option<i64>) -> i64 {
        let Some(r) = self.rune(f) else { return 0 };
        let remainder = r.elastic && pool.is_some();
        let max = if remainder { pool.unwrap_or(0) } else { r.max_concurrent };
        let mut free = if remainder { max } else { (max - self.live_of(f)).max(0) };
        if let Some(p) = pool {
            free = free.min(p);
        }
        free
    }

    fn log(&mut self, line: String) {
        self.plan.log.push(line);
    }

    /// `task_can_take_slot`: the first task rune that could take a lone remaining slot.
    fn task_can_take_slot(&self) -> Option<(String, i64)> {
        let task_live: i64 = if self.w.max_dwarves.is_some() { self.w.task.iter().map(|t| self.live_of(&t.name)).sum() } else { 0 };
        for t in &self.w.task {
            let Some(n) = self.ready_of(&t.name) else { continue };
            if n <= 0 || self.free(&t.name, None) <= 0 {
                continue;
            }
            if self.w.max_dwarves.is_some() && ck7_pool(self.w.max_dwarves, task_live) == Some(0) {
                continue;
            }
            if self.w.throttled && !self.w.express_ready {
                continue;
            }
            return Some((t.name.clone(), n));
        }
        None
    }

    /// `call_rune` past the pass-wide gates: the fleet ceiling, the elastic and only-slot
    /// holds, readiness and the persona's own cap. A planned call counts as live at once.
    fn call(&mut self, f: &str, pool: Option<i64>, require_express: bool) -> bool {
        let Some(rune) = self.rune(f).cloned() else { return false };
        if let Some(max_live) = self.w.max_live_dwarves {
            let live_all = self.live_total();
            if live_all >= max_live {
                let spill = rune.lane && self.w.lanes_max_live.is_some_and(|m| live_all < max_live + m && self.lanes_live() < m);
                if !spill {
                    self.log(format!("CHECK7 {f}: {live_all}/{max_live} dwarf(s) live across the whole fleet — not calling"));
                    return false;
                }
                self.log(format!("CHECK7 {f}: {live_all}/{max_live} dwarf(s) live — a lane may spill over the ceiling"));
            }
            if rune.elastic && max_live - live_all == 1 {
                let held = self.w.task.iter().filter(|t| !t.elastic).find_map(|t| self.ready_of(&t.name).filter(|n| *n > 0).map(|n| (t.name.clone(), n)));
                if let Some((nef, n)) = held {
                    self.log(format!("CHECK7 {f}: 1 fleet slot remaining, held back — {nef} has {n} ready bead(s)"));
                    return false;
                }
            }
            if rune.lane {
                if let Some(lanes_max) = self.w.lanes_max_live {
                    let live_lanes = self.lanes_live();
                    if live_lanes >= lanes_max {
                        self.log(format!("CHECK7 {f}: {live_lanes}/{lanes_max} lane slot(s) in use — not calling"));
                        return false;
                    }
                }
                if max_live - live_all == 1 {
                    if let Some((t, n)) = self.task_can_take_slot() {
                        self.log(format!("CHECK7 {f}: 1 fleet slot remaining, held for task work — {t} has {n} ready; the lane spills once the fleet is full"));
                        self.only_slot_held.push(f.to_string());
                        return false;
                    }
                }
            }
        }
        let ready = match self.ready.get(f).copied().flatten() {
            None => {
                self.log(format!("CHECK7 {f}: CLAIM-ERROR: the ready count could not be read — not counted as zero ready, not calling this pass"));
                return false;
            }
            Some(n) => n,
        };
        if ready == 0 {
            return false;
        }
        let free = self.free(f, pool);
        if free == 0 {
            self.log(format!("CHECK7 {f}: {ready} ready, at concurrency cap"));
            return false;
        }
        let restricted = if require_express { ", restricted to express" } else { "" };
        self.log(format!("CHECK7 {f}: {ready} ready, {free} free — calling{restricted}"));
        *self.live.entry(f.to_string()).or_default() += 1;
        self.ready.insert(f.to_string(), Some((ready - 1).max(0)));
        self.plan.calls.push(Call { rune: f.to_string(), require_express });
        true
    }

    /// `preempt_lane_for`: the fleet is full and a P0 express bead waits.
    fn preempt_for(&mut self, express: &str) {
        let Some(max_live) = self.w.max_live_dwarves else { return };
        let live = self.live_total();
        if live < max_live {
            return;
        }
        let ready_builder = self.w.task.iter().any(|t| self.ready_of(&t.name).is_some_and(|n| n > 0) && self.free(&t.name, None) > 0);
        if !ready_builder {
            self.log(format!("CHECK7 lanes: express {express} waits but no task persona can take a slot — no lane preempted"));
            return;
        }
        let youngest = self.w.lane_dwarves.iter().filter(|d| !d.bead.starts_with("sweep-")).max_by_key(|d| d.started_ms).cloned();
        let Some(d) = youngest else {
            self.log(format!("CHECK7 lanes: express {express} waits, {live}/{max_live} live, no lane dwarf to preempt"));
            return;
        };
        if let Some(n) = self.live.get_mut(&d.lane) {
            *n = (*n - 1).max(0);
        }
        self.plan.preempt = Some(Preempt { lane: d.lane, bead: d.bead, express: express.to_string() });
    }
}

/// The calls one CHECK 7 pass makes over `w`, and at most one preemption.
pub fn plan(w: &CallWorld) -> CallPlan {
    let mut p = Planner { w, live: BTreeMap::new(), ready: BTreeMap::new(), plan: CallPlan::default(), only_slot_held: vec![] };
    for r in w.task.iter().chain(&w.lanes) {
        p.live.insert(r.name.clone(), r.live);
        p.ready.insert(r.name.clone(), r.ready);
    }
    let gate = match w.world {
        World::Halted => Some("halted — not calling (world.sh start to lift)".to_string()),
        World::Draining => Some("draining — not calling (world.sh resume to lift)".to_string()),
        World::Running => w.skew.as_ref().map(|s| format!("RELEASE SKEW — {s}; not spawning successors")),
    }
    .or(match w.capacity {
        Capacity::Open => None,
        Capacity::Paused => Some("the account is out of capacity — not calling".to_string()),
        Capacity::Unknown => Some("the capacity pause file could not be read — not calling (failing closed)".to_string()),
    });
    if let Some(why) = gate {
        p.log(format!("CHECK7: {why}"));
        p.plan.refusal = Some(why);
        return p.plan;
    }
    let lane_names: Vec<String> = w.lanes.iter().map(|l| l.name.clone()).collect();
    let mut pool = w.max_dwarves.map(|m| (m - w.task.iter().map(|t| t.live).sum::<i64>()).max(0));
    let mut rotated = lane_rotate(w.lane_last.trim(), &lane_names);
    if !lane_names.is_empty() {
        match &w.urgent {
            Some(u) if u.iter().any(|b| b.p0 || b.express) => {
                if let Some(x) = u.iter().find(|b| b.express && b.p0) {
                    p.preempt_for(&x.id);
                }
                rotated.clear();
            }
            Some(_) => {}
            None => rotated.clear(),
        }
    }
    for f in &rotated {
        if p.call(f, None, false) {
            p.plan.lane_last = Some(f.clone());
        }
    }
    let mut require_express = false;
    if w.throttled {
        pool = Some(check7_pool_decision(true, pool.unwrap_or(0), w.express_ready));
        require_express = w.express_ready;
    }
    for t in &w.task {
        let mut fill = 0;
        while p.call(&t.name, pool, require_express) {
            pool = pool.map(|n| (n - 1).max(0));
            fill += 1;
            if ck7_fill_cap(fill, pool, w.max_live_dwarves.unwrap_or(4)) {
                break;
            }
        }
    }
    for f in std::mem::take(&mut p.only_slot_held) {
        if p.call(&f, None, false) {
            p.plan.lane_last = Some(f);
        }
    }
    p.plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rune(name: &str, ready: i64) -> Rune {
        Rune { name: name.into(), max_concurrent: 1, ready: Some(ready), ..Rune::default() }
    }

    fn world() -> CallWorld {
        CallWorld { task: vec![rune("builder", 2), rune("ops", 0)], max_live_dwarves: Some(4), urgent: Some(vec![]), ..CallWorld::default() }
    }

    fn called(p: &CallPlan) -> Vec<&str> {
        p.calls.iter().map(|c| c.rune.as_str()).collect()
    }

    #[test]
    fn an_open_fleet_calls_the_ready_rune_up_to_its_own_cap() {
        assert_eq!(called(&plan(&world())), ["builder"]);
        let mut w = world();
        w.task[0].max_concurrent = 3;
        assert_eq!(called(&plan(&w)), ["builder", "builder"], "two ready, three allowed: two calls");
    }

    #[test]
    fn a_closed_gate_calls_nobody_and_says_why() {
        for w in [CallWorld { world: World::Halted, ..world() }, CallWorld { skew: Some("old".into()), ..world() }, CallWorld { capacity: Capacity::Unknown, ..world() }] {
            let p = plan(&w);
            assert!(p.calls.is_empty() && p.refusal.is_some(), "{w:?}");
        }
    }

    #[test]
    fn an_unreadable_ready_count_is_never_zero_and_calls_nobody() {
        let mut w = world();
        w.task[0].ready = None;
        let p = plan(&w);
        assert!(p.calls.is_empty());
        assert!(p.log.iter().any(|l| l.contains("CLAIM-ERROR")), "{:?}", p.log);
    }

    #[test]
    fn the_fleet_ceiling_holds_and_a_lane_spills_within_its_own_cap() {
        let mut w = world();
        w.task[0].live = 4;
        assert!(plan(&w).calls.is_empty(), "4/4 live: nobody");
        w.task[0].max_concurrent = 5;
        w.lanes = vec![Rune { lane: true, ..rune("triage", 1) }];
        w.lanes_max_live = Some(1);
        assert_eq!(called(&plan(&w)), ["triage"], "a lane may spill over the ceiling inside its own cap");
    }

    #[test]
    fn throttled_with_an_express_bead_grants_exactly_one_restricted_slot() {
        let w = CallWorld { throttled: true, express_ready: true, max_live_dwarves: Some(5), task: vec![Rune { max_concurrent: 5, ..rune("builder", 5) }], ..world() };
        let p = plan(&w);
        assert_eq!(p.calls, [Call { rune: "builder".into(), require_express: true }]);
        assert!(plan(&CallWorld { express_ready: false, ..w }).calls.is_empty(), "throttled with nothing express: the pool is 0");
    }

    #[test]
    fn urgent_work_yields_the_lanes_and_a_full_fleet_preempts_the_youngest_lane_dwarf() {
        let mut w = world();
        w.task[0].max_concurrent = 2;
        w.task[0].live = 1;
        w.lanes = vec![Rune { lane: true, live: 3, ..rune("triage", 1) }];
        w.lane_dwarves = vec![LaneDwarf { lane: "triage".into(), bead: "sp-old".into(), started_ms: 1 }, LaneDwarf { lane: "triage".into(), bead: "sp-new".into(), started_ms: 9 }];
        w.urgent = Some(vec![Urgent { id: "sp-x".into(), p0: true, express: true }]);
        let p = plan(&w);
        assert_eq!(p.preempt, Some(Preempt { lane: "triage".into(), bead: "sp-new".into(), express: "sp-x".into() }));
        assert_eq!(called(&p), ["builder"], "the freed slot goes to the task pool, never back to a lane");
        w.urgent = None;
        let p = plan(&w);
        assert!(p.preempt.is_none() && !called(&p).contains(&"triage"), "an unanswered urgent read yields the lanes");
    }

    #[test]
    fn the_lane_that_won_last_draws_last() {
        let mut w = world();
        w.task.clear();
        w.lanes = vec![Rune { lane: true, ..rune("a", 1) }, Rune { lane: true, ..rune("b", 1) }];
        w.lanes_max_live = Some(1);
        w.lane_last = "a".into();
        let p = plan(&w);
        assert_eq!(called(&p), ["b"]);
        assert_eq!(p.lane_last.as_deref(), Some("b"));
    }
}
