use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use spira_otel::metrics::{MetricsSink, Point};

pub const COMPONENT: &str = "sccache-dav";
pub const FLUSH_EVERY: Duration = Duration::from_secs(15);

type Key = (&'static str, &'static str);

/// Request tallies, flushed on a timer so a build's thousands of requests cost one push, not one each.
#[derive(Default)]
pub struct Tally(Mutex<BTreeMap<Key, Vec<f64>>>);

pub fn operation(method: &str) -> &'static str {
    match method {
        "PROPFIND" => "propfind",
        "MKCOL" => "mkcol",
        "GET" => "get",
        "HEAD" => "head",
        "PUT" => "put",
        "DELETE" => "delete",
        _ => "other",
    }
}

/// A GET that finds nothing is a cache `miss`, one that finds an entry a `hit`.
pub fn outcome(method: &str, status: u16) -> &'static str {
    match (method, status) {
        ("GET", 200) => "hit",
        ("GET", 404) => "miss",
        (_, 401) => "denied",
        (_, 500..=599) => "error",
        (_, 400..=499) if method != "HEAD" && method != "DELETE" => "error",
        _ => "ok",
    }
}

impl Tally {
    pub fn record(&self, method: &str, status: u16, took: Duration) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.entry((operation(method), outcome(method, status))).or_default().push(took.as_secs_f64());
    }

    pub fn flush(&self, sink: &MetricsSink) {
        let drained = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        for ((op, out), seconds) in drained {
            let mean = seconds.iter().sum::<f64>() / seconds.len() as f64;
            sink.push(
                &[Point::Count { name: "spira_component_invocations", value: seconds.len() as u64 }, Point::Seconds { name: "spira_component_duration_seconds", value: mean }],
                &[("component", COMPONENT), ("operation", op), ("outcome", out)],
            );
        }
    }
}

pub fn sink() -> MetricsSink {
    let endpoint = spira_config::process::cfg("SPIRA_METRICS_ENDPOINT").ok();
    MetricsSink::new(endpoint.as_deref(), "spira-sccache-dav", spira_otel::metrics::run::DEADLINE)
}

/// Eviction sweep: files and bytes removed, so the cache's churn is visible beside its hit rate.
pub fn push_eviction(sink: &MetricsSink, files: u64, bytes: u64) {
    sink.push(&[Point::Count { name: "spira_sccache_dav_evicted_files", value: files }, Point::Count { name: "spira_sccache_dav_evicted_bytes", value: bytes }], &[("component", COMPONENT)]);
}
