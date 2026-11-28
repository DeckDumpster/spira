//! Fixture OTLP receivers for tests: they accept pushes on a local port and decode them.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::logs::v1::LogRecord;
use opentelemetry_proto::tonic::trace::v1::Span;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::metrics::v1::{metric::Data, number_data_point::Value as Num};
use prost::Message;

/// One data point as received.
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub service: String,
    pub name: String,
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

impl Seen {
    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

pub struct Sink {
    addr: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
}

impl Sink {
    pub fn start() -> Sink {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let (seen, stop) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
        let (s2, st2) = (seen.clone(), stop.clone());
        std::thread::spawn(move || {
            while !st2.load(Ordering::Relaxed) {
                let Ok((mut c, _)) = l.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                c.set_nonblocking(false).unwrap();
                c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                if let Some(body) = read_request(&mut c) {
                    if let Ok(req) = ExportMetricsServiceRequest::decode(body.as_slice()) {
                        s2.lock().unwrap().extend(flatten(&req));
                    }
                }
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        Sink { addr, seen, stop }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Runs `exe args` in a minimal environment whose config (`toml`) points the metrics endpoint
    /// at this sink, then returns what was pushed. `extra_env` is added to that environment.
    pub fn run(&self, exe: &str, args: &[&str], toml: &std::path::Path, home: &str, extra_env: &[(&str, &str)]) -> Vec<Seen> {
        // batch-job: runs the binary under test to completion
        std::process::Command::new(exe)
            .args(args)
            .env_clear()
            .env("SPIRA_TOML", toml)
            .env("SPIRA_HOME", home)
            .env("PATH", system_paths::SYSTEM_PATH_MIN)
            .envs(extra_env.iter().copied())
            .output()
            .unwrap();
        self.received(Duration::from_secs(3))
    }

    /// Everything received so far, waiting up to `wait` for at least one point.
    pub fn received(&self, wait: Duration) -> Vec<Seen> {
        let end = std::time::Instant::now() + wait;
        while self.seen.lock().unwrap().is_empty() && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn read_request(c: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = c.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
        let len: usize = head.split("content-length: ").nth(1)?.split("\r\n").next()?.parse().ok()?;
        if buf.len() >= i + 4 + len {
            return Some(buf[i + 4..i + 4 + len].to_vec());
        }
    }
}

fn flatten(req: &ExportMetricsServiceRequest) -> Vec<Seen> {
    let mut out = Vec::new();
    for rm in &req.resource_metrics {
        let service = rm
            .resource
            .iter()
            .flat_map(|r| &r.attributes)
            .find(|a| a.key == "service.name")
            .and_then(|a| a.value.as_ref())
            .and_then(|v| if let Some(Value::StringValue(s)) = &v.value { Some(s.clone()) } else { None })
            .unwrap_or_default();
        for m in rm.scope_metrics.iter().flat_map(|s| &s.metrics) {
            let attrs = |a: &[opentelemetry_proto::tonic::common::v1::KeyValue]| {
                a.iter().filter_map(|kv| Some((kv.key.clone(), if let Some(Value::StringValue(s)) = &kv.value.as_ref()?.value { s.clone() } else { String::new() }))).collect::<Vec<_>>()
            };
            let num = |p: &opentelemetry_proto::tonic::metrics::v1::NumberDataPoint| match p.value {
                Some(Num::AsInt(i)) => i as f64,
                Some(Num::AsDouble(d)) => d,
                None => 0.0,
            };
            match &m.data {
                Some(Data::Sum(s)) => out.extend(s.data_points.iter().map(|p| Seen { service: service.clone(), name: m.name.clone(), labels: attrs(&p.attributes), value: num(p) })),
                Some(Data::Gauge(g)) => out.extend(g.data_points.iter().map(|p| Seen { service: service.clone(), name: m.name.clone(), labels: attrs(&p.attributes), value: num(p) })),
                Some(Data::Histogram(h)) => out.extend(h.data_points.iter().map(|p| Seen { service: service.clone(), name: m.name.clone(), labels: attrs(&p.attributes), value: p.sum.unwrap_or(0.0) })),
                _ => {}
            }
        }
    }
    out
}

/// A stand-in for the metrics store in tests: takes OTLP pushes on `/opentelemetry/v1/metrics`
/// and answers `/api/v1/export` from what it holds, on one port as the real store does. A sample
/// keeps its own timestamp, and a second sample with the same name, labels and millisecond
/// replaces the first, as the store's own ingestion does.
pub struct FakeStore {
    addr: String,
    samples: Arc<Mutex<Vec<Stored>>>,
    exports: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
struct Stored {
    labels: std::collections::BTreeMap<String, String>,
    ms: i64,
    value: f64,
}

impl FakeStore {
    pub fn start() -> FakeStore {
        FakeStore::start_on("127.0.0.1:0")
    }

    pub fn start_on(bind: &str) -> FakeStore {
        let l = TcpListener::bind(bind).unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let (samples, stop) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
        let exports = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s2, st2, e2) = (samples.clone(), stop.clone(), exports.clone());
        std::thread::spawn(move || {
            while !st2.load(Ordering::Relaxed) {
                let Ok((mut c, _)) = l.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                c.set_nonblocking(false).unwrap();
                c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let reply = serve_one(&mut c, &s2, &e2);
                let _ = c.write_all(&reply);
            }
        });
        FakeStore { addr, samples, exports, stop }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// How many samples it holds.
    pub fn len(&self) -> usize {
        self.samples.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many export (read) requests it has answered.
    pub fn export_requests(&self) -> usize {
        self.exports.load(Ordering::SeqCst)
    }
}

impl Drop for FakeStore {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn read_http(c: &mut std::net::TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let n = c.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&buf[..i]).into_owned();
        let len: usize = head.to_lowercase().split("content-length: ").nth(1).and_then(|s| s.split("\r\n").next()).and_then(|s| s.parse().ok()).unwrap_or(0);
        if buf.len() >= i + 4 + len {
            return Some((head, buf[i + 4..i + 4 + len].to_vec()));
        }
    }
}

fn serve_one(c: &mut std::net::TcpStream, samples: &Mutex<Vec<Stored>>, exports: &std::sync::atomic::AtomicUsize) -> Vec<u8> {
    let Some((head, body)) = read_http(c) else { return b"HTTP/1.0 400 Bad Request\r\n\r\n".to_vec() };
    let line = head.lines().next().unwrap_or("");
    if line.starts_with("POST ") {
        let Ok(req) = ExportMetricsServiceRequest::decode(body.as_slice()) else { return b"HTTP/1.0 400 Bad Request\r\n\r\n".to_vec() };
        let mut held = samples.lock().unwrap();
        for s in flatten_timed(&req) {
            match held.iter_mut().find(|h| h.ms == s.ms && h.labels == s.labels) {
                Some(h) => h.value = s.value,
                None => held.push(s),
            }
        }
        return b"HTTP/1.0 204 No Content\r\n\r\n".to_vec();
    }
    let target = line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/api/v1/export" { // not a path: VictoriaMetrics export route
        return b"HTTP/1.0 404 Not Found\r\n\r\n".to_vec();
    }
    exports.fetch_add(1, Ordering::SeqCst);
    let mut matchers = Vec::new();
    let mut start = 0i64;
    let mut end = i64::MAX / 1000;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "match[]" => matchers.push(parse_selector(&unescape(v))),
            "start" => start = v.parse().unwrap_or(0),
            "end" => end = v.parse().unwrap_or(end),
            _ => {}
        }
    }
    let held = samples.lock().unwrap();
    let mut series: std::collections::BTreeMap<std::collections::BTreeMap<String, String>, Vec<(i64, f64)>> = Default::default();
    for s in held.iter().filter(|s| s.ms >= start * 1000 && s.ms <= end * 1000 && matchers.iter().any(|m| m.iter().all(|c| c.accepts(&s.labels)))) {
        series.entry(s.labels.clone()).or_default().push((s.ms, s.value));
    }
    let mut out = String::new();
    for (labels, mut pts) in series {
        pts.sort_by_key(|p| p.0);
        let metric: serde_json::Map<String, serde_json::Value> = labels.into_iter().map(|(k, v)| (k, serde_json::Value::String(v))).collect();
        out.push_str(&serde_json::json!({"metric": metric, "values": pts.iter().map(|p| p.1).collect::<Vec<_>>(), "timestamps": pts.iter().map(|p| p.0).collect::<Vec<_>>()}).to_string());
        out.push('\n');
    }
    format!("HTTP/1.0 200 OK\r\nContent-Type: application/stream+json\r\n\r\n{out}").into_bytes()
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Cond {
    label: String,
    regex: bool,
    want: String,
}

impl Cond {
    fn accepts(&self, labels: &std::collections::BTreeMap<String, String>) -> bool {
        let got = labels.get(&self.label).map(String::as_str).unwrap_or("");
        if self.regex {
            self.want.split('|').any(|alt| alt == got)
        } else {
            self.want == got
        }
    }
}

/// `name{a="x",b=~"p|q"}` or `{a="x"}`: a leading metric name, equality and alternation of
/// literals, the selectors the readers send.
fn parse_selector(s: &str) -> Vec<Cond> {
    let s = s.trim();
    let (name, labels) = s.split_once('{').unwrap_or(("", s));
    let inner = labels.trim_end_matches('}');
    let named = (!name.trim().is_empty()).then(|| Cond { label: "__name__".into(), regex: false, want: name.trim().to_string() });
    named
        .into_iter()
        .chain(inner.split(',').filter_map(|part| {
            let (name, rest) = part.split_once('=')?;
            let (regex, value) = match rest.strip_prefix('~') {
                Some(v) => (true, v),
                None => (false, rest),
            };
            Some(Cond { label: name.trim().to_string(), regex, want: value.trim().trim_matches('"').to_string() })
        }))
        .collect()
}

fn flatten_timed(req: &ExportMetricsServiceRequest) -> Vec<Stored> {
    let mut out = Vec::new();
    for rm in &req.resource_metrics {
        let service = rm
            .resource
            .iter()
            .flat_map(|r| &r.attributes)
            .find(|a| a.key == "service.name")
            .and_then(|a| a.value.as_ref())
            .and_then(|v| if let Some(Value::StringValue(s)) = &v.value { Some(s.clone()) } else { None })
            .unwrap_or_default();
        for m in rm.scope_metrics.iter().flat_map(|s| &s.metrics) {
            let Some(Data::Gauge(g)) = &m.data else { continue };
            for p in &g.data_points {
                let mut labels: std::collections::BTreeMap<String, String> = p
                    .attributes
                    .iter()
                    .filter_map(|kv| Some((kv.key.clone(), if let Some(Value::StringValue(s)) = &kv.value.as_ref()?.value { s.clone() } else { String::new() })))
                    .filter(|(_, v)| !v.is_empty())
                    .collect();
                labels.insert("__name__".into(), m.name.clone());
                labels.insert("service.name".into(), service.clone());
                let value = match p.value {
                    Some(Num::AsInt(i)) => i as f64,
                    Some(Num::AsDouble(d)) => d,
                    None => 0.0,
                };
                out.push(Stored { labels, ms: (p.time_unix_nano / 1_000_000) as i64, value });
            }
        }
    }
    out
}

#[cfg(test)]
mod selector_tests {
    use super::parse_selector;

    #[test_id(tsbxq2o)]
    #[test]
    fn a_leading_metric_name_is_a_name_condition_beside_the_labels() {
        let c = parse_selector("spira_tsd_value{tsd_family=\"bead-stage\"}");
        let got: Vec<_> = c.iter().map(|c| (c.label.as_str(), c.want.as_str())).collect();
        assert_eq!(got, vec![("__name__", "spira_tsd_value"), ("tsd_family", "bead-stage")]);
        assert_eq!(parse_selector("{a=\"x\"}").len(), 1);
    }
}

/// One POST as the fixture Collector received it.
#[derive(Debug, Clone)]
pub struct Post {
    pub path: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

#[derive(Default)]
struct Held {
    posts: Vec<Post>,
    conns: Vec<std::net::TcpStream>,
    hanging: bool,
}

type OnPost = Box<dyn Fn(&Post) + Send>;

/// A stand-in for the OpenTelemetry Collector's OTLP/HTTP receiver: records every POST and
/// answers 200, or, while hanging, holds the connection unanswered until [`Collector::release`].
pub struct Collector {
    addr: String,
    held: Arc<(Mutex<Held>, std::sync::Condvar)>,
    stop: Arc<AtomicBool>,
}

impl Collector {
    pub fn start() -> Collector {
        Self::with(false, Box::new(|_| {}))
    }

    /// Accepts and reads every request and answers none.
    pub fn hanging() -> Collector {
        Self::with(true, Box::new(|_| {}))
    }

    /// As `start`, running `on_post` on each request before it is answered.
    pub fn start_then(on_post: impl Fn(&Post) + Send + 'static) -> Collector {
        Self::with(false, Box::new(on_post))
    }

    fn with(hanging: bool, on_post: OnPost) -> Collector {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let held = Arc::new((Mutex::new(Held { hanging, ..Held::default() }), std::sync::Condvar::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, st) = (held.clone(), stop.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                if st.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut c) = c else { continue };
                c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                let Some((head, body)) = read_http(&mut c) else { continue };
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let content_type = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-type: ").map(str::to_string)).unwrap_or_default();
                let post = Post { path, content_type, body };
                on_post(&post);
                let mut g = h.0.lock().unwrap();
                g.posts.push(post);
                if g.hanging {
                    g.conns.push(c);
                } else {
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
                }
                h.1.notify_all();
            }
        });
        Collector { addr, held, stop }
    }

    /// `host:port`.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Answers every held request and every later one.
    pub fn release(&self) {
        let mut g = self.held.0.lock().unwrap();
        g.hanging = false;
        for mut c in g.conns.drain(..) {
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        }
    }

    pub fn posts(&self) -> Vec<Post> {
        self.held.0.lock().unwrap().posts.clone()
    }

    /// Blocks until `done` holds of the posts received, or `cap` passes; whether it held.
    pub fn wait_until(&self, done: impl Fn(&[Post]) -> bool, cap: Duration) -> bool {
        let g = self.held.0.lock().unwrap();
        let (g, _) = self.held.1.wait_timeout_while(g, cap, |g| !done(&g.posts)).unwrap();
        done(&g.posts)
    }

    /// Every span received on `/v1/traces`, in order.
    pub fn spans(&self) -> Vec<Span> {
        spans_in(&self.posts())
    }

    /// Every span received, in the OTLP JSON shape.
    pub fn spans_json(&self) -> Vec<serde_json::Value> {
        self.spans().iter().map(|s| serde_json::to_value(s).unwrap()).collect()
    }

    /// Every log record received on `/v1/logs`, with its resource's `service.name`.
    pub fn log_records(&self) -> Vec<(String, LogRecord)> {
        self.posts()
            .iter()
            .filter(|p| p.path == "/v1/logs") // not a path: OTLP URL path
            .flat_map(|p| ExportLogsServiceRequest::decode(p.body.as_slice()).unwrap().resource_logs)
            .flat_map(|r| {
                let service = r.resource.iter().flat_map(|x| &x.attributes).find(|a| a.key == "service.name").and_then(|a| match a.value.as_ref()?.value.as_ref()? {
                    Value::StringValue(s) => Some(s.clone()),
                    _ => None,
                });
                r.scope_logs.into_iter().flat_map(|s| s.log_records).map(move |rec| (service.clone().unwrap_or_default(), rec))
            })
            .collect()
    }
}

/// The spans carried by the `/v1/traces` posts among `posts`.
pub fn spans_in(posts: &[Post]) -> Vec<Span> {
    posts
        .iter()
        .filter(|p| p.path == "/v1/traces") // not a path: OTLP URL path
        .flat_map(|p| ExportTraceServiceRequest::decode(p.body.as_slice()).unwrap().resource_spans)
        .flat_map(|r| r.scope_spans)
        .flat_map(|s| s.spans)
        .collect()
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(&self.addr);
        self.held.0.lock().unwrap().conns.clear();
    }
}
