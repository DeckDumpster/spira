//! Wide events as OTLP log records through the collector's file receiver. Nothing here names a field.

use runtime::{Fields, Value, WideSink};
use spira_otel::logs::{Attr, LogQueue, LogSink};
use spira_otel::metrics::{MetricsSink, Point};

/// A gauge of a monotonic total: the store drops sums, so the alarm reads the latest value.
pub const SPILLED_TOTAL: &str = "spira_runtime_offload_spilled_total";
const SPILL_UNIT: &str = "offload.spill";

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
        "dispatch" | "loop.turn" => text("outcome") == "ok",
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
        OtlpSink { queue: sink.queue_then(cap, move |body, attrs| push_spill(&metrics, body, attrs)), debug }
    }
}

fn push_spill(metrics: &MetricsSink, body: &str, attrs: &[(String, Attr)]) {
    if body != SPILL_UNIT {
        return;
    }
    let find = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v);
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

    #[test]
    fn successful_turns_are_routine_and_failures_and_anomalies_are_not() {
        for routine in [
            rec(&[("unit", "dispatch"), ("outcome", "ok")]),
            rec(&[("unit", "loop.turn"), ("outcome", "ok")]),
            rec(&[("unit", "effect"), ("effect.outcome", "succeeded")]),
            rec(&[("unit", "effect.deferred")]),
        ] {
            assert!(is_routine(&routine), "{routine:?}");
        }
        for kept in [
            rec(&[("unit", "dispatch"), ("outcome", "error")]),
            rec(&[("unit", "dispatch"), ("outcome", "panic")]),
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
}
