//! Pure scheduling: how many suites at once (maxpar) and in what order (exclusive first,
//! then longest-first). DESIGN.md §4.3.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Binding {
    Cpu,
    Memory,
    Override,
    /// No `requested` value at all: a quarter of the CPU ceiling, clamped to [2, 16] —
    /// never the hardware bound (sp-tj8k3).
    Default,
}

impl Binding {
    pub fn as_str(self) -> &'static str {
        match self {
            Binding::Cpu => "cpu",
            Binding::Memory => "memory",
            Binding::Override => "override",
            Binding::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaxparInputs {
    pub nproc: u32,
    /// SPIRA_BATCH_MAXPAR_CEILING, used only when positive.
    pub ceiling: Option<u32>,
    pub mem_avail_mib: i64,
    pub mem_reserve_mib: i64,
    pub mem_per_suite_mib: i64,
    /// SPIRA_BATCH_MAXPAR as a positive request, already refused by the settings layer when
    /// it was given as zero or something unparsable (sp-tj8k3: "unlimited" is no longer a
    /// value this ever carries) — larger than the hardware bound is clamped; `None` (the key
    /// was absent) gets the scheduler's own small default, below.
    pub requested: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maxpar {
    pub value: u32,
    pub binding: Binding,
    pub hardware: u32,
    pub hardware_binding: Binding,
    pub cpu_ceiling: u32,
}

pub fn maxpar(i: &MaxparInputs) -> Maxpar {
    let cpu_ceiling = i.ceiling.filter(|c| *c > 0).unwrap_or(i.nproc).max(1);
    let per = i.mem_per_suite_mib.max(1);
    let budget = (i.mem_avail_mib - i.mem_reserve_mib).max(per);
    let mem_bound = (budget / per).max(1) as u32;
    let (hardware, hardware_binding) = if cpu_ceiling <= mem_bound {
        (cpu_ceiling, Binding::Cpu)
    } else {
        (mem_bound, Binding::Memory)
    };
    // sp-tj8k3: on 2026-10-01, an unset SPIRA_BATCH_MAXPAR resolved to `hardware` — the full
    // core count on a plentiful-memory host — and every concurrent gate and testenv
    // invocation independently claimed that whole box, putting 61 containers and load 90 on
    // one 32-core host. The unset default is now a quarter of the CPU ceiling, clamped to a
    // small fixed range, and still never above what the box can actually support.
    let default_cap = (cpu_ceiling / 4).clamp(2, 16);
    let (value, binding) = match i.requested {
        Some(n) if n > 0 && n <= hardware as i64 => (n as u32, Binding::Override),
        Some(n) if n > 0 => (hardware, hardware_binding),
        _ if default_cap < hardware => (default_cap, Binding::Default),
        _ => (hardware, hardware_binding),
    };
    Maxpar {
        value,
        binding,
        hardware,
        hardware_binding,
        cpu_ceiling,
    }
}

/// A suite as the scheduler sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    pub exclusive: Option<String>,
    /// Declared `# pids: N`: processes the suite may hold at its peak.
    pub weight: u32,
    /// Declared `# lane: <name>`: suites of a lane share that lane's own slot budget.
    pub lane: Option<String>,
}

impl Job {
    pub fn new(name: &str, exclusive: Option<String>) -> Job {
        Job { name: name.into(), exclusive, weight: DEFAULT_PIDS_WEIGHT, lane: None }
    }
}

/// The container's `--pids-limit`; the one number `testenv up` and the admission share.
pub const PIDS_LIMIT: u32 = 8192;
/// Share of [`PIDS_LIMIT`] the running suites' declared weights may fill.
pub const PIDS_HEADROOM_PCT: u32 = 75;
/// Weight of a suite that declares none.
pub const DEFAULT_PIDS_WEIGHT: u32 = 16;
/// Suites of one lane running at once, whatever the pool's slots allow.
pub const LANE_SLOTS: u32 = 2;

pub fn pids_budget() -> u32 {
    PIDS_LIMIT / 100 * PIDS_HEADROOM_PCT
}

/// What may start next: the pool's slots, the declared process weight of what is running
/// against the pids budget, and each lane's own slots. A suite heavier than the whole budget
/// still runs, alone.
#[derive(Debug, Clone, Default)]
pub struct Admission {
    pub slots: u32,
    pub budget: u32,
    pub lane_slots: u32,
    running: u32,
    weight: u32,
    lanes: HashMap<String, u32>,
}

impl Admission {
    pub fn new(slots: u32, budget: u32, lane_slots: u32) -> Admission {
        Admission { slots, budget, lane_slots, ..Admission::default() }
    }

    pub fn running(&self) -> u32 {
        self.running
    }

    pub fn fits(&self, j: &Job) -> bool {
        if self.running == 0 {
            return true;
        }
        if self.slots > 0 && self.running >= self.slots {
            return false;
        }
        if self.budget > 0 && self.weight.saturating_add(j.weight) > self.budget {
            return false;
        }
        match &j.lane {
            Some(l) if self.lane_slots > 0 => self.lanes.get(l).copied().unwrap_or(0) < self.lane_slots,
            _ => true,
        }
    }

    pub fn admit(&mut self, j: &Job) {
        self.running += 1;
        self.weight = self.weight.saturating_add(j.weight);
        if let Some(l) = &j.lane {
            *self.lanes.entry(l.clone()).or_default() += 1;
        }
    }

    pub fn release(&mut self, weight: u32, lane: &Option<String>) {
        self.running = self.running.saturating_sub(1);
        self.weight = self.weight.saturating_sub(weight);
        if let Some(c) = lane.as_ref().and_then(|l| self.lanes.get_mut(l)) {
            *c = c.saturating_sub(1);
        }
    }
}

/// The caller's order, exactly: exclusive suites first (they cannot share the box), each group in
/// the order given. Used for an explicit `--suites` list, whose order is the caller's decision.
pub fn given(jobs: &[Job]) -> Vec<Job> {
    let mut out: Vec<Job> = jobs.iter().filter(|j| j.exclusive.is_some()).cloned().collect();
    out.extend(jobs.iter().filter(|j| j.exclusive.is_none()).cloned());
    out
}

/// Exclusive suites first (in selection order), then the pool longest-first by recorded mean
/// wall time. A suite with no record sorts as the longest on record + 1 — an unmeasured
/// suite is the one a late start costs most. Ties keep selection order.
pub fn order(jobs: &[Job], mean_wall: &HashMap<String, f64>) -> Vec<Job> {
    let longest = mean_wall
        .values()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0_f64, f64::max);
    let sentinel = longest + 1.0;
    let mut out: Vec<Job> = jobs
        .iter()
        .filter(|j| j.exclusive.is_some())
        .cloned()
        .collect();
    let mut pool: Vec<(f64, usize, &Job)> = jobs
        .iter()
        .enumerate()
        .filter(|(_, j)| j.exclusive.is_none())
        .map(|(idx, j)| {
            (
                mean_wall
                    .get(&j.name)
                    .copied()
                    .filter(|v| v.is_finite())
                    .unwrap_or(sentinel),
                idx,
                j,
            )
        })
        .collect();
    pool.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    out.extend(pool.into_iter().map(|(_, _, j)| j.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp(nproc: u32, avail: i64, requested: Option<i64>) -> MaxparInputs {
        MaxparInputs {
            nproc,
            ceiling: None,
            mem_avail_mib: avail,
            mem_reserve_mib: 1024,
            mem_per_suite_mib: 192,
            requested,
        }
    }

    #[test]
    fn unset_defaults_to_a_quarter_of_cores_clamped() {
        let m = maxpar(&inp(8, 64_000, None));
        assert_eq!((m.value, m.binding), (2, Binding::Default));
        // The hardware term is still correctly cpu-bound when memory is plentiful — the
        // default just no longer uses it as the *value*.
        assert_eq!((m.hardware, m.hardware_binding), (8, Binding::Cpu));
    }

    /// sp-tj8k3: on 2026-10-01, batch_maxpar removed from production config left `requested`
    /// None on a 32-core host; the scheduler's own "unset" value resolved to the full
    /// hardware bound (32), and with two gates plus one testenv run concurrent, each
    /// claiming the whole box, that put 61 containers and load 90 on one host. Unset must
    /// resolve to a small, host-derived default — never the hardware ceiling.
    #[test]
    fn unset_never_resolves_to_the_hardware_ceiling() {
        let m = maxpar(&inp(32, 64_000, None));
        assert_ne!(m.value, 0, "unset must never mean unlimited");
        assert!(
            m.value <= 16,
            "unset must stay a small, host-derived default, got {} on a 32-core host",
            m.value
        );
    }

    #[test]
    fn memory_bound_when_memory_is_short() {
        // (2048 - 1024) / 192 = 5
        let m = maxpar(&inp(32, 2048, None));
        assert_eq!((m.value, m.binding), (5, Binding::Memory));
    }

    #[test]
    fn budget_never_drops_below_one_suite() {
        let m = maxpar(&inp(32, 100, None));
        assert_eq!(m.value, 1);
    }

    #[test]
    fn ceiling_raises_the_cpu_term() {
        // The ceiling raises `hardware` (the term an explicit override clamps against) —
        // it is not itself the unset default, which stays a small fraction of it.
        let mut i = inp(8, 64_000, None);
        i.ceiling = Some(24);
        assert_eq!(maxpar(&i).hardware, 24);
        i.ceiling = Some(0);
        assert_eq!(maxpar(&i).hardware, 8);
    }

    #[test]
    fn override_is_a_ceiling_and_a_non_positive_request_falls_back_to_the_default() {
        assert_eq!(
            maxpar(&inp(32, 64_000, Some(16))).binding,
            Binding::Override
        );
        assert_eq!(maxpar(&inp(32, 64_000, Some(16))).value, 16);
        let m = maxpar(&inp(8, 64_000, Some(16)));
        assert_eq!((m.value, m.binding), (8, Binding::Cpu));
        // sp-tj8k3: zero and anything unparsable are refused by the settings layer before
        // `requested` is ever built, never read down here as "unlimited" — but the pure
        // scheduler still treats a non-positive value the same as absent, defensively.
        let m = maxpar(&inp(8, 64_000, Some(0)));
        assert_eq!((m.value, m.binding), (2, Binding::Default));
    }

    fn job(n: &str, excl: bool) -> Job {
        Job::new(n, excl.then(|| "heavy".to_string()))
    }

    fn heavy(n: &str, weight: u32, lane: Option<&str>) -> Job {
        Job { weight, lane: lane.map(str::to_string), ..Job::new(n, None) }
    }

    /// Replays an LPT order of heavy suites through the admission, completing the oldest
    /// running suite whenever the next does not fit, and returns the peak declared weight.
    fn peak_weight(jobs: &[Job], mut adm: Admission) -> u32 {
        let (mut running, mut peak): (Vec<&Job>, u32) = (vec![], 0);
        for j in jobs {
            while !adm.fits(j) {
                let done = running.remove(0);
                adm.release(done.weight, &done.lane);
            }
            adm.admit(j);
            running.push(j);
            peak = peak.max(running.iter().map(|r| r.weight).sum());
        }
        peak
    }

    #[test]
    fn heavy_suites_in_lpt_order_never_exceed_the_pids_budget() {
        let jobs: Vec<Job> = (0..40).map(|i| heavy(&format!("s{i}"), 1500, None)).collect();
        let unbounded = peak_weight(&jobs, Admission::new(32, 0, 0));
        assert!(unbounded > PIDS_LIMIT, "the fixture must exceed the limit without admission, got {unbounded}");
        assert!(peak_weight(&jobs, Admission::new(32, pids_budget(), 0)) <= pids_budget());
    }

    #[test]
    fn a_suite_heavier_than_the_budget_runs_alone() {
        let mut adm = Admission::new(4, 100, 0);
        let big = heavy("big", 500, None);
        assert!(adm.fits(&big));
        adm.admit(&big);
        assert!(!adm.fits(&heavy("small", 1, None)));
        adm.release(500, &None);
        assert!(adm.fits(&heavy("small", 1, None)));
    }

    #[test]
    fn a_lane_has_its_own_slots_and_leaves_the_pool_free() {
        let mut adm = Admission::new(8, 0, 2);
        let sim = heavy("sim", 1, Some("sim"));
        adm.admit(&sim);
        adm.admit(&sim);
        assert!(!adm.fits(&sim));
        assert!(adm.fits(&heavy("plain", 1, None)));
        adm.release(1, &Some("sim".into()));
        assert!(adm.fits(&sim));
    }

    #[test]
    fn given_keeps_the_callers_order_with_exclusive_first() {
        let j = |n: &str, x: bool| Job::new(n, x.then(|| "x".into()));
        let jobs = vec![j("c", false), j("a", false), j("x", true), j("b", false)];
        let names: Vec<String> = given(&jobs).into_iter().map(|j| j.name).collect();
        assert_eq!(names, ["x", "c", "a", "b"], "no re-sorting: the caller's order stands");
    }

    #[test]
    fn exclusive_first_then_longest_first_with_unmeasured_as_longest() {
        let jobs = vec![
            job("a", false),
            job("b", false),
            job("x", true),
            job("c", false),
            job("new", false),
        ];
        let wall: HashMap<String, f64> = [("a", 5.0), ("b", 50.0), ("c", 20.0)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let names: Vec<String> = order(&jobs, &wall).into_iter().map(|j| j.name).collect();
        assert_eq!(names, vec!["x", "new", "b", "c", "a"]);
    }

    #[test]
    fn no_history_keeps_selection_order() {
        let jobs = vec![job("a", false), job("b", false), job("c", false)];
        let names: Vec<String> = order(&jobs, &HashMap::new())
            .into_iter()
            .map(|j| j.name)
            .collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
