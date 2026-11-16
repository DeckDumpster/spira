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

/// The one class whose limit is `SPIRA_MAX_DWARVES`.
pub const BUILD_DISK: &str = "build-disk";

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
    pub single_flight: bool,
    pub elastic: bool,
    pub live: i64,
    pub ready: Option<i64>,
    /// The scarce resource classes this rune uses heavily (`RUNE_USES`).
    pub uses: Vec<String>,
    /// May start over a class limit (`RUNE_REACTIVE=1`, an explicit designation).
    pub reactive: bool,
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
    /// The limit of each bound resource class; a class absent here is unbound.
    pub limits: BTreeMap<String, i64>,
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
    /// The task slots this pass found open before its own calls; `None` when no ceiling is configured.
    pub task_room: Option<i64>,
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
        let room = if r.elastic { self.tightest_room(r) } else { None };
        let bound = [pool, room].into_iter().flatten().min();
        match bound {
            Some(b) if r.elastic => b,
            _ => {
                let own = if r.single_flight { (1 - self.live_of(f)).max(0) } else { self.ready_of(f).unwrap_or(0).max(0) };
                pool.map_or(own, |p| own.min(p))
            }
        }
    }

    /// Live sessions of every rune that declares class `c`, planned calls included.
    fn held(&self, c: &str) -> i64 {
        self.w.task.iter().chain(&self.w.lanes).filter(|r| r.uses.iter().any(|u| u == c)).map(|r| self.live_of(&r.name)).sum()
    }

    /// What is left under class `c`'s limit; `None` when the class is unbound.
    fn class_room(&self, c: &str) -> Option<i64> {
        self.w.limits.get(c).map(|l| (l - self.held(c)).max(0))
    }

    /// The room left in the tightest bound class `r` declares; a reactive rune is never held by one.
    fn tightest_room(&self, r: &Rune) -> Option<i64> {
        if r.reactive {
            return None;
        }
        r.uses.iter().filter_map(|c| self.class_room(c)).min()
    }

    fn log(&mut self, line: String) {
        self.plan.log.push(line);
    }

    /// `task_can_take_slot`: the first task rune that could take a lone remaining slot.
    fn task_can_take_slot(&self) -> Option<(String, i64)> {
        for t in &self.w.task {
            let Some(n) = self.ready_of(&t.name) else { continue };
            if n <= 0 || self.free(&t.name, None) <= 0 {
                continue;
            }
            if self.tightest_room(t) == Some(0) {
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
        for c in &rune.uses {
            let Some(limit) = self.w.limits.get(c).copied() else { continue };
            let held = self.held(c);
            if held < limit {
                continue;
            }
            if !rune.reactive {
                self.log(format!("CHECK7 {f}: {c} {held} of {limit} held — no backfill"));
                return false;
            }
            self.log(format!("CHECK7 {f}: {c} {held} of {limit} held — reactive, starting over the limit"));
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
        p.plan.task_room = Some(0);
        return p.plan;
    }
    let lane_names: Vec<String> = w.lanes.iter().map(|l| l.name.clone()).collect();
    let mut pool: Option<i64> = None;
    p.plan.task_room = if w.throttled {
        Some(0)
    } else {
        [p.class_room(BUILD_DISK), w.max_live_dwarves.map(|m| m - p.live_total())].into_iter().flatten().min().map(|n| n.max(0))
    };
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
        Rune { name: name.into(), ready: Some(ready), ..Rune::default() }
    }

    fn world() -> CallWorld {
        CallWorld { task: vec![rune("builder", 2), rune("ops", 0)], max_live_dwarves: Some(4), urgent: Some(vec![]), ..CallWorld::default() }
    }

    fn called(p: &CallPlan) -> Vec<&str> {
        p.calls.iter().map(|c| c.rune.as_str()).collect()
    }

    #[test]
    fn an_open_fleet_calls_one_per_ready_bead() {
        assert_eq!(called(&plan(&world())), ["builder", "builder"]);
    }

    #[test]
    fn a_single_flight_persona_runs_one_at_a_time() {
        let sf = |live, ready| CallWorld { task: vec![Rune { single_flight: true, live, ..rune("warden", ready) }], ..world() };
        assert!(called(&plan(&sf(1, 2))).is_empty(), "one live: no second");
        assert_eq!(called(&plan(&sf(0, 2))), ["warden"], "none live, two ready: one");
    }

    #[test]
    fn a_persona_without_single_flight_is_called_once_per_ready_bead() {
        let w = CallWorld { task: vec![Rune { live: 2, ..rune("skuld", 3) }], max_live_dwarves: Some(9), ..world() };
        assert_eq!(called(&plan(&w)), ["skuld", "skuld", "skuld"]);
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
        w.lanes = vec![Rune { lane: true, ..rune("triage", 1) }];
        w.lanes_max_live = Some(1);
        assert_eq!(called(&plan(&w)), ["triage"], "a lane may spill over the ceiling inside its own cap");
    }

    #[test]
    fn throttled_with_an_express_bead_grants_exactly_one_restricted_slot() {
        let w = CallWorld { throttled: true, express_ready: true, max_live_dwarves: Some(5), task: vec![rune("builder", 5)], ..world() };
        let p = plan(&w);
        assert_eq!(p.calls, [Call { rune: "builder".into(), require_express: true }]);
        assert!(plan(&CallWorld { express_ready: false, ..w }).calls.is_empty(), "throttled with nothing express: the pool is 0");
    }

    #[test]
    fn urgent_work_yields_the_lanes_and_a_full_fleet_preempts_the_youngest_lane_dwarf() {
        let mut w = world();
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

    fn room(w: &CallWorld) -> Option<i64> {
        plan(w).task_room
    }

    #[test]
    fn task_room_is_zero_when_the_task_pool_is_full() {
        let mut w = world();
        w.limits.insert("build-disk".into(), 7);
        w.task[0].uses = vec!["build-disk".into()];
        w.task[0].live = 7;
        w.max_live_dwarves = None;
        assert_eq!(room(&w), Some(0));
    }

    #[test]
    fn task_room_is_what_the_fleet_ceiling_leaves() {
        let mut w = world();
        w.task[0].live = 5;
        w.max_live_dwarves = Some(9);
        assert_eq!(room(&w), Some(4), "nine under the ceiling, five live");
        w.task[0].live = 7;
        assert_eq!(room(&w), Some(2));
        w.task[0].live = 5;
        w.other_live = 2;
        assert_eq!(room(&w), Some(2));
    }

    #[test]
    fn task_room_is_zero_when_the_fleet_is_full_though_the_pool_has_room() {
        let mut w = world();
        w.limits.insert("build-disk".into(), 7);
        w.task[0].uses = vec!["build-disk".into()];
        w.task[0].live = 3;
        w.other_live = 6;
        w.max_live_dwarves = Some(9);
        assert_eq!(room(&w), Some(0));
    }

    #[test]
    fn task_room_is_zero_when_throttled_or_halted() {
        assert_eq!(room(&CallWorld { throttled: true, ..world() }), Some(0));
        for closed in [CallWorld { world: World::Halted, ..world() }, CallWorld { world: World::Draining, ..world() }, CallWorld { skew: Some("old".into()), ..world() }, CallWorld { capacity: Capacity::Paused, ..world() }] {
            assert_eq!(room(&closed), Some(0), "{closed:?}");
        }
    }

    #[test]
    fn task_room_is_unknown_when_no_ceiling_is_configured() {
        assert_eq!(room(&CallWorld { max_live_dwarves: None, ..world() }), None);
    }

    fn using(name: &str, ready: i64, live: i64, classes: &[&str], reactive: bool) -> Rune {
        Rune { live, uses: classes.iter().map(|c| c.to_string()).collect(), reactive, ..rune(name, ready) }
    }

    fn builder(ready: i64, live: i64) -> Rune {
        Rune { elastic: true, ..using("builder", ready, live, &["build-disk"], false) }
    }

    fn mender(ready: i64, live: i64) -> Rune {
        using("mender", ready, live, &["build-disk"], true)
    }

    fn limited(task: Vec<Rune>, limits: &[(&str, i64)]) -> CallWorld {
        CallWorld { task, limits: limits.iter().map(|(c, l)| (c.to_string(), *l)).collect(), max_live_dwarves: None, urgent: Some(vec![]), ..CallWorld::default() }
    }

    #[test]
    fn a_session_with_no_bound_class_is_admitted_with_every_limit_full() {
        let w = limited(vec![builder(0, 7), using("hasher", 0, 1, &["cpu"], false), rune("spike", 1)], &[("build-disk", 7), ("cpu", 1)]);
        let p = plan(&w);
        assert_eq!(called(&p), ["spike"], "{:?}", p.log);
    }

    #[test]
    fn a_planned_build_disk_session_waits_at_the_limit() {
        let w = limited(vec![builder(3, 5), mender(0, 2)], &[("build-disk", 7)]);
        let p = plan(&w);
        assert!(p.calls.is_empty(), "{:?}", p.log);
        assert!(p.log.iter().any(|l| l.contains("build-disk 7 of 7 held — no backfill")), "{:?}", p.log);
    }

    #[test]
    fn a_reactive_session_spills_and_no_planned_session_starts_until_back_under() {
        let pass = |builders: i64, menders: i64| {
            let waiting = if menders == 0 { 2 } else { 0 };
            let w = limited(vec![builder(4, builders), mender(waiting, menders)], &[("build-disk", 7)]);
            let p = plan(&w);
            (called(&p).iter().map(|s| s.to_string()).collect::<Vec<_>>(), p.log)
        };
        assert_eq!(pass(7, 0).0, ["mender", "mender"], "a full class still starts reactive sessions: {:?}", pass(7, 0).1);
        assert!(pass(6, 2).0.is_empty(), "{:?}", pass(6, 2).1);
        assert!(pass(5, 2).0.is_empty(), "over the limit, no planned session: {:?}", pass(5, 2).1);
        assert_eq!(pass(4, 2).0, ["builder"], "back under: one builder fills the last unit: {:?}", pass(4, 2).1);
    }

    #[test]
    fn a_limit_on_a_class_makes_only_its_declarers_wait() {
        let tasks = || vec![using("a", 1, 1, &["memory"], false), using("b", 1, 0, &[], false)];
        assert_eq!(called(&plan(&limited(tasks(), &[("memory", 1)]))), ["b"]);
        assert_eq!(called(&plan(&limited(tasks(), &[]))), ["a", "b"]);
    }

    #[test]
    fn a_reactive_session_over_the_limit_logs_it_and_a_live_one_leaves_no_room_for_a_builder() {
        let p = plan(&limited(vec![builder(3, 7), mender(1, 0)], &[("build-disk", 7)]));
        assert!(p.log.iter().any(|l| l.contains("build-disk 7 of 7 held — reactive, starting over the limit")), "{:?}", p.log);
        let p = plan(&limited(vec![builder(3, 5), mender(0, 2)], &[("build-disk", 7)]));
        assert_eq!(p.task_room, Some(0), "a live mender fills the class: {:?}", p.log);
    }

    #[test]
    fn a_builder_takes_the_room_of_its_tightest_declared_class() {
        let mut b = builder(5, 0);
        b.uses.push("memory".into());
        let p = plan(&limited(vec![b], &[("build-disk", 4), ("memory", 2)]));
        assert_eq!(called(&p), ["builder", "builder"], "{:?}", p.log);
    }
}
