//! What a testenv run pushes to the metrics store: suites run, reds by class, slot queue wait
//! and over-cap kills. Every push is fire-and-forget through [`spira_otel::metrics`].

use spira_otel::metrics::{MetricsSink, Point};
use std::sync::Mutex;

static QUEUE_WAITS: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());

pub fn sink() -> MetricsSink {
    spira_config::process::metrics_sink("spira-testenv")
}

/// Records seconds spent waiting for an admission slot of `pool`; pushed with the run's finish.
pub fn note_queue_wait(pool: &'static str, secs: u64) {
    if secs > 0 {
        QUEUE_WAITS.lock().unwrap_or_else(|e| e.into_inner()).push((pool, secs));
    }
}

/// `real` for a red suite, `harness` for a fault (`reason` names it), `none` for a green.
pub fn red_class(rc: i32) -> &'static str {
    match rc {
        0 => "none",
        1 => "real",
        _ => "harness",
    }
}

pub fn push_finish(sink: &MetricsSink, rc: i32, ran: usize, red: usize, skipped: usize, reason: Option<&str>) {
    let waits = std::mem::take(&mut *QUEUE_WAITS.lock().unwrap_or_else(|e| e.into_inner()));
    push_run(sink, rc, ran, red, skipped, reason, waits);
}

fn push_run(sink: &MetricsSink, rc: i32, ran: usize, red: usize, skipped: usize, reason: Option<&str>, waits: Vec<(&'static str, u64)>) {
    let base = [("component", "testenv")];
    sink.push(
        &[
            Point::Count { name: "spira_testenv_suites_run", value: ran as u64 },
            Point::Count { name: "spira_testenv_suites_skipped", value: skipped as u64 },
        ],
        &base,
    );
    if rc != 0 {
        let class = red_class(rc);
        let value = if rc == 1 { red as u64 } else { 1 };
        sink.push(&[Point::Count { name: "spira_testenv_reds", value }], &[("component", "testenv"), ("class", class), ("reason", reason.unwrap_or("-"))]);
    }
    for (pool, secs) in waits {
        sink.push(&[Point::Seconds { name: "spira_testenv_queue_wait_seconds", value: secs as f64 }], &[("component", "testenv"), ("pool", pool)]);
    }
}

/// An over-cap kill: `tier` is `suite` or `gate`, or `self` for a run that cancelled itself.
pub fn push_over_cap(sink: &MetricsSink, tier: &str) {
    sink.push(&[Point::Count { name: "spira_testenv_over_cap_kills", value: 1 }], &[("component", "testenv"), ("tier", tier)]);
}
