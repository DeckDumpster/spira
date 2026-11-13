//! Fire-and-forget OTLP metrics for `round-controller`: one run and its seconds per invocation,
//! by verb and outcome (law-every-component-sends-otlp).

use spira_otel::metrics::{self, MetricsSink, Point};

use crate::outcome::{Cause, Log, Outcome};
use crate::slots::Pool;

pub const COMPONENT: &str = "round-controller";
pub const OPERATIONS: &[&str] = &["version", "pass", "sifts"];
/// Runner slots held under a live lock, labelled by `pool`.
pub const SLOTS_IN_USE: &str = "spira_round_controller_slots_in_use";
/// Seconds a round waited GREEN before it landed, labelled by `repo`.
pub const GREEN_TO_LANDED: &str = "spira_round_controller_green_to_landed_seconds";
/// The oldest GREEN round's wait in minutes, as the repo's section last published it.
pub const GREEN_UNLANDED_MINUTES: &str = "spira_round_controller_green_unlanded_minutes";
/// Planner passes over the trailing 24 h on an identity that already held a verdict.
pub const REPEAT_IDENTITY_PASSES: &str = "spira_round_controller_repeat_identity_passes_24h";

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
        match o.cause {
            Cause::Dwell { ms } => self.sink.push(
                &[Point::Seconds {
                    name: GREEN_TO_LANDED,
                    value: ms as f64 / 1000.0,
                }],
                &[("repo", o.repo.as_str())],
            ),
            Cause::Section {
                green_unlanded_minutes,
                repeat_identity_passes_24h,
            } => self.sink.push(
                &[
                    Point::Gauge {
                        name: GREEN_UNLANDED_MINUTES,
                        value: green_unlanded_minutes as f64,
                    },
                    Point::Gauge {
                        name: REPEAT_IDENTITY_PASSES,
                        value: repeat_identity_passes_24h as f64,
                    },
                ],
                &[("repo", o.repo.as_str())],
            ),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_verb_is_its_own_operation_and_anything_else_is_other() {
        assert_eq!(operation(Some("version")), "version");
        assert_eq!(operation(Some("--repo")), "other");
        assert_eq!(operation(None), "other");
    }
}
