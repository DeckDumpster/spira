//! Fire-and-forget OTLP metrics for `queue`: one run and its seconds per invocation, by operation and outcome.

use spira_otel::metrics::{self, MetricsSink};

pub const COMPONENT: &str = "queue";
pub const OPERATIONS: &[&str] = &[
    "submit",
    "protect",
    "stats",
    "churn",
    "flush",
    "step",
    "verdict",
    "eject",
    "abandon",
    "open-batch",
    "claim",
    "release",
    "resubmit",
    "absolve",
    "rework",
    "forced-suites",
    "land-local",
    "publish",
    "publish-settle",
    "to-forge",
    "to-local",
    "mode-switch",
    "rollback-local",
    "rebase-waiting",
    "round",
];

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

/// Seconds a verb held a repo's queue lock, labelled by `verb` and `repo`.
pub const LOCK_HOLD: &str = "spira_queue_lock_hold_seconds";

pub fn run(sink: &MetricsSink, first_arg: Option<&str>, f: impl FnOnce() -> i32) -> i32 {
    let op = operation(first_arg);
    let code = metrics::timed(sink, COMPONENT, op, f);
    push_holds(sink, &crate::lock::take_holds());
    if let Some(event) = batch_event(op).filter(|_| code == 0) {
        sink.push(
            &[metrics::Point::Count {
                name: "spira_queue_batch_events",
                value: 1,
            }],
            &[("event", event)],
        );
    }
    code
}

/// One sample per hold a verb gave back.
pub fn push_holds(sink: &MetricsSink, holds: &[crate::lock::Hold]) {
    for h in holds {
        if let crate::lock::Hold::Released { repo, verb, held } = h {
            sink.push(
                &[metrics::Point::Seconds {
                    name: LOCK_HOLD,
                    value: held.as_secs_f64(),
                }],
                &[("verb", verb), ("repo", repo)],
            );
        }
    }
}

/// The batch lifecycle event a successful `queue` operation is.
pub fn batch_event(op: &str) -> Option<&'static str> {
    match op {
        "open-batch" => Some("opened"),
        "abandon" => Some("abandoned"),
        "land-local" => Some("landed"),
        "rollback-local" => Some("rolled_back"),
        "eject" => Some("ejected"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_otel::metrics::fixture;

    #[test]
    fn a_run_pushes_its_count_and_seconds_labelled_by_component_operation_and_outcome() {
        let (code, got) = fixture::capture(|s| run(s, Some("submit"), || 1));
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
            (Some("queue"), Some("submit"), Some("error"))
        );
        assert!(
            got.iter().any(|c| c.name == metrics::RUN_DURATION),
            "run seconds pushed"
        );
    }

    #[test]
    fn each_queue_lock_hold_a_verb_gave_back_is_one_sample_by_verb_and_repo() {
        let (_, got) = fixture::capture(|s| {
            push_holds(
                s,
                &[
                    crate::lock::Hold::Taken {
                        repo: "spira".into(),
                        verb: "round open".into(),
                    },
                    crate::lock::Hold::Released {
                        repo: "spira".into(),
                        verb: "round open".into(),
                        held: std::time::Duration::from_millis(1500),
                    },
                ],
            );
            0
        });
        let holds: Vec<_> = got.iter().filter(|c| c.name == LOCK_HOLD).collect();
        assert_eq!(holds.len(), 1, "a hold still taken is no sample");
        assert_eq!(
            (holds[0].label("verb"), holds[0].label("repo")),
            (Some("round open"), Some("spira"))
        );
    }

    #[test]
    fn an_argument_that_is_no_operation_is_other() {
        assert_eq!(operation(Some("sp-abc123")), "other");
        assert_eq!(operation(None), "other");
    }
}
