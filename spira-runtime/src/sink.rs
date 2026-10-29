//! Wide events as OTLP log records through the collector's file receiver. Nothing here names a field.

use runtime::{Fields, Value, WideSink};
use spira_otel::logs::{Attr, LogQueue, LogSink};
use spira_otel::metrics::{MetricsSink, Point};

/// A gauge of a monotonic total: the store drops sums, so the alarm reads the latest value.
pub const SPILLED_TOTAL: &str = "spira_runtime_offload_spilled_total";
const SPILL_UNIT: &str = "offload.spill";

pub struct OtlpSink(LogQueue);

impl OtlpSink {
    pub fn new(sink: LogSink, cap: usize) -> OtlpSink {
        OtlpSink(sink.queue(cap))
    }

    /// Each offload spill record also pushes its running total as a metric, from the writer thread.
    pub fn with_metrics(sink: LogSink, cap: usize, metrics: MetricsSink) -> OtlpSink {
        OtlpSink(sink.queue_then(cap, move |body, attrs| push_spill(&metrics, body, attrs)))
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
        let unit = match fields.get("unit") {
            Some(Value::Str(u)) => u.as_str(),
            _ => "unit",
        };
        let attrs: Vec<(String, Attr)> = fields.iter().map(|(k, v)| (k.clone(), attr(v))).collect();
        self.0.emit(unit, &attrs);
    }
}
