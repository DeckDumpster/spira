//! CHECK 7's call planner, pure: the reactive-then-planned loop of `_ck7_call_body` and the refusal
//! ladder of `call_rune` (`call.rs`) over one [`CallWorld`] read up front, so the scheduler's
//! `sentinel-call` task decides with no IO. The binary's `Sentinel::ck7_call_body` asks the
//! same questions of the live host one at a time; this asks them of the snapshot, counting
//! each call it plans as live so a later rune in the same pass sees the slot taken.

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

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
    pub single_flight: bool,
    pub elastic: bool,
    pub live: i64,
    pub ready: Option<i64>,
    /// The scarce resource classes this rune uses heavily (`RUNE_USES`).
    pub uses: Vec<String>,
    /// May start over a class limit (`RUNE_REACTIVE=1`, an explicit designation).
    pub reactive: bool,
}

/// One row of `spira-claim urgent`: a P0 or REWORK bead. Only `p0` or `express` hold the reactive runes that use a bound class.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Urgent {
    pub id: String,
    pub p0: bool,
    pub express: bool,
}

/// A live reactive dwarf and the bead it works, with when it started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReactiveDwarf {
    pub rune: String,
    pub bead: String,
    pub started_ms: u64,
}

/// A live dwarf and the bead its pidfile names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveDwarf {
    pub rune: String,
    pub bead: String,
}

/// Everything CHECK 7 reads, read once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallWorld {
    pub world: World,
    /// The release skew refusal (`skew_gate`), when this process is not the current release.
    pub skew: Option<String>,
    pub capacity: Capacity,
    /// The planned runes, then the reactive runes, each in roster order.
    pub task: Vec<Rune>,
    pub reactive: Vec<Rune>,
    /// Live dwarves of no rune on the roster: they count toward the fleet ceiling only.
    pub other_live: i64,
    /// The limit of each bound resource class; a class absent here is unbound.
    pub limits: BTreeMap<String, i64>,
    pub max_live_dwarves: Option<i64>,
    pub throttled: bool,
    pub express_ready: bool,
    /// `None` when the machine could not answer: the reactive runes yield, never "nothing urgent".
    pub urgent: Option<Vec<Urgent>>,
    pub reactive_dwarves: Vec<ReactiveDwarf>,
    pub live_dwarves: Vec<LiveDwarf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub rune: String,
    pub require_express: bool,
}

/// Stop the youngest reactive dwarf so a P0 express bead's builder can take its slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preempt {
    pub rune: String,
    pub bead: String,
    pub express: String,
}

/// One bound resource class as this pass left it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bound {
    pub class: String,
    pub limit: i64,
    pub held: i64,
    pub waiting: i64,
    pub over: bool,
}

/// The limit that bound task work this pass: `pool`, `ceiling`, `throttle` or `closed`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLimit {
    pub name: String,
    pub limit: i64,
    pub held: i64,
    /// The beads of the dwarves `held` counts; one with no pidfile bead is counted and not named.
    #[serde(default)]
    pub holders: Vec<String>,
}

impl TaskLimit {
    fn room(&self) -> i64 {
        (self.limit - self.held).max(0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallPlan {
    pub calls: Vec<Call>,
    pub preempt: Option<Preempt>,
    /// Why nobody was called, when a gate closed for the whole pass.
    pub refusal: Option<String>,
    /// The task slots this pass found open before its own calls; `None` when no ceiling is configured.
    pub task_room: Option<i64>,
    /// The limit with the least room behind `task_room`; `None` when none is configured.
    pub task_limit: Option<TaskLimit>,
    /// Each class with a limit: its declarers' live count after this pass's calls, and their ready beads left uncalled.
    pub bound: Vec<Bound>,
    /// Each rune that ends the pass with ready work (or an unreadable count) and no call, with the refusal that stopped it.
    pub uncalled: BTreeMap<String, String>,
    /// The pass log, one line per refusal or call, as the binary printed them.
    pub log: Vec<String>,
}

struct Planner<'a> {
    w: &'a CallWorld,
    live: BTreeMap<String, i64>,
    ready: BTreeMap<String, Option<i64>>,
    plan: CallPlan,
}

impl Planner<'_> {
    fn rune(&self, f: &str) -> Option<&Rune> {
        self.w.task.iter().chain(&self.w.reactive).find(|r| r.name == f)
    }

    fn live_of(&self, f: &str) -> i64 {
        self.live.get(f).copied().unwrap_or(0)
    }

    fn live_total(&self) -> i64 {
        self.live.values().sum::<i64>() + self.w.other_live
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
        self.w.task.iter().chain(&self.w.reactive).filter(|r| r.uses.iter().any(|u| u == c)).map(|r| self.live_of(&r.name)).sum()
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

    fn bound(&self) -> Vec<Bound> {
        self.w
            .limits
            .iter()
            .map(|(c, limit)| {
                let waiting = self.w.task.iter().chain(&self.w.reactive).filter(|r| r.uses.iter().any(|u| u == c)).map(|r| self.ready_of(&r.name).unwrap_or(0)).sum();
                let held = self.held(c);
                Bound { class: c.clone(), limit: *limit, held, waiting, over: held > *limit }
            })
            .collect()
    }

    /// A rune names at most as many beads as it has live units: a dead dwarf's lease can outlive its unit.
    fn holders(&self, counts: impl Fn(&Rune) -> bool) -> Vec<String> {
        self.w
            .task
            .iter()
            .chain(&self.w.reactive)
            .filter(|r| counts(r))
            .flat_map(|r| self.w.live_dwarves.iter().filter(|d| d.rune == r.name).take(usize::try_from(self.live_of(&r.name)).unwrap_or(0)))
            .map(|d| d.bead.clone())
            .collect()
    }

    /// A limit every live dwarf counts toward.
    fn fleet_limit(&self, name: &str, limit: i64) -> TaskLimit {
        TaskLimit { name: name.into(), limit, held: self.live_total(), holders: self.holders(|_| true) }
    }

    fn set_task_limit(&mut self, limit: Option<TaskLimit>) {
        self.plan.task_room = limit.as_ref().map(TaskLimit::room);
        self.plan.task_limit = limit;
    }

    fn log(&mut self, line: String) {
        self.plan.log.push(line);
    }

    fn refuse(&mut self, f: &str, reason: String) {
        self.log(format!("CHECK7 {f}: {reason}"));
        if self.ready_of(f) != Some(0) {
            self.plan.uncalled.insert(f.to_string(), reason);
        }
    }

    /// `call_rune` past the pass-wide gates: the fleet ceiling, the elastic
    /// hold, readiness and the persona's own cap. A planned call counts as live at once.
    fn call(&mut self, f: &str, pool: Option<i64>, require_express: bool) -> bool {
        let Some(rune) = self.rune(f).cloned() else { return false };
        if let Some(max_live) = self.w.max_live_dwarves {
            let live_all = self.live_total();
            if live_all >= max_live {
                self.refuse(f, format!("{live_all}/{max_live} dwarf(s) live across the whole fleet — not calling"));
                return false;
            }
            if rune.elastic && max_live - live_all == 1 {
                let held = self.w.task.iter().filter(|t| !t.elastic).find_map(|t| self.ready_of(&t.name).filter(|n| *n > 0).map(|n| (t.name.clone(), n)));
                if let Some((nef, n)) = held {
                    self.refuse(f, format!("1 fleet slot remaining, held back — {nef} has {n} ready bead(s)"));
                    return false;
                }
            }
        }
        let ready = match self.ready.get(f).copied().flatten() {
            None => {
                self.refuse(f, "CLAIM-ERROR: the ready count could not be read — not counted as zero ready, not calling this pass".into());
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
                self.refuse(f, format!("{c} {held} of {limit} held — no backfill"));
                return false;
            }
            self.log(format!("CHECK7 {f}: {c} {held} of {limit} held — reactive, starting over the limit"));
        }
        let free = self.free(f, pool);
        if free == 0 {
            self.refuse(f, format!("{ready} ready, at concurrency cap"));
            return false;
        }
        let restricted = if require_express { ", restricted to express" } else { "" };
        self.log(format!("CHECK7 {f}: {ready} ready, {free} free — calling{restricted}"));
        *self.live.entry(f.to_string()).or_default() += 1;
        self.ready.insert(f.to_string(), Some((ready - 1).max(0)));
        self.plan.calls.push(Call { rune: f.to_string(), require_express });
        true
    }

    /// `preempt_for`: the fleet is full and a P0 express bead waits.
    fn preempt_for(&mut self, express: &str) {
        let Some(max_live) = self.w.max_live_dwarves else { return };
        let live = self.live_total();
        if live < max_live {
            return;
        }
        let ready_builder = self.w.task.iter().any(|t| self.ready_of(&t.name).is_some_and(|n| n > 0) && self.free(&t.name, None) > 0);
        if !ready_builder {
            self.log(format!("CHECK7 reactive: express {express} waits but no task persona can take a slot — no reactive dwarf preempted"));
            return;
        }
        let youngest = self.w.reactive_dwarves.iter().filter(|d| !d.bead.starts_with("sweep-")).max_by_key(|d| d.started_ms).cloned();
        let Some(d) = youngest else {
            self.log(format!("CHECK7 reactive: express {express} waits, {live}/{max_live} live, no reactive dwarf to preempt"));
            return;
        };
        if let Some(n) = self.live.get_mut(&d.rune) {
            *n = (*n - 1).max(0);
        }
        self.plan.preempt = Some(Preempt { rune: d.rune, bead: d.bead, express: express.to_string() });
    }
}

/// The calls one CHECK 7 pass makes over `w`, and at most one preemption.
pub fn plan(w: &CallWorld) -> CallPlan {
    let mut p = Planner { w, live: BTreeMap::new(), ready: BTreeMap::new(), plan: CallPlan::default() };
    for r in w.task.iter().chain(&w.reactive) {
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
        for r in w.task.iter().chain(&w.reactive) {
            if r.ready != Some(0) {
                p.plan.uncalled.insert(r.name.clone(), why.clone());
            }
        }
        p.plan.refusal = Some(why);
        p.set_task_limit(Some(p.fleet_limit("closed", 0)));
        p.plan.bound = p.bound();
        return p.plan;
    }
    let mut pool: Option<i64> = None;
    let limit = if w.throttled {
        Some(p.fleet_limit("throttle", 0))
    } else {
        let pool = w.limits.get(BUILD_DISK).map(|l| TaskLimit { name: "pool".into(), limit: *l, held: p.held(BUILD_DISK), holders: p.holders(|r| r.uses.iter().any(|u| u == BUILD_DISK)) });
        let ceiling = w.max_live_dwarves.map(|m| p.fleet_limit("ceiling", m));
        [pool, ceiling].into_iter().flatten().min_by_key(TaskLimit::room)
    };
    p.set_task_limit(limit);
    let mut hold: Option<String> = None;
    if !w.reactive.is_empty() {
        match &w.urgent {
            Some(u) if u.iter().any(|b| b.p0 || b.express) => {
                if let Some(x) = u.iter().find(|b| b.express && b.p0) {
                    p.preempt_for(&x.id);
                }
                let ids: Vec<&str> = u.iter().filter(|b| b.p0 || b.express).map(|b| b.id.as_str()).collect();
                hold = Some(format!("urgent bead(s) unclaimed: {}", ids.join(" ")));
            }
            Some(_) => {}
            None => hold = Some("the urgent read did not answer".into()),
        }
    }
    for r in &w.reactive {
        match &hold {
            Some(why) if r.uses.iter().any(|c| w.limits.contains_key(c)) => {
                p.refuse(&r.name, format!("yielding — {why}"));
            }
            _ => {
                p.call(&r.name, None, false);
            }
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
    p.plan.bound = p.bound();
    let called: Vec<&str> = p.plan.calls.iter().map(|c| c.rune.as_str()).collect();
    p.plan.uncalled.retain(|r, _| !called.contains(&r.as_str()));
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

    #[test_id(tts8b76)]
    #[test]
    fn an_open_fleet_calls_one_per_ready_bead() {
        assert_eq!(called(&plan(&world())), ["builder", "builder"]);
    }

    #[test_id(tf3s2z5)]
    #[test]
    fn a_single_flight_persona_runs_one_at_a_time() {
        let sf = |live, ready| CallWorld { task: vec![Rune { single_flight: true, live, ..rune("warden", ready) }], ..world() };
        assert!(called(&plan(&sf(1, 2))).is_empty(), "one live: no second");
        assert_eq!(called(&plan(&sf(0, 2))), ["warden"], "none live, two ready: one");
    }

    #[test_id(tk5afg0)]
    #[test]
    fn a_persona_without_single_flight_is_called_once_per_ready_bead() {
        let w = CallWorld { task: vec![Rune { live: 2, ..rune("skuld", 3) }], max_live_dwarves: Some(9), ..world() };
        assert_eq!(called(&plan(&w)), ["skuld", "skuld", "skuld"]);
    }

    #[test_id(tx0hcw5)]
    #[test]
    fn a_closed_gate_calls_nobody_and_says_why() {
        for w in [CallWorld { world: World::Halted, ..world() }, CallWorld { skew: Some("old".into()), ..world() }, CallWorld { capacity: Capacity::Unknown, ..world() }] {
            let p = plan(&w);
            assert!(p.calls.is_empty() && p.refusal.is_some(), "{w:?}");
        }
    }

    #[test_id(tqne8an)]
    #[test]
    fn an_unreadable_ready_count_is_never_zero_and_calls_nobody() {
        let mut w = world();
        w.task[0].ready = None;
        let p = plan(&w);
        assert!(p.calls.is_empty());
        assert!(p.log.iter().any(|l| l.contains("CLAIM-ERROR")), "{:?}", p.log);
    }

    #[test_id(tqyp1ek)]
    #[test]
    fn the_fleet_ceiling_is_a_backstop_nobody_starts_above() {
        let w = CallWorld { task: vec![Rune { live: 40, ..rune("builder", 2) }], reactive: vec![Rune { reactive: true, ..rune("ops", 1) }], max_live_dwarves: Some(40), ..world() };
        let p = plan(&w);
        assert!(p.calls.is_empty(), "{:?}", p.log);
        assert!(p.log.iter().any(|l| l.contains("40/40 dwarf(s) live across the whole fleet — not calling")), "{:?}", p.log);
    }

    #[test_id(t7z30vj)]
    #[test]
    fn reactive_runes_are_called_before_planned_ones() {
        let w = CallWorld { task: vec![rune("builder", 1)], reactive: vec![Rune { reactive: true, ..rune("ops", 1) }], max_live_dwarves: Some(4), ..world() };
        let w = CallWorld { other_live: 3, ..w };
        let p = plan(&w);
        assert_eq!(called(&p), ["ops"], "{:?}", p.log);
    }

    #[test_id(tj7c50r)]
    #[test]
    fn throttled_with_an_express_bead_grants_exactly_one_restricted_slot() {
        let w = CallWorld { throttled: true, express_ready: true, max_live_dwarves: Some(5), task: vec![rune("builder", 5)], ..world() };
        let p = plan(&w);
        assert_eq!(p.calls, [Call { rune: "builder".into(), require_express: true }]);
        assert!(plan(&CallWorld { express_ready: false, ..w }).calls.is_empty(), "throttled with nothing express: the pool is 0");
    }

    #[test_id(t2e6f5q)]
    #[test]
    fn urgent_work_yields_the_reactive_runes_and_a_full_fleet_preempts_the_youngest_reactive_dwarf() {
        let mut w = world();
        w.task[0].live = 1;
        w.limits.insert("build-disk".into(), 9);
        w.reactive = vec![Rune { reactive: true, live: 3, uses: vec!["build-disk".into()], ..rune("triage", 1) }];
        w.reactive_dwarves = vec![ReactiveDwarf { rune: "triage".into(), bead: "sp-old".into(), started_ms: 1 }, ReactiveDwarf { rune: "triage".into(), bead: "sp-new".into(), started_ms: 9 }];
        w.urgent = Some(vec![Urgent { id: "sp-x".into(), p0: true, express: true }]);
        let p = plan(&w);
        assert_eq!(p.preempt, Some(Preempt { rune: "triage".into(), bead: "sp-new".into(), express: "sp-x".into() }));
        assert_eq!(called(&p), ["builder"], "the freed slot goes to the planned runes, never back to a reactive one");
        w.urgent = None;
        let p = plan(&w);
        assert!(p.preempt.is_none() && !called(&p).contains(&"triage"), "an unanswered urgent read yields the reactive runes");
    }

    #[test_id(t1pc5zg)]
    #[test]
    fn a_p0_behind_a_full_pool_holds_only_the_reactive_runes_that_use_a_bound_class() {
        let mut w = limited(vec![builder(2, 5)], &[("build-disk", 5)]);
        w.max_live_dwarves = Some(40);
        w.reactive = vec![using("skuld", 2, 0, &[], true), mender(1, 0)];
        w.urgent = Some(vec![Urgent { id: "sp-7nj5be".into(), p0: true, express: false }]);
        let p = plan(&w);
        assert_eq!(called(&p), ["skuld"], "{:?}", p.log);
        assert!(p.log.iter().any(|l| l == "CHECK7 mender: yielding — urgent bead(s) unclaimed: sp-7nj5be"), "{:?}", p.log);
    }

    #[test_id(tqkucrt)]
    #[test]
    fn an_unanswered_urgent_read_holds_only_the_reactive_runes_that_use_a_bound_class() {
        let mut w = limited(vec![builder(0, 0)], &[("build-disk", 5)]);
        w.reactive = vec![using("skuld", 1, 0, &[], true), mender(1, 0)];
        w.urgent = None;
        let p = plan(&w);
        assert_eq!(called(&p), ["skuld"], "{:?}", p.log);
        assert!(p.log.iter().any(|l| l == "CHECK7 mender: yielding — the urgent read did not answer"), "{:?}", p.log);
    }

    #[test_id(tj4jrgp)]
    #[test]
    fn urgent_work_logs_no_preemption_when_no_reactive_rune_is_rostered() {
        let mut w = world();
        w.task[0].live = 4;
        w.urgent = Some(vec![Urgent { id: "sp-x".into(), p0: true, express: true }]);
        let p = plan(&w);
        assert!(p.preempt.is_none() && !p.log.iter().any(|l| l.contains("CHECK7 reactive:")), "{:?}", p.log);
    }

    fn room(w: &CallWorld) -> Option<i64> {
        plan(w).task_room
    }

    #[test_id(t8ruqi7)]
    #[test]
    fn task_room_is_zero_when_the_task_pool_is_full() {
        let mut w = world();
        w.limits.insert("build-disk".into(), 7);
        w.task[0].uses = vec!["build-disk".into()];
        w.task[0].live = 7;
        w.max_live_dwarves = None;
        assert_eq!(room(&w), Some(0));
    }

    #[test_id(t4wn34b)]
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

    #[test_id(t23wida)]
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

    #[test_id(t987g2e)]
    #[test]
    fn task_room_is_zero_when_throttled_or_halted() {
        assert_eq!(room(&CallWorld { throttled: true, ..world() }), Some(0));
        for closed in [CallWorld { world: World::Halted, ..world() }, CallWorld { world: World::Draining, ..world() }, CallWorld { skew: Some("old".into()), ..world() }, CallWorld { capacity: Capacity::Paused, ..world() }] {
            assert_eq!(room(&closed), Some(0), "{closed:?}");
        }
    }

    #[test_id(trzrxd7)]
    #[test]
    fn task_room_is_unknown_when_no_ceiling_is_configured() {
        assert_eq!(room(&CallWorld { max_live_dwarves: None, ..world() }), None);
    }

    fn binding(w: &CallWorld) -> Option<(String, i64, i64)> {
        plan(w).task_limit.map(|t| (t.name, t.limit, t.held))
    }

    #[test_id(tbvszbn)]
    #[test]
    fn a_full_pool_under_a_roomy_ceiling_is_bound_by_the_pool() {
        let mut w = world();
        w.limits.insert("build-disk".into(), 7);
        w.task[0].uses = vec!["build-disk".into()];
        w.task[0].live = 7;
        w.max_live_dwarves = Some(9);
        assert_eq!(binding(&w), Some(("pool".into(), 7, 7)));
    }

    #[test_id(t02kv34)]
    #[test]
    fn a_full_fleet_with_pool_room_is_bound_by_the_ceiling() {
        let mut w = world();
        w.limits.insert("build-disk".into(), 7);
        w.task[0].uses = vec!["build-disk".into()];
        w.task[0].live = 3;
        w.other_live = 6;
        w.max_live_dwarves = Some(9);
        assert_eq!(binding(&w), Some(("ceiling".into(), 9, 9)));
    }

    #[test_id(t1tve7z)]
    #[test]
    fn a_throttle_or_a_closed_gate_names_itself_as_the_limit() {
        assert_eq!(binding(&CallWorld { throttled: true, ..world() }).map(|b| b.0), Some("throttle".into()));
        assert_eq!(binding(&CallWorld { world: World::Halted, ..world() }).map(|b| b.0), Some("closed".into()));
    }

    #[test_id(tff0mgz)]
    #[test]
    fn no_ceiling_and_no_pool_names_no_limit() {
        assert_eq!(binding(&CallWorld { max_live_dwarves: None, ..world() }), None);
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

    #[test_id(tcqbrqa)]
    #[test]
    fn a_session_with_no_bound_class_is_admitted_with_every_limit_full() {
        let w = limited(vec![builder(0, 7), using("hasher", 0, 1, &["cpu"], false), rune("spike", 1)], &[("build-disk", 7), ("cpu", 1)]);
        let p = plan(&w);
        assert_eq!(called(&p), ["spike"], "{:?}", p.log);
    }

    #[test_id(tc80jov)]
    #[test]
    fn a_planned_build_disk_session_waits_at_the_limit() {
        let w = limited(vec![builder(3, 5), mender(0, 2)], &[("build-disk", 7)]);
        let p = plan(&w);
        assert!(p.calls.is_empty(), "{:?}", p.log);
        assert!(p.log.iter().any(|l| l.contains("build-disk 7 of 7 held — no backfill")), "{:?}", p.log);
    }

    #[test_id(tm5gojh)]
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

    #[test_id(tcuzba7)]
    #[test]
    fn a_limit_on_a_class_makes_only_its_declarers_wait() {
        let tasks = || vec![using("a", 1, 1, &["memory"], false), using("b", 1, 0, &[], false)];
        assert_eq!(called(&plan(&limited(tasks(), &[("memory", 1)]))), ["b"]);
        assert_eq!(called(&plan(&limited(tasks(), &[]))), ["a", "b"]);
    }

    #[test_id(td1eg6e)]
    #[test]
    fn a_reactive_session_over_the_limit_logs_it_and_a_live_one_leaves_no_room_for_a_builder() {
        let p = plan(&limited(vec![builder(3, 7), mender(1, 0)], &[("build-disk", 7)]));
        assert!(p.log.iter().any(|l| l.contains("build-disk 7 of 7 held — reactive, starting over the limit")), "{:?}", p.log);
        let p = plan(&limited(vec![builder(3, 5), mender(0, 2)], &[("build-disk", 7)]));
        assert_eq!(p.task_room, Some(0), "a live mender fills the class: {:?}", p.log);
    }

    #[test_id(tifsol8)]
    #[test]
    fn bound_reports_held_waiting_and_over() {
        let p = plan(&limited(vec![builder(3, 7), mender(0, 2)], &[("build-disk", 7)]));
        assert_eq!(p.bound, [Bound { class: "build-disk".into(), limit: 7, held: 9, waiting: 3, over: true }], "{:?}", p.log);
    }

    #[test_id(t02idte)]
    #[test]
    fn uncalled_names_the_refusal_that_stopped_each_rune_with_ready_work() {
        let mut w = limited(vec![builder(3, 5), mender(0, 2), rune("idle", 0), rune("blind", 1), rune("served", 1)], &[("build-disk", 7)]);
        w.task[3].ready = None;
        w.task[3].uses = vec![];
        let p = plan(&w);
        assert_eq!(p.uncalled.get("builder").map(String::as_str), Some("build-disk 7 of 7 held — no backfill"), "{:?}", p.log);
        assert!(p.uncalled["blind"].contains("CLAIM-ERROR"), "{:?}", p.uncalled);
        assert_eq!(called(&p), ["served"]);
        assert!(!p.uncalled.contains_key("served") && !p.uncalled.contains_key("idle") && !p.uncalled.contains_key("mender"), "{:?}", p.uncalled);

        let w = CallWorld { task: vec![Rune { live: 40, ..rune("builder", 2) }, rune("idle", 0)], max_live_dwarves: Some(40), ..world() };
        let p = plan(&w);
        assert_eq!(p.uncalled.get("builder").map(String::as_str), Some("40/40 dwarf(s) live across the whole fleet — not calling"));
        assert!(!p.uncalled.contains_key("idle"));

        let w = CallWorld { task: vec![Rune { single_flight: true, live: 1, ..rune("capped", 2) }], max_live_dwarves: Some(9), ..world() };
        let p = plan(&w);
        assert_eq!(p.uncalled.get("capped").map(String::as_str), Some("2 ready, at concurrency cap"), "{:?}", p.log);
    }

    #[test_id(tp76e1v)]
    #[test]
    fn a_closed_gate_gives_every_ready_rune_the_gates_reason() {
        let mut w = CallWorld { world: World::Halted, ..world() };
        w.reactive = vec![Rune { reactive: true, ..rune("ops", 1) }, Rune { reactive: true, ..rune("quiet", 0) }];
        let p = plan(&w);
        let why = p.refusal.clone().unwrap();
        assert_eq!(p.uncalled, BTreeMap::from([("builder".to_string(), why.clone()), ("ops".to_string(), why)]));
    }

    #[test_id(tpeg07n)]
    #[test]
    fn the_urgent_yield_is_the_reason_a_bound_reactive_rune_is_uncalled() {
        let mut w = limited(vec![builder(1, 0)], &[("build-disk", 7)]);
        w.reactive = vec![mender(1, 0)];
        w.urgent = Some(vec![Urgent { id: "sp-x".into(), p0: true, express: false }]);
        let p = plan(&w);
        assert_eq!(p.uncalled.get("mender").map(String::as_str), Some("yielding — urgent bead(s) unclaimed: sp-x"));
    }

    #[test_id(thmsiov)]
    #[test]
    fn a_builder_takes_the_room_of_its_tightest_declared_class() {
        let mut b = builder(5, 0);
        b.uses.push("memory".into());
        let p = plan(&limited(vec![b], &[("build-disk", 4), ("memory", 2)]));
        assert_eq!(called(&p), ["builder", "builder"], "{:?}", p.log);
    }

    #[test_id(t064xy9)]
    #[test]
    fn a_full_pool_names_only_the_beads_of_the_runes_that_declare_build_disk() {
        let mut w = limited(vec![builder(1, 4)], &[("build-disk", 4)]);
        w.reactive = vec![using("skuld", 0, 2, &[], true)];
        w.live_dwarves = [("builder", "sp-b0"), ("skuld", "sp-s0"), ("builder", "sp-b1"), ("builder", "sp-b2"), ("skuld", "sp-s1"), ("builder", "sp-b3")]
            .map(|(rune, bead)| LiveDwarf { rune: rune.into(), bead: bead.into() })
            .into();
        let limit = plan(&w).task_limit.unwrap();
        assert_eq!((limit.name.as_str(), limit.held), ("pool", 4));
        assert_eq!(limit.holders, ["sp-b0", "sp-b1", "sp-b2", "sp-b3"]);
    }

    #[test_id(tpei7qc)]
    #[test]
    fn every_limit_but_the_pool_names_every_live_dwarf_on_the_roster() {
        let mut w = limited(vec![builder(1, 1)], &[]);
        w.reactive = vec![using("skuld", 0, 1, &[], true)];
        w.live_dwarves = vec![LiveDwarf { rune: "builder".into(), bead: "sp-b".into() }, LiveDwarf { rune: "skuld".into(), bead: "sp-s".into() }, LiveDwarf { rune: "gone".into(), bead: "sp-g".into() }];
        w.max_live_dwarves = Some(2);
        for (w, name) in [(w.clone(), "ceiling"), (CallWorld { throttled: true, ..w.clone() }, "throttle"), (CallWorld { world: World::Halted, ..w }, "closed")] {
            let limit = plan(&w).task_limit.unwrap();
            assert_eq!((limit.name.as_str(), limit.holders), (name, vec!["sp-b".to_string(), "sp-s".to_string()]));
        }
    }

    #[test_id(thin1lr)]
    #[test]
    fn a_limit_never_names_more_beads_of_a_rune_than_it_counted_and_invents_none() {
        let mut w = limited(vec![builder(0, 1), using("mender", 0, 2, &["build-disk"], false)], &[("build-disk", 3)]);
        w.live_dwarves = ["sp-new", "sp-ended"].map(|b| LiveDwarf { rune: "builder".into(), bead: b.into() }).into();
        let limit = plan(&w).task_limit.unwrap();
        assert_eq!((limit.held, limit.holders), (3, vec!["sp-new".to_string()]));
    }
}
