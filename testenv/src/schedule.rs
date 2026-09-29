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
    OverrideUnlimited,
}

impl Binding {
    pub fn as_str(self) -> &'static str {
        match self {
            Binding::Cpu => "cpu",
            Binding::Memory => "memory",
            Binding::Override => "override",
            Binding::OverrideUnlimited => "override-unlimited",
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
    /// SPIRA_BATCH_MAXPAR as given (0 = unlimited; larger than the hardware bound is clamped).
    pub requested: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maxpar {
    /// 0 = unlimited.
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
    let (value, binding) = match i.requested {
        Some(0) => (0, Binding::OverrideUnlimited),
        Some(n) if n > 0 && n <= hardware as i64 => (n as u32, Binding::Override),
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
    fn cpu_bound_when_memory_is_plentiful() {
        let m = maxpar(&inp(8, 64_000, None));
        assert_eq!((m.value, m.binding), (8, Binding::Cpu));
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
        let mut i = inp(8, 64_000, None);
        i.ceiling = Some(24);
        assert_eq!(maxpar(&i).value, 24);
        i.ceiling = Some(0);
        assert_eq!(maxpar(&i).value, 8);
    }

    #[test]
    fn override_is_a_ceiling_and_zero_is_unlimited() {
        assert_eq!(
            maxpar(&inp(32, 64_000, Some(16))).binding,
            Binding::Override
        );
        assert_eq!(maxpar(&inp(32, 64_000, Some(16))).value, 16);
        let m = maxpar(&inp(8, 64_000, Some(16)));
        assert_eq!((m.value, m.binding), (8, Binding::Cpu));
        assert_eq!(maxpar(&inp(8, 64_000, Some(0))).value, 0);
        assert_eq!(
            maxpar(&inp(8, 64_000, Some(0))).binding,
            Binding::OverrideUnlimited
        );
    }

    fn job(n: &str, excl: bool) -> Job {
        Job {
            name: n.into(),
            exclusive: excl.then(|| "heavy".to_string()),
        }
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
