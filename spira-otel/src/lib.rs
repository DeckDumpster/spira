//! One OTLP span per call, pushed to a Collector over OTLP/HTTP twice: as a trace span, for
//! the Collector's request metrics, and as a log record, the wide event its logs pipeline
//! delivers. `emit` only queues: one thread pushes each second's queue. A span the queue
//! cannot take or a push cannot deliver is counted and dropped, never retried.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod cgroup;
pub mod fixture;
pub mod logs;
pub mod metrics;
pub mod oneshot;
pub mod ops;
pub mod query;
pub mod rounds;
pub mod run;
pub mod testresult;
pub mod vlogs;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{any_value::Value, AnyValue, ArrayValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{span::SpanKind, status::StatusCode, ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;

static DROPPED: AtomicU64 = AtomicU64::new(0);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

const TRACES_PATH: &str = "/v1/traces"; // not a path: OTLP traces URL path
const LOGS_PATH: &str = "/v1/logs"; // not a path: OTLP logs URL path

/// Queues one service's spans and pushes them to a Collector.
pub struct Sink {
    inner: Arc<Inner>,
    queue: Arc<Queue>,
}

struct Inner {
    addr: String,
    service: String,
    deadline: Duration,
    dropped: AtomicU64,
}

/// Spans waiting for the writer thread; `bytes` is their encoded size.
#[derive(Default)]
struct Pending {
    spans: Vec<Span>,
    bytes: usize,
    busy: bool,
    flushers: usize,
}

#[derive(Default)]
struct Queue {
    pending: Mutex<Pending>,
    wake: Condvar,
    done: Condvar,
}

/// The most encoded span bytes a sink holds before it drops.
pub const QUEUE_MAX_BYTES: usize = 8 << 20;
const APPEND_EVERY: Duration = Duration::from_secs(1);
const FLUSH_BOUND: Duration = Duration::from_secs(5);
/// Connect, write and status read of one POST.
const PUSH_DEADLINE: Duration = Duration::from_secs(2);

/// What one call looked like; the attribute set of the span built from it.
pub struct Call<'a> {
    pub method: &'a str,
    pub caller: &'a str,
    pub request_size: u64,
    pub response_size: u64,
    pub exit_code: i32,
    pub start: SystemTime,
    pub end: SystemTime,
    pub queue_wait: Duration,
    /// Request received to the worker being handed it.
    pub before_work: Duration,
    /// Handing the worker the request to its answer, or to the cutoff.
    pub answer_wait: Duration,
    /// The answer in hand to the end of the span: reply, settling, counters.
    pub after_answer: Duration,
    /// The answer was the deadline answer.
    pub cut: bool,
    /// Request read to answer written; `None` where no request was read.
    pub answered: Option<Duration>,
    /// The request cutoff that abandoned the call; `None` where it answered in time.
    pub cut_at: Option<Duration>,
    /// Microseconds per store phase the request recorded.
    pub phases: &'a [(&'a str, u64)],
    /// The duration of each store commit, in order.
    pub commits_us: &'a [u64],
    /// SQLite read transactions.
    pub reads: u64,
    pub read_max_us: u64,
    pub reads_ended: u64,
    pub reads_overran: u64,
    /// The store phase a request cut at its deadline was still in.
    pub open_phase: Option<&'a str>,
    /// Dolt SQL statements run.
    pub content_queries: u64,
    /// The longest single Dolt statement, in microseconds, and its shape.
    pub content_query_max_us: u64,
    pub content_query_max_shape: &'a str,
    /// The scheduler task whose effect made the call.
    pub task: Option<&'a str>,
    /// Child processes run.
    pub children: u64,
    /// Basenames of the child programs, in order.
    pub programs: &'a [String],
    /// The request's sub-verb, when it is a plain word.
    pub subverb: Option<&'a str>,
    /// The tasks whose rows a scheduler turn carries, comma-joined.
    pub turn_tasks: Option<&'a str>,
}

/// Spans lost to a full queue or a failed push since this process started.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

impl Sink {
    /// `endpoint` is the Collector's OTLP/HTTP receiver, `host:port`.
    pub fn buffered(endpoint: &str, service: &str) -> Sink {
        Self::with_deadline(endpoint, service, PUSH_DEADLINE)
    }

    fn with_deadline(endpoint: &str, service: &str, deadline: Duration) -> Sink {
        let inner = Arc::new(Inner { addr: vlogs::host_port(endpoint), service: service.to_string(), deadline, dropped: AtomicU64::new(0) });
        let queue = Arc::new(Queue::default());
        let (i, q) = (inner.clone(), queue.clone());
        std::thread::Builder::new().name("otel-writer".into()).spawn(move || writer_loop(&i, &q)).expect("the span writer thread starts");
        Sink { inner, queue }
    }

    /// Spans this sink lost; `dropped()` counts the process.
    pub fn dropped_here(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    /// Queues one span; a span the queue cannot take is counted, never returned.
    pub fn emit(&self, call: &Call) {
        let span = span_of(call);
        let len = span.encoded_len();
        let mut p = self.queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        if p.bytes + len > QUEUE_MAX_BYTES {
            drop(p);
            return self.inner.drop_spans(1);
        }
        p.bytes += len;
        p.spans.push(span);
        self.queue.wake.notify_one();
    }

    /// Blocks until everything queued so far has been pushed or dropped, or `FLUSH_BOUND`
    /// passes: a Collector that never answers must not hold a stopping process.
    pub fn flush(&self) {
        let mut p = self.queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.flushers += 1;
        self.queue.wake.notify_one();
        let (mut p, _) = self.queue.done.wait_timeout_while(p, FLUSH_BOUND, |p| !p.spans.is_empty() || p.busy).unwrap_or_else(|e| e.into_inner());
        p.flushers -= 1;
    }
}

fn writer_loop(inner: &Inner, queue: &Queue) {
    let mut p = queue.pending.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        while p.spans.is_empty() {
            p = queue.wake.wait(p).unwrap_or_else(|e| e.into_inner());
        }
        let batch = std::mem::take(&mut p.spans);
        p.bytes = 0;
        p.busy = true;
        drop(p);
        inner.push(batch);
        p = queue.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.busy = false;
        queue.done.notify_all();
        p = queue.wake.wait_timeout_while(p, APPEND_EVERY, |p| p.flushers == 0).unwrap_or_else(|e| e.into_inner()).0;
    }
}

impl Inner {
    fn drop_spans(&self, n: u64) {
        DROPPED.fetch_add(n, Ordering::Relaxed);
        self.dropped.fetch_add(n, Ordering::Relaxed);
    }

    /// Both posts are tried; a batch that misses either is lost, and counted once.
    fn push(&self, spans: Vec<Span>) {
        let n = spans.len() as u64;
        let logs = wide_events(&self.service, &spans).encode_to_vec();
        let traces = traces(&self.service, spans).encode_to_vec();
        let sent = [(TRACES_PATH, traces), (LOGS_PATH, logs)].map(|(path, body)| metrics::post(&self.addr, path, &body, self.deadline).is_ok());
        if sent.contains(&false) {
            self.drop_spans(n);
        }
    }
}

fn traces(service: &str, spans: Vec<Span>) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource { attributes: vec![kv_str("service.name", service)], ..Default::default() }),
            scope_spans: vec![ScopeSpans { scope: Some(InstrumentationScope { name: "spira-otel".into(), ..Default::default() }), spans, ..Default::default() }],
            ..Default::default()
        }],
    }
}

/// One log record per span: every span attribute, its duration and ids, timed at its start.
fn wide_events(service: &str, spans: &[Span]) -> ExportLogsServiceRequest {
    let records = spans
        .iter()
        .map(|s| {
            let mut attributes = s.attributes.clone();
            attributes.push(kv_int("spira.duration_us", s.end_time_unix_nano.saturating_sub(s.start_time_unix_nano) / 1000));
            let mut record = logs::record(&s.name, attributes, s.start_time_unix_nano);
            record.trace_id = s.trace_id.clone();
            record.span_id = s.span_id.clone();
            record
        })
        .collect();
    logs::export(service, records)
}

/// The trace request carrying one call's span.
pub fn span_request(service: &str, call: &Call) -> ExportTraceServiceRequest {
    traces(service, vec![span_of(call)])
}

fn span_of(c: &Call) -> Span {
    let nanos = |t: SystemTime| t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as u64;
    let failed = c.exit_code != 0;
    let mut attributes = vec![
        kv_str("rpc.system", "spira-lc"),
        kv_str("rpc.method", c.method),
        kv_str("spira.caller", c.caller),
        kv_int("rpc.request.size", c.request_size),
        kv_int("rpc.response.size", c.response_size),
        kv_int("spira.queue_wait_us", c.queue_wait.as_micros() as u64),
        kv_int("spira.serve.before_work_us", c.before_work.as_micros() as u64),
        kv_int("spira.serve.answer_wait_us", c.answer_wait.as_micros() as u64),
        kv_int("spira.serve.after_answer_us", c.after_answer.as_micros() as u64),
        KeyValue { key: "spira.serve.cut".into(), value: Some(AnyValue { value: Some(Value::BoolValue(c.cut)) }) },
    ];
    if let Some(answered) = c.answered {
        attributes.push(kv_int("spira.answer_us", answered.as_micros() as u64));
    }
    if let Some(cut) = c.cut_at {
        attributes.push(kv_int("spira.deadline_cut_ms", cut.as_millis() as u64));
    }
    for (name, us) in c.phases {
        attributes.push(kv_int(&format!("spira.phase.{name}_us"), *us));
    }
    if !c.commits_us.is_empty() {
        attributes.push(kv_int("spira.store.commits", c.commits_us.len() as u64));
        let values = c.commits_us.iter().map(|us| AnyValue { value: Some(Value::IntValue(*us as i64)) }).collect();
        attributes.push(KeyValue { key: "spira.phase.commits_us".into(), value: Some(AnyValue { value: Some(Value::ArrayValue(ArrayValue { values })) }) });
    }
    if c.reads > 0 {
        attributes.push(kv_int("spira.store.reads", c.reads));
        attributes.push(kv_int("spira.store.read_max_us", c.read_max_us));
        attributes.push(kv_int("spira.store.reads_ended", c.reads_ended));
        attributes.push(kv_int("spira.store.reads_overran", c.reads_overran));
    }
    if c.content_queries > 0 {
        attributes.push(kv_int("spira.content.queries", c.content_queries));
        attributes.push(kv_int("spira.content.query_max_us", c.content_query_max_us));
        attributes.push(kv_str("spira.content.query_max_shape", c.content_query_max_shape));
    }
    if let Some(task) = c.task {
        attributes.push(kv_str("spira.task", task));
    }
    if c.children > 0 {
        attributes.push(kv_int("spira.children", c.children));
    }
    if !c.programs.is_empty() {
        attributes.push(kv_str("spira.children.programs", &c.programs.join(",")));
    }
    if let Some(sub) = c.subverb {
        attributes.push(kv_str("spira.subverb", sub));
    }
    if let Some(tasks) = c.turn_tasks {
        attributes.push(kv_str("spira.turn.tasks", tasks));
    }
    if let Some(open) = c.open_phase {
        attributes.push(kv_str("spira.phase.open", open));
    }
    if failed {
        attributes.push(KeyValue { key: "spira.exit_code".into(), value: Some(AnyValue { value: Some(Value::IntValue(i64::from(c.exit_code))) }) });
    }
    Span {
        trace_id: ids(16),
        span_id: ids(8),
        name: c.method.to_string(),
        kind: SpanKind::Server as i32,
        start_time_unix_nano: nanos(c.start),
        end_time_unix_nano: nanos(c.end),
        attributes,
        status: Some(Status { code: if failed { StatusCode::Error } else { StatusCode::Ok } as i32, message: String::new() }),
        ..Default::default()
    }
}

pub(crate) fn kv_str(k: &str, v: &str) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::StringValue(v.into())) }) }
}

fn kv_int(k: &str, v: u64) -> KeyValue {
    KeyValue { key: k.into(), value: Some(AnyValue { value: Some(Value::IntValue(v as i64)) }) }
}

/// `n` unique non-zero bytes: time, pid and a counter, mixed — not random, but never repeated.
fn ids(n: usize) -> Vec<u8> {
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_nanos() as u64;
    let mut x = t ^ seq.rotate_left(32) ^ (u64::from(std::process::id()) << 16);
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.extend_from_slice(&(z ^ (z >> 31) | 1).to_be_bytes());
    }
    out.truncate(n);
    out[n - 1] |= 1;
    out
}

/// `YYYY-MM-DD` of a UTC instant.
pub fn utc_date(t: SystemTime) -> String {
    let days = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO).as_secs() / 86_400;
    let (y, m, d) = civil(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn dir_of(run: &Path) -> PathBuf {
    run.join("otel")
}

#[cfg(test)]
mod test_yaml {
    /// The indented keys of a YAML file as dotted paths, each with its inline value.
    pub(crate) fn paths(yaml: &str) -> Vec<(String, String)> {
        let mut stack: Vec<(usize, String)> = Vec::new();
        let mut out = Vec::new();
        for line in yaml.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') || t.starts_with('-') {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            let Some((key, value)) = t.split_once(':') else { continue };
            stack.retain(|(i, _)| *i < indent);
            stack.push((indent, key.to_string()));
            out.push((stack.iter().map(|(_, k)| k.as_str()).collect::<Vec<_>>().join("."), value.trim().to_string()));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Collector;
    use std::net::TcpListener;

    /// A cap on a wait for the fixture's own signal.
    const CAP: Duration = Duration::from_secs(30);

    fn call(code: i32) -> Call<'static> {
        let start = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        Call { method: "list", caller: "work/list", request_size: 40, response_size: 439_002, exit_code: code, start, end: start + Duration::from_millis(12), queue_wait: Duration::ZERO, before_work: Duration::ZERO, answer_wait: Duration::ZERO, after_answer: Duration::ZERO, cut: false, answered: None, cut_at: None, phases: &[], commits_us: &[], reads: 0, read_max_us: 0, reads_ended: 0, reads_overran: 0, open_phase: None, content_queries: 0, content_query_max_us: 0, content_query_max_shape: "", task: None, children: 0, programs: &[], subverb: None, turn_tasks: None }
    }

    fn encoded(c: &Call) -> Span {
        span_request("spira-lc", c).resource_spans[0].scope_spans[0].spans[0].clone()
    }

    fn attr_of<'a>(attrs: &'a [KeyValue], k: &str) -> Option<&'a Value> {
        attrs.iter().find(|a| a.key == k).and_then(|a| a.value.as_ref()?.value.as_ref())
    }

    fn attr<'a>(span: &'a Span, k: &str) -> Option<&'a Value> {
        attr_of(&span.attributes, k)
    }

    fn text(s: &str) -> Option<Value> {
        Some(Value::StringValue(s.into()))
    }

    #[test_id(txpa66z)]
    #[test]
    fn answer_time_and_cutoff_are_int_attributes_only_when_given() {
        let given = encoded(&Call { answered: Some(Duration::from_millis(12)), cut_at: Some(Duration::from_millis(1000)), ..call(0) });
        let none = encoded(&call(0));
        assert_eq!(attr(&given, "spira.answer_us"), Some(&Value::IntValue(12_000)));
        assert_eq!(attr(&given, "spira.deadline_cut_ms"), Some(&Value::IntValue(1000)));
        assert_eq!(attr(&none, "spira.answer_us"), None);
        assert_eq!(attr(&none, "spira.deadline_cut_ms"), None);
    }

    #[test_id(tal4n2j)]
    #[test]
    fn the_subverb_children_and_queries_are_written_only_when_present() {
        let programs = ["bd".to_string(), "git".to_string()];
        let given = encoded(&Call { subverb: Some("note"), children: 2, programs: &programs, content_queries: 3, ..call(0) });
        let none = encoded(&call(0));
        assert_eq!(attr(&given, "spira.subverb"), text("note").as_ref());
        assert_eq!(attr(&given, "spira.children.programs"), text("bd,git").as_ref());
        assert_eq!(attr(&given, "spira.children"), Some(&Value::IntValue(2)));
        assert_eq!(attr(&given, "spira.content.queries"), Some(&Value::IntValue(3)));
        for k in ["spira.subverb", "spira.children", "spira.children.programs", "spira.content.queries"] {
            assert_eq!(attr(&none, k), None, "{k}");
        }
    }

    #[test_id(toh8axn)]
    #[test]
    fn the_longest_statement_and_the_task_are_written_only_when_present() {
        let given = encoded(&Call { content_queries: 2, content_query_max_us: 938_132, content_query_max_shape: "SELECT a FROM t WHERE x = ?", task: Some("sentinel"), ..call(0) });
        let none = encoded(&Call { content_query_max_us: 5, content_query_max_shape: "SELECT 1", ..call(0) });
        assert_eq!(attr(&given, "spira.content.query_max_us"), Some(&Value::IntValue(938_132)));
        assert_eq!(attr(&given, "spira.content.query_max_shape"), text("SELECT a FROM t WHERE x = ?").as_ref());
        assert_eq!(attr(&given, "spira.task"), text("sentinel").as_ref());
        for k in ["spira.content.query_max_us", "spira.content.query_max_shape", "spira.task"] {
            assert_eq!(attr(&none, k), None, "{k}");
        }
    }

    #[test_id(tovzjfu)]
    #[test]
    fn the_serve_stages_and_the_cut_are_written_each_under_its_own_name() {
        let span = encoded(&Call { before_work: Duration::from_micros(11), answer_wait: Duration::from_micros(22), after_answer: Duration::from_micros(33), cut: true, ..call(0) });
        assert_eq!(attr(&span, "spira.serve.before_work_us"), Some(&Value::IntValue(11)));
        assert_eq!(attr(&span, "spira.serve.answer_wait_us"), Some(&Value::IntValue(22)));
        assert_eq!(attr(&span, "spira.serve.after_answer_us"), Some(&Value::IntValue(33)));
        assert_eq!(attr(&span, "spira.serve.cut"), Some(&Value::BoolValue(true)));
    }

    #[test_id(tdemrvd)]
    #[test]
    fn a_span_names_its_service_ids_times_and_status() {
        let ok = span_request("spira-lc", &call(0));
        assert_eq!(attr_of(&ok.resource_spans[0].resource.as_ref().unwrap().attributes, "service.name"), text("spira-lc").as_ref());
        let s = &ok.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!((s.trace_id.len(), s.span_id.len(), s.kind), (16, 8, SpanKind::Server as i32));
        assert_eq!(s.end_time_unix_nano - s.start_time_unix_nano, 12_000_000);
        assert_eq!(attr(s, "rpc.system"), text("spira-lc").as_ref());
        assert_eq!(attr(s, "rpc.method"), text("list").as_ref());
        assert_eq!(attr(s, "spira.caller"), text("work/list").as_ref());
        assert_eq!(attr(s, "rpc.request.size"), Some(&Value::IntValue(40)));
        assert_eq!(attr(s, "rpc.response.size"), Some(&Value::IntValue(439_002)));
        assert_eq!(attr(s, "spira.exit_code"), None);
        assert_eq!(s.status.as_ref().unwrap().code, StatusCode::Ok as i32);
        let bad = encoded(&call(3));
        assert_eq!(bad.status.as_ref().unwrap().code, StatusCode::Error as i32);
        assert_eq!(attr(&bad, "spira.exit_code"), Some(&Value::IntValue(3)));
    }

    #[test_id(t7icq07)]
    #[test]
    fn emitted_calls_arrive_as_one_trace_post_and_one_log_post_per_batch_with_the_whole_span_in_each_record() {
        let collector = Collector::hanging();
        let sink = Sink::with_deadline(collector.addr(), "spira-lc", Duration::from_secs(3600));
        let answered = || Call { answered: Some(Duration::from_millis(7)), ..call(0) };
        sink.emit(&answered());
        assert!(collector.wait_until(|p| !p.is_empty(), CAP), "the first push reached the collector");
        for _ in 0..3 {
            sink.emit(&answered());
        }
        collector.release();
        sink.flush();
        let posts = collector.posts();
        assert_eq!(posts.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), [TRACES_PATH, LOGS_PATH, TRACES_PATH, LOGS_PATH]);
        assert!(posts.iter().all(|p| p.content_type == "application/x-protobuf"), "{posts:?}");
        assert_eq!(crate::fixture::spans_in(&posts[2..]).len(), 3, "the calls queued during a push leave as one batch");
        let (spans, records) = (collector.spans(), collector.log_records());
        assert_eq!((spans.len(), records.len()), (4, 4));
        for (span, (service, record)) in spans.iter().zip(&records) {
            assert_eq!(service, "spira-lc");
            assert_eq!(record.body.as_ref().and_then(|b| b.value.clone()), text("list"));
            assert_eq!((&record.trace_id, &record.span_id), (&span.trace_id, &span.span_id));
            assert_eq!(record.trace_id.len(), 16);
            assert_eq!(record.time_unix_nano, span.start_time_unix_nano);
            for a in &span.attributes {
                assert!(record.attributes.contains(a), "{} is on the record", a.key);
            }
            assert_eq!(attr_of(&record.attributes, "rpc.method"), text("list").as_ref());
            assert_eq!(attr_of(&record.attributes, "spira.caller"), text("work/list").as_ref());
            assert_eq!(attr_of(&record.attributes, "spira.answer_us"), Some(&Value::IntValue(7_000)));
            assert_eq!(attr_of(&record.attributes, "spira.duration_us"), Some(&Value::IntValue(12_000)));
        }
        assert_eq!(sink.dropped_here(), 0);
    }

    #[test_id(tn0w4e8)]
    #[test]
    fn emit_returns_while_the_collector_holds_a_push_and_spans_past_the_queue_cap_are_counted() {
        let collector = Collector::hanging();
        let sink = Sink::with_deadline(collector.addr(), "spira-lc", Duration::from_secs(3600));
        sink.emit(&call(0));
        assert!(collector.wait_until(|p| !p.is_empty(), CAP), "the first push reached the collector");
        let shape = "x".repeat(16 << 10);
        for _ in 0..1000 {
            sink.emit(&Call { content_queries: 1, content_query_max_shape: &shape, ..call(0) });
        }
        assert_eq!(collector.posts().len(), 1, "the first request is still unanswered");
        let held = sink.queue.pending.lock().unwrap().bytes;
        assert!(held <= QUEUE_MAX_BYTES && held > QUEUE_MAX_BYTES - (17 << 10), "{held}");
        assert_eq!(sink.dropped_here(), 1000 - sink.queue.pending.lock().unwrap().spans.len() as u64);
        assert!(sink.dropped_here() > 0);
    }

    #[test_id(tc7gfjm)]
    #[test]
    fn a_push_nobody_answers_in_time_counts_its_spans() {
        let collector = Collector::hanging();
        let sink = Sink::with_deadline(collector.addr(), "spira-lc", Duration::from_millis(50));
        sink.emit(&call(0));
        sink.flush();
        assert_eq!(sink.dropped_here(), 1);
    }

    #[test_id(tezr034)]
    #[test]
    fn with_nothing_listening_every_emitted_span_is_counted() {
        let gone = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = gone.local_addr().unwrap().to_string();
        drop(gone);
        let before = dropped();
        let sink = Sink::buffered(&addr, "spira-lc");
        for _ in 0..5 {
            sink.emit(&call(0));
        }
        sink.flush();
        assert_eq!(sink.dropped_here(), 5);
        assert!(dropped() >= before + 5);
    }

    #[test_id(tbh79ot)]
    #[test]
    fn the_collector_takes_spans_over_otlp_only_and_forwards_logs_to_victorialogs() {
        let paths = test_yaml::paths(include_str!("../../spira/metrics/collector.yaml"));
        let at = |p: &str| paths.iter().find(|(k, _)| k == p).map(|(_, v)| v.as_str()).unwrap_or_else(|| panic!("{p} is not in the collector config"));
        let list = |p: &str| at(p).trim_matches(['[', ']']).split(',').map(|s| s.trim().to_string()).collect::<Vec<_>>();
        assert_eq!(list("service.pipelines.traces.receivers"), ["otlp"]);
        assert_eq!(list("service.pipelines.traces.exporters"), ["spanmetrics"]);
        assert!(list("service.pipelines.logs.receivers").contains(&"otlp".to_string()));
        assert_eq!(list("service.pipelines.logs.exporters"), ["otlphttp/victorialogs"]);
        assert_eq!(list("service.pipelines.metrics.receivers"), ["spanmetrics"]);
        assert_eq!(list("service.pipelines.metrics.exporters"), ["prometheusremotewrite"]);
        let tailing: Vec<&str> = paths.iter().filter_map(|(k, _)| k.strip_prefix("receivers.")?.strip_suffix(".include")).collect();
        assert_eq!(tailing, ["otlpjsonfile/logs"], "no other receiver tails files");
    }

    #[test_id(tftbb0d)]
    #[test]
    fn a_utc_date_is_the_calendar_day_of_the_instant() {
        for (secs, day) in [(0u64, "1970-01-01"), (86_400 * 59, "1970-03-01"), (1_700_006_399, "2023-11-14"), (1_700_006_400, "2023-11-15"), (4_102_444_800, "2100-01-01")] {
            assert_eq!(utc_date(UNIX_EPOCH + Duration::from_secs(secs)), day);
        }
    }
}

/// A process exit status a `main` body can return and the telemetry wrapper can read back,
/// which `std::process::ExitCode` does not allow.
pub mod exit {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ExitCode(u8);

    impl ExitCode {
        pub const SUCCESS: ExitCode = ExitCode(0);
        pub const FAILURE: ExitCode = ExitCode(1);

        pub fn code(self) -> i32 {
            i32::from(self.0)
        }
    }

    impl From<u8> for ExitCode {
        fn from(c: u8) -> ExitCode {
            ExitCode(c)
        }
    }

    pub fn finish(code: i32) -> std::process::ExitCode {
        std::process::ExitCode::from(code.clamp(0, 255) as u8)
    }
}
