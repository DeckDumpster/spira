//! Wide events as OTLP log records through the collector's file receiver. Nothing here names a field.

use runtime::{Fields, Value, WideSink};
use spira_otel::logs::{Attr, LogQueue, LogSink};
use spira_otel::metrics::{MetricsSink, Point};

/// A gauge of a monotonic total: the store drops sums, so the alarm reads the latest value.
pub const SPILLED_TOTAL: &str = "spira_runtime_offload_spilled_total";
/// A gauge of the runs of a task that found work on the sweep alone: each is a missed wake-up.
pub const SWEEP_FOUND_TOTAL: &str = "spira_runtime_sweep_found_total";
const SPILL_UNIT: &str = "offload.spill";
const SWEEP_FOUND_UNIT: &str = "wake.sweep_found";

pub struct OtlpSink {
    queue: LogQueue,
    debug: bool,
}

/// Units the engine writes for every successful turn of the loop. Failures, overruns and
/// quarantines are never among them.
pub fn is_routine(fields: &Fields) -> bool {
    let text = |k: &str| match fields.get(k) {
        Some(Value::Str(s)) => s.as_str(),
        _ => "",
    };
    match text("unit") {
        "dispatch" => text("outcome") == "ok",
        "effect" => text("effect.outcome") == "succeeded",
        "effect.deferred" => true,
        _ => false,
    }
}

impl OtlpSink {
    pub fn new(sink: LogSink, cap: usize, debug: bool) -> OtlpSink {
        OtlpSink { queue: sink.queue(cap), debug }
    }

    /// Each offload spill record also pushes its running total as a metric, from the writer thread.
    /// Returns once every record taken so far is written.
    pub fn close(self) {
        self.queue.close();
    }

    pub fn with_metrics(sink: LogSink, cap: usize, metrics: MetricsSink, debug: bool) -> OtlpSink {
        OtlpSink { queue: sink.queue_then(cap, move |body, attrs| push_metrics(&metrics, body, attrs)), debug }
    }
}

fn push_metrics(metrics: &MetricsSink, body: &str, attrs: &[(String, Attr)]) {
    let find = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    if body == SWEEP_FOUND_UNIT {
        if let (Some(Attr::Str(task)), Some(Attr::Int(total))) = (find("task"), find("wake.sweep_found_total")) {
            metrics.push(&[Point::Gauge { name: SWEEP_FOUND_TOTAL, value: *total as f64 }], &[("task", task)]);
        }
        return;
    }
    if body != SPILL_UNIT {
        return;
    }
    if let (Some(Attr::Str(what)), Some(Attr::Int(total))) = (find("what"), find("offload.spilled_total")) {
        metrics.push(&[Point::Gauge { name: SPILLED_TOTAL, value: *total as f64 }], &[("what", what)]);
    }
}

fn attr(v: &Value) -> Attr {
    match v {
        Value::Str(s) => Attr::Str(s.clone()),
        Value::Int(i) => Attr::Int(*i),
        Value::Float(f) => Attr::Float(*f),
        Value::Bool(b) => Attr::Bool(*b),
        Value::Strs(v) => Attr::Strs(v.clone()),
        Value::Ints(v) => Attr::Ints(v.clone()),
        Value::Floats(v) => Attr::Floats(v.clone()),
        Value::Bools(v) => Attr::Bools(v.clone()),
    }
}

impl WideSink for OtlpSink {
    fn emit(&self, fields: &Fields) {
        if !self.debug && is_routine(fields) {
            return;
        }
        let unit = match fields.get("unit") {
            Some(Value::Str(u)) => u.as_str(),
            _ => "unit",
        };
        let attrs: Vec<(String, Attr)> = fields.iter().map(|(k, v)| (k.clone(), attr(v))).collect();
        self.queue.emit(unit, &attrs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pairs: &[(&str, &str)]) -> Fields {
        pairs.iter().map(|(k, v)| (k.to_string(), Value::Str(v.to_string()))).collect()
    }

    #[test_id(txvl7rl)]
    #[test]
    fn successful_turns_are_routine_and_failures_and_anomalies_are_not() {
        for routine in [
            rec(&[("unit", "dispatch"), ("outcome", "ok")]),
            rec(&[("unit", "effect"), ("effect.outcome", "succeeded")]),
            rec(&[("unit", "effect.deferred")]),
        ] {
            assert!(is_routine(&routine), "{routine:?}");
        }
        for kept in [
            rec(&[("unit", "dispatch"), ("outcome", "error")]),
            rec(&[("unit", "dispatch"), ("outcome", "panic")]),
            rec(&[("unit", "loop.turn"), ("outcome", "ok")]),
            rec(&[("unit", "loop.turn"), ("outcome", "overrun")]),
            rec(&[("unit", "effect"), ("effect.outcome", "failed")]),
            rec(&[("unit", "effect"), ("effect.outcome", "timed_out")]),
            rec(&[("unit", "quarantine"), ("outcome", "quarantined")]),
            rec(&[("unit", "offload.spill")]),
            rec(&[("unit", "watchdog.restart"), ("outcome", "restarted")]),
        ] {
            assert!(!is_routine(&kept), "{kept:?}");
        }
    }

    #[test_id(tl9y8i6)]
    #[test]
    fn routine_records_reach_the_file_only_in_debug() {
        for (debug, want) in [(false, 0), (true, 1)] {
            let d = testkit::TempDir::new("sink-routine");
            let s = OtlpSink::new(LogSink::new(d.path(), "svc"), 8, debug);
            s.emit(&rec(&[("unit", "effect.deferred")]));
            s.close();
            assert_eq!(spira_otel::logs::read_all(d.path(), "svc").len(), want, "debug={debug}");
        }
        let d = testkit::TempDir::new("sink-anomaly");
        let s = OtlpSink::new(LogSink::new(d.path(), "svc"), 8, false);
        s.emit(&rec(&[("unit", "dispatch"), ("outcome", "error")]));
        s.close();
        assert_eq!(spira_otel::logs::read_all(d.path(), "svc").len(), 1, "positive control: a failure is written");
    }

    #[test_id(t8ysd7s)]
    #[test]
    fn a_sweep_found_record_pushes_its_total_as_a_gauge_labelled_by_task() {
        let rec = |unit: &str| vec![("task".to_string(), Attr::Str("a".into())), ("wake.sweep_found_total".to_string(), Attr::Int(3)), ("unit".to_string(), Attr::Str(unit.into()))];
        let (_, got) = spira_otel::metrics::fixture::capture(|m| {
            push_metrics(m, "dispatch", &rec("dispatch"));
            push_metrics(m, SWEEP_FOUND_UNIT, &rec(SWEEP_FOUND_UNIT));
        });
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!((got[0].name.as_str(), got[0].value, got[0].label("task")), (SWEEP_FOUND_TOTAL, 3.0, Some("a")));
    }

    #[test_id(tno3cdu)]
    #[test]
    fn the_dashboard_has_the_scheduler_idle_cost_row_and_queries_the_pushed_series() {
        let board: serde_json::Value = serde_json::from_str(include_str!("../../spira/metrics/grafana/dashboards/bead-store.json")).unwrap();
        let panels = board["panels"].as_array().unwrap();
        assert!(panels.iter().any(|p| p["type"] == "row" && p["title"].as_str().is_some_and(|t| t.contains("Scheduler idle cost"))));
        let exprs: Vec<String> = panels.iter().filter(|p| p["type"] != "row").flat_map(|p| p["targets"].as_array().into_iter().flatten().map(|t| t["expr"].as_str().unwrap_or_default().to_string())).collect();
        assert_eq!(exprs.iter().filter(|e| e.contains(SWEEP_FOUND_TOTAL)).count(), 1);
        assert_eq!(exprs.iter().filter(|e| e.contains("spira_lc_requests") && e.contains(r#"caller="spira-runtime""#) && e.contains(r#"verb!="watch""#)).count(), 2);
        let ids: Vec<u64> = panels.iter().map(|p| p["id"].as_u64().unwrap()).collect();
        assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), ids.len(), "panel ids are unique");
    }

    #[test_id(thiyj3d)]
    #[test]
    fn a_sweep_found_record_reaches_the_log_file_outside_debug() {
        let d = testkit::TempDir::new("sink-sweep-found");
        let s = OtlpSink::new(LogSink::new(d.path(), "svc"), 8, false);
        s.emit(&rec(&[("unit", SWEEP_FOUND_UNIT), ("task", "a")]));
        s.close();
        assert_eq!(spira_otel::logs::read_all(d.path(), "svc").len(), 1);
    }
}
