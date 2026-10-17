//! Fire-and-forget OTLP metrics, each point a timestamped Gauge sample (the store drops delta sums and histograms), pushed straight to a Prometheus-compatible store (VictoriaMetrics
//! `/opentelemetry/v1/metrics`), protobuf over plain HTTP. Every phase has a deadline; a failed
//! push is counted in `push_failed()`, never returned, and never touches the producer's durable record.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value::Value, AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::metrics::v1::{
    metric::Data, number_data_point::Value as Num, Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

static PUSH_FAILED: AtomicU64 = AtomicU64::new(0);

pub const PATH: &str = "/opentelemetry/v1/metrics";

/// Pushes pending metrics lost to a connect, write, read or status error since this process started.
pub fn push_failed() -> u64 {
    PUSH_FAILED.load(Ordering::Relaxed)
}

pub enum Point<'a> {
    /// One event sample of `value` (sum it over a window to count).
    Count { name: &'a str, value: u64 },
    /// One duration sample in seconds.
    Seconds { name: &'a str, value: f64 },
    /// A gauge: the latest value of `name`.
    Gauge { name: &'a str, value: f64 },
}

pub struct MetricsSink {
    endpoint: Option<String>,
    service: String,
    deadline: Duration,
}

impl MetricsSink {
    /// `endpoint` is `http://host:port` (or `host:port`); empty or `None` disables the sink.
    pub fn new(endpoint: Option<&str>, service: &str, deadline: Duration) -> MetricsSink {
        let endpoint = endpoint.map(str::trim).filter(|e| !e.is_empty()).map(|e| e.trim_start_matches("http://").trim_end_matches('/').to_string());
        MetricsSink { endpoint, service: service.to_string(), deadline }
    }

    /// A sink for one component's own runs: `component` is the OTLP service name, the deadline short.
    pub fn for_component(endpoint: Option<&str>, component: &str) -> MetricsSink {
        MetricsSink::new(endpoint, component, Duration::from_secs(2))
    }

    /// Pushes `points`, all carrying `labels`; a failure is counted, never returned.
    pub fn push(&self, points: &[Point], labels: &[(&str, &str)]) {
        if self.try_push(points, labels).is_err() {
            PUSH_FAILED.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn try_push(&self, points: &[Point], labels: &[(&str, &str)]) -> std::io::Result<()> {
        let Some(addr) = &self.endpoint else { return Ok(()) };
        post(addr, &self.encode(points, labels, SystemTime::now()).encode_to_vec(), self.deadline)
    }

    pub fn encode(&self, points: &[Point], labels: &[(&str, &str)], now: SystemTime) -> ExportMetricsServiceRequest {
        let t = now.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as u64;
        let attributes: Vec<KeyValue> = labels.iter().map(|(k, v)| kv(k, v)).collect();
        let metrics = points
            .iter()
            .map(|p| {
                let (name, unit, value) = match p {
                    Point::Count { name, value } => (*name, "", Num::AsInt(*value as i64)),
                    Point::Seconds { name, value } => (*name, "s", Num::AsDouble(*value)),
                    Point::Gauge { name, value } => (*name, "", Num::AsDouble(*value)),
                };
                Metric {
                    name: name.into(),
                    unit: unit.into(),
                    data: Some(Data::Gauge(Gauge { data_points: vec![NumberDataPoint { attributes: attributes.clone(), time_unix_nano: t, value: Some(value), ..Default::default() }] })),
                    ..Default::default()
                }
            })
            .collect();
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource { attributes: vec![kv("service.name", &self.service)], ..Default::default() }),
                scope_metrics: vec![ScopeMetrics { scope: Some(InstrumentationScope { name: "spira-otel".into(), ..Default::default() }), metrics, ..Default::default() }],
                ..Default::default()
            }],
        }
    }
}

pub const INVOCATIONS: &str = "spira_component_invocations";
pub const RUN_SECONDS: &str = "spira_component_run_seconds";

/// Runs one unit of work and pushes its invocation count and duration, labelled by
/// `component`, `operation` and `outcome` (`ok`, or `exit_<code>` for a non-zero exit code).
/// `f` returns the exit code; the push is fire-and-forget and never changes it.
pub fn instrument(sink: &MetricsSink, component: &str, operation: &str, f: impl FnOnce() -> i32) -> i32 {
    instrument_with(sink, component, operation, &[], || (f(), Vec::new()))
}

/// [`instrument`] for a `Result`: the outcome is `ok` or `error`.
pub fn instrument_result<T>(sink: &MetricsSink, component: &str, operation: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let mut out = None;
    instrument(sink, component, operation, || {
        let r = f();
        let code = i32::from(r.is_err());
        out = Some(r);
        code
    });
    out.expect("instrument ran f")
}

/// [`instrument`] where `f` also returns extra points describing the work it did, pushed with the same labels.
pub fn instrument_with(sink: &MetricsSink, component: &str, operation: &str, extra_labels: &[(&str, &str)], f: impl FnOnce() -> (i32, Vec<(String, f64)>)) -> i32 {
    let start = Instant::now();
    let (code, counts) = f();
    let outcome = if code == 0 { "ok".to_string() } else { format!("exit_{code}") };
    let mut points = vec![Point::Count { name: INVOCATIONS, value: 1 }, Point::Seconds { name: RUN_SECONDS, value: start.elapsed().as_secs_f64() }];
    points.extend(counts.iter().map(|(name, value)| Point::Gauge { name, value: *value }));
    let mut labels = vec![("component", component), ("operation", operation), ("outcome", outcome.as_str())];
    labels.extend_from_slice(extra_labels);
    sink.push(&points, &labels);
    code
}

fn kv(k: &str, v: &str) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::StringValue(v.into())) }), ..Default::default() }
}

fn post(addr: &str, body: &[u8], deadline: Duration) -> std::io::Result<()> {
    let end = Instant::now() + deadline;
    let left = || end.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::TimedOut, "deadline"));
    let target = addr.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::other("no address"))?;
    let mut s = TcpStream::connect_timeout(&target, left()?)?;
    s.set_write_timeout(Some(left()?))?;
    let head = format!("POST {PATH} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.set_read_timeout(Some(left()?))?;
    let mut status = [0u8; 12];
    s.read_exact(&mut status)?;
    match &status[9..12] {
        b"200" | b"202" | b"204" => Ok(()),
        other => Err(std::io::Error::other(format!("status {}", String::from_utf8_lossy(other)))),
    }
}

pub const RUN_DEADLINE: Duration = Duration::from_secs(1);

/// One run or unit of work of a component: `end` pushes `spira_component_invocations`,
/// `spira_component_duration_seconds` and any extra points, labelled by component, operation and
/// outcome. Labels stay bounded — an operation is a subcommand or route, never an id.
pub struct Run {
    sink: MetricsSink,
    component: String,
    operation: String,
    start: Instant,
}

impl Run {
    pub fn begin(endpoint: Option<&str>, component: &str, operation: &str) -> Run {
        Run { sink: MetricsSink::new(endpoint, &format!("spira-{component}"), RUN_DEADLINE), component: component.into(), operation: operation.into(), start: Instant::now() }
    }

    pub fn end(self, outcome: &str) {
        self.end_with(outcome, &[]);
    }

    pub fn end_with(self, outcome: &str, extra: &[Point]) {
        let mut points = vec![Point::Count { name: "spira_component_invocations", value: 1 }, Point::Seconds { name: "spira_component_duration_seconds", value: self.start.elapsed().as_secs_f64() }];
        points.extend(extra.iter().map(|p| match p {
            Point::Count { name, value } => Point::Count { name, value: *value },
            Point::Seconds { name, value } => Point::Seconds { name, value: *value },
        }));
        self.sink.push(&points, &[("component", &self.component), ("operation", &self.operation), ("outcome", outcome)]);
    }
}

/// `ok` for exit 0, `error` otherwise.
pub fn outcome_of(code: i32) -> &'static str {
    if code == 0 { "ok" } else { "error" }
}

/// [`outcome_of`] for a `std::process::ExitCode`, which exposes no accessor or equality: read from its `Debug` form.
pub fn outcome_of_exit(code: std::process::ExitCode) -> &'static str {
    if format!("{code:?}").contains("(0)") { "ok" } else { "error" }
}

/// A sink for tests: a local HTTP listener that records every metric pushed to it.
pub mod fixture {
    use super::*;
    use std::collections::BTreeMap;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, PartialEq)]
    pub struct Sample {
        pub name: String,
        pub value: f64,
        pub labels: BTreeMap<String, String>,
    }

    /// `bin` under `env -i`, with only the config document in force and the harness home it names.
    /// The caller declares `SPIRA_METRICS_ENDPOINT` in the document (`spira_config::process::fixture_toml`).
    pub fn bare(bin: &std::path::Path, toml: &std::path::Path, home: &std::path::Path) -> std::process::Command {
        let mut c = std::process::Command::new("env");
        c.arg("-i").arg("PATH=/usr/bin:/bin").arg(format!("SPIRA_HOME={}", home.display())).arg(format!("SPIRA_TOML={}", toml.display())).arg(bin);
        c
    }

    pub struct Capture {
        pub addr: String,
        seen: Arc<Mutex<Vec<Sample>>>,
    }

    impl Capture {
        pub fn start() -> Capture {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = l.local_addr().unwrap().to_string();
            let seen: Arc<Mutex<Vec<Sample>>> = Arc::default();
            let out = seen.clone();
            std::thread::spawn(move || {
                for c in l.incoming() {
                    let Ok(mut c) = c else { break };
                    let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let body = loop {
                        let Ok(n) = c.read(&mut chunk) else { break None };
                        if n == 0 {
                            break None;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        let len: usize = head.split("content-length: ").nth(1).and_then(|r| r.split("\r\n").next()).and_then(|v| v.parse().ok()).unwrap_or(0);
                        if buf.len() >= i + 4 + len {
                            break Some(buf[i + 4..i + 4 + len].to_vec());
                        }
                    };
                    let _ = c.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
                    let Some(body) = body else { continue };
                    let Ok(req) = ExportMetricsServiceRequest::decode(body.as_slice()) else { continue };
                    let mut v = out.lock().unwrap();
                    for rm in req.resource_metrics {
                        for sm in rm.scope_metrics {
                            for m in sm.metrics {
                                let Some(Data::Gauge(g)) = m.data else { continue };
                                for dp in g.data_points {
                                    let value = match dp.value {
                                        Some(Num::AsInt(i)) => i as f64,
                                        Some(Num::AsDouble(d)) => d,
                                        None => continue,
                                    };
                                    let labels = dp.attributes.iter().filter_map(|a| match a.value.as_ref()?.value.as_ref()? {
                                        Value::StringValue(s) => Some((a.key.clone(), s.clone())),
                                        _ => None,
                                    });
                                    v.push(Sample { name: m.name.clone(), value, labels: labels.collect() });
                                }
                            }
                        }
                    }
                }
            });
            Capture { addr, seen }
        }

        /// Samples received so far, waiting up to two seconds for at least `n`.
        pub fn samples(&self, n: usize) -> Vec<Sample> {
            let end = Instant::now() + Duration::from_secs(2);
            while self.seen.lock().unwrap().len() < n && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
            self.seen.lock().unwrap().clone()
        }

        /// A run of `component`/`operation` was pushed, whatever its outcome.
        pub fn assert_ran(&self, component: &str, operation: &str) {
            let all = self.samples(2);
            for name in ["spira_component_invocations", "spira_component_duration_seconds"] {
                assert!(
                    all.iter().any(|s| s.name == name && s.labels.get("component").map(String::as_str) == Some(component) && s.labels.get("operation").map(String::as_str) == Some(operation) && s.labels.contains_key("outcome")),
                    "{name}{{component={component},operation={operation}}} not pushed; got {all:?}"
                );
            }
        }

        /// The standard run metrics for `component`/`operation`/`outcome` were pushed.
        pub fn assert_run(&self, component: &str, operation: &str, outcome: &str) {
            let all = self.samples(2);
            for name in ["spira_component_invocations", "spira_component_duration_seconds"] {
                assert!(
                    all.iter().any(|s| s.name == name && s.labels.get("component").map(String::as_str) == Some(component) && s.labels.get("operation").map(String::as_str) == Some(operation) && s.labels.get("outcome").map(String::as_str) == Some(outcome)),
                    "{name}{{component={component},operation={operation},outcome={outcome}}} not pushed; got {all:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn sink(addr: &str) -> MetricsSink {
        MetricsSink::new(Some(addr), "spira-tsd", Duration::from_millis(300))
    }

    fn serve(reply: &'static str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = c.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
                let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                let len: usize = head.split("content-length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
                if buf.len() >= i + 4 + len {
                    break;
                }
            }
            c.write_all(reply.as_bytes()).unwrap();
            buf
        });
        (addr, h)
    }

    const LABELS: [(&str, &str); 2] = [("repo", "r"), ("instance_type", "c7a.4xlarge")];

    #[test]
    fn every_point_encodes_as_a_timestamped_gauge_with_its_labels() {
        let s = sink("127.0.0.1:1");
        let req = s.encode(&[Point::Seconds { name: "cert_pass_boot_seconds", value: 20.0 }, Point::Count { name: "cert_pass_verdicts", value: 1 }], &LABELS, UNIX_EPOCH + Duration::from_secs(5));
        let back = ExportMetricsServiceRequest::decode(req.encode_to_vec().as_slice()).unwrap();
        let m = &back.resource_metrics[0].scope_metrics[0].metrics;
        let Some(Data::Gauge(g)) = &m[0].data else { panic!("gauge") };
        let p = &g.data_points[0];
        assert_eq!(p.value, Some(Num::AsDouble(20.0)));
        assert_eq!(p.time_unix_nano, 5_000_000_000);
        assert_eq!(p.attributes[1].key, "instance_type");
        let Some(Data::Gauge(c)) = &m[1].data else { panic!("gauge") };
        assert_eq!(c.data_points[0].value, Some(Num::AsInt(1)));
        assert_eq!(c.data_points[0].time_unix_nano, 5_000_000_000);
    }

    #[test]
    fn a_push_posts_protobuf_to_the_otlp_path() {
        let (addr, h) = serve("HTTP/1.1 204 No Content\r\n\r\n");
        sink(&addr).try_push(&[Point::Count { name: "c", value: 2 }], &LABELS).unwrap();
        let got = h.join().unwrap();
        let text = String::from_utf8_lossy(&got);
        assert!(text.starts_with("POST /opentelemetry/v1/metrics HTTP/1.1"), "{text}");
        assert!(text.contains("application/x-protobuf"));
        let i = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert!(ExportMetricsServiceRequest::decode(&got[i..]).is_ok());
    }

    #[test]
    fn a_refused_connection_and_a_server_error_are_counted_not_raised() {
        let before = push_failed();
        sink("127.0.0.1:1").push(&[Point::Count { name: "c", value: 1 }], &[]);
        let (addr, h) = serve("HTTP/1.1 500 Internal Server Error\r\n\r\n");
        sink(&addr).push(&[Point::Count { name: "c", value: 1 }], &[]);
        h.join().unwrap();
        assert!(push_failed() >= before + 2);
    }

    #[test]
    fn a_server_that_never_answers_is_cut_off_at_the_deadline() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let before = push_failed();
        sink(&addr).push(&[Point::Count { name: "c", value: 1 }], &[]);
        assert!(push_failed() > before);
        drop(l);
    }

    #[test]
    fn an_unset_endpoint_pushes_nothing_and_counts_nothing() {
        let before = push_failed();
        MetricsSink::new(None, "s", Duration::from_millis(50)).push(&[Point::Count { name: "c", value: 1 }], &[]);
        MetricsSink::new(Some("  "), "s", Duration::from_millis(50)).push(&[Point::Count { name: "c", value: 1 }], &[]);
        assert_eq!(push_failed(), before);
    }

    #[test]
    fn a_run_pushes_invocation_duration_and_extras_labelled_by_component_operation_outcome() {
        let cap = fixture::Capture::start();
        Run::begin(Some(&cap.addr), "mail", "send").end_with("error", &[Point::Count { name: "spira_mail_sent", value: 3 }]);
        cap.assert_run("mail", "send", "error");
        let all = cap.samples(3);
        assert!(all.iter().any(|s| s.name == "spira_mail_sent" && s.value == 3.0));
        assert_eq!(outcome_of(0), "ok");
        assert_eq!(outcome_of(2), "error");
        assert_eq!(outcome_of_exit(std::process::ExitCode::SUCCESS), "ok");
        assert_eq!(outcome_of_exit(std::process::ExitCode::FAILURE), "error");
        assert_eq!(outcome_of_exit(std::process::ExitCode::from(10)), "error");
    }

    #[test]
    fn a_run_with_no_endpoint_pushes_nothing() {
        let before = push_failed();
        Run::begin(None, "mail", "send").end("ok");
        assert_eq!(push_failed(), before);
    }
}

#[cfg(test)]
mod instrument_tests {
    use super::*;

    #[test]
    fn a_run_pushes_its_invocation_duration_and_outcome_and_keeps_its_exit_code() {
        let fx = crate::fixture::Sink::start();
        let sink = MetricsSink::for_component(Some(fx.addr()), "spira-x");
        assert_eq!(instrument(&sink, "x", "pass", || 3), 3);
        assert_eq!(instrument_result(&sink, "x", "other", || Ok::<_, String>(7)), Ok(7));
        std::thread::sleep(Duration::from_millis(200));
        let got = fx.received(Duration::from_secs(2));
        let fail = got.iter().find(|s| s.name == INVOCATIONS && s.label("operation") == Some("pass")).unwrap();
        assert_eq!((fail.service.as_str(), fail.label("component"), fail.label("outcome")), ("spira-x", Some("x"), Some("exit_3")));
        assert!(got.iter().any(|s| s.name == RUN_SECONDS && s.label("operation") == Some("pass")));
        assert!(got.iter().any(|s| s.name == INVOCATIONS && s.label("operation") == Some("other") && s.label("outcome") == Some("ok")));
    }

    #[test]
    fn a_gauge_point_round_trips() {
        let req = MetricsSink::new(None, "s", Duration::from_millis(50)).encode(&[Point::Gauge { name: "g", value: 2.5 }], &[], UNIX_EPOCH);
        let Some(Data::Gauge(g)) = &req.resource_metrics[0].scope_metrics[0].metrics[0].data else { panic!() };
        assert_eq!(g.data_points[0].value, Some(Num::AsDouble(2.5)));
    }
}
