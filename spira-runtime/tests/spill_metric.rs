//! A spill wide event also lands as a metric, pushed from the telemetry writer thread.

use std::time::Duration;

use runtime::{Fields, WideSink};
use spira_otel::fixture::Sink;
use spira_otel::logs::LogSink;
use spira_otel::metrics::MetricsSink;
use spira_runtime::sink::{OtlpSink, SPILLED_TOTAL};

fn record(unit: &str, what: &str, total: i64) -> Fields {
    let mut f = Fields::new();
    f.insert("unit".into(), unit.into());
    f.insert("what".into(), what.into());
    f.insert("offload.spilled_total".into(), total.into());
    f
}

#[test]
fn a_spill_record_pushes_its_total_labelled_by_what_and_other_records_push_nothing() {
    let d = testkit::TempDir::new("spill-metric");
    let store = Sink::start();
    let metrics = MetricsSink::for_component(Some(store.addr()), "spira-runtime");
    let sink = OtlpSink::with_metrics(LogSink::new(d.path(), "spira-runtime"), 8, metrics);
    sink.emit(&record("loop.turn", "turn", 99));
    sink.emit(&record("offload.spill", "turn", 3));
    let seen = store.received(Duration::from_secs(5));
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!((seen[0].name.as_str(), seen[0].value, seen[0].label("what"), seen[0].service.as_str()), (SPILLED_TOTAL, 3.0, Some("turn"), "spira-runtime"));
}

#[test]
fn an_unset_endpoint_pushes_nothing_and_the_record_is_still_logged() {
    let d = testkit::TempDir::new("spill-metric-off");
    let sink = OtlpSink::with_metrics(LogSink::new(d.path(), "spira-runtime"), 8, MetricsSink::for_component(None, "spira-runtime"));
    sink.emit(&record("offload.spill", "turn", 1));
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while spira_otel::logs::read_all(d.path(), "spira-runtime").is_empty() && std::time::Instant::now() < end { // clock-safe: cap on the wait for the writer thread
        std::thread::sleep(Duration::from_millis(10)); // clock-safe: polls the file the writer thread fills
    }
    assert_eq!(spira_otel::logs::read_all(d.path(), "spira-runtime").len(), 1);
}
