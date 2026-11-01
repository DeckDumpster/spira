//! Fire-and-forget OTLP metrics for `round-controller`: one run and its seconds per invocation,
//! by verb and outcome (law-every-component-sends-otlp).

use spira_otel::metrics::{self, MetricsSink, Point};

use crate::outcome::{Cause, Log, Outcome};
use crate::slots::Pool;

pub const COMPONENT: &str = "round-controller";
pub const OPERATIONS: &[&str] = &["version", "pass"];
/// Runner slots held under a live lock, labelled by `pool`.
pub const SLOTS_IN_USE: &str = "spira_round_controller_slots_in_use";
/// Seconds a round waited GREEN before it landed, labelled by `repo`.
pub const GREEN_TO_LANDED: &str = "spira_round_controller_green_to_landed_seconds";

pub fn sink() -> MetricsSink {
    metrics::sink_for(
        spira_config::process::cfg("SPIRA_METRICS_ENDPOINT")
            .ok()
            .as_deref(),
        COMPONENT,
    )
}

pub fn operation(first_arg: Option<&str>) -> &'static str {
    metrics::operation(first_arg, OPERATIONS)
}

pub fn run(sink: &MetricsSink, first_arg: Option<&str>, f: impl FnOnce() -> i32) -> i32 {
    metrics::timed(sink, COMPONENT, operation(first_arg), f)
}

/// One gauge sample per pool.
pub fn push_slots(sink: &MetricsSink, used: &[(Pool, u32)]) {
    for (pool, n) in used {
        sink.push(
            &[Point::Gauge {
                name: SLOTS_IN_USE,
                value: f64::from(*n),
            }],
            &[("pool", pool.word())],
        );
    }
}

/// Every outcome written to `inner`, and each landing's dwell pushed as one sample per repo.
pub struct DwellLog<'a> {
    pub sink: &'a MetricsSink,
    pub inner: &'a dyn Log,
}

impl Log for DwellLog<'_> {
    fn write(&self, o: &Outcome) {
        self.inner.write(o);
        if let Cause::Dwell { ms } = o.cause {
            self.sink.push(
                &[Point::Seconds {
                    name: GREEN_TO_LANDED,
                    value: ms as f64 / 1000.0,
                }],
                &[("repo", o.repo.as_str())],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_run_pushes_its_count_and_seconds_labelled_by_component_verb_and_outcome() {
        let (code, got) = fixture::capture(|s| run(s, Some("pass"), || 1));
        assert_eq!(code, 1);
        let runs = got
            .iter()
            .find(|c| c.name == metrics::RUNS)
            .expect("run count pushed");
        assert_eq!(
            (
                runs.label("component"),
                runs.label("operation"),
                runs.label("outcome")
            ),
            (Some(COMPONENT), Some("pass"), Some("error"))
        );
        assert!(
            got.iter().any(|c| c.name == metrics::RUN_DURATION),
            "run seconds pushed"
        );
    }

    #[test]
    fn slots_in_use_are_one_gauge_per_pool_labelled_by_pool() {
        let ((), got) = fixture::capture(|s| push_slots(s, &[(Pool::Proxmox, 1), (Pool::Ec2, 2)]));
        let mut by_pool: Vec<(String, String)> = got
            .iter()
            .filter(|c| c.name == SLOTS_IN_USE)
            .map(|c| {
                (
                    c.label("pool").unwrap_or("").to_string(),
                    c.value.to_string(),
                )
            })
            .collect();
        by_pool.sort();
        assert_eq!(
            by_pool,
            [("ec2".into(), "2".into()), ("proxmox".into(), "1".into())]
        );
    }

    #[test]
    fn a_landing_pushes_its_dwell_by_repo_and_every_outcome_reaches_the_inner_log() {
        let mem = crate::outcome::MemLog::default();
        let ((), got) = fixture::capture(|s| {
            let log = DwellLog { sink: s, inner: &mem };
            log.write(&Outcome::landing(0, "lane", "k1", 90_000));
            log.write(&Outcome::visited(0, "lane"));
        });
        assert_eq!(mem.outcomes().len(), 2);
        let dwell: Vec<_> = got.iter().filter(|c| c.name == GREEN_TO_LANDED).collect();
        assert_eq!(dwell.len(), 1, "one sample, for the landing only");
        assert_eq!(
            (dwell[0].label("repo"), dwell[0].value.to_string()),
            (Some("lane"), "90".to_string())
        );
    }

    #[test]
    fn each_verb_is_its_own_operation_and_anything_else_is_other() {
        assert_eq!(operation(Some("version")), "version");
        assert_eq!(operation(Some("--repo")), "other");
        assert_eq!(operation(None), "other");
    }
}
